use anyhow::{Context, Result};
use clap::Parser;
use connlib_client_shared::{file_logger, Callbacks, Session, Sockets};
use connlib_shared::{
    keypair,
    linux::{etc_resolv_conf, get_dns_control_from_env, DnsControlMethod},
    LoginUrl,
};
use firezone_cli_utils::setup_global_subscriber;
use secrecy::SecretString;
use std::{future, net::IpAddr, path::PathBuf, str::FromStr, task::Poll};
use tokio::signal::unix::SignalKind;

pub async fn run() -> Result<()> {
    let cli = super::Cli::parse();
    let max_partition_time = cli.max_partition_time.map(|d| d.into());

    let (layer, _handle) = cli.log_dir.as_deref().map(file_logger::layer).unzip();
    setup_global_subscriber(layer);

    let callbacks = CallbackHandler;

    // AKA "Device ID", not the Firezone slug
    let firezone_id = match cli.firezone_id {
        Some(id) => id,
        None => connlib_shared::device_id::get().context("Could not get `firezone_id` from CLI, could not read it from disk, could not generate it and save it to disk")?.id,
    };

    let token = match cli.token {
        Some(x) => x,
        None => {
            let path = PathBuf::from("/etc")
                .join(connlib_shared::BUNDLE_ID)
                .join("token.txt");
            let bytes = tokio::fs::read(path).await?;
            let s = String::from_utf8(bytes)?;
            s.trim().to_string()
        }
    };

    let (private_key, public_key) = keypair();
    let login = LoginUrl::client(
        cli.api_url,
        &SecretString::from(token),
        firezone_id,
        None,
        public_key.to_bytes(),
    )?;

    let session = Session::connect(
        login,
        Sockets::new(),
        private_key,
        None,
        callbacks.clone(),
        max_partition_time,
        tokio::runtime::Handle::current(),
    );
    // TODO: this should be added dynamically
    session.set_dns(system_resolvers(get_dns_control_from_env()).unwrap_or_default());

    let mut sigint = tokio::signal::unix::signal(SignalKind::interrupt())?;
    let mut sighup = tokio::signal::unix::signal(SignalKind::hangup())?;

    future::poll_fn(|cx| loop {
        if sigint.poll_recv(cx).is_ready() {
            tracing::debug!("Received SIGINT");

            return Poll::Ready(std::io::Result::Ok(()));
        }

        if sighup.poll_recv(cx).is_ready() {
            tracing::debug!("Received SIGHUP");

            session.reconnect();
            continue;
        }

        return Poll::Pending;
    })
    .await?;

    session.disconnect();

    Ok(())
}

fn system_resolvers(dns_control_method: Option<DnsControlMethod>) -> Result<Vec<IpAddr>> {
    match dns_control_method {
        None => get_system_default_resolvers_resolv_conf(),
        Some(DnsControlMethod::EtcResolvConf) => get_system_default_resolvers_resolv_conf(),
        Some(DnsControlMethod::NetworkManager) => get_system_default_resolvers_network_manager(),
        Some(DnsControlMethod::Systemd) => get_system_default_resolvers_systemd_resolved(),
    }
}

#[derive(Clone)]
struct CallbackHandler;

impl Callbacks for CallbackHandler {
    fn on_disconnect(&self, error: &connlib_client_shared::Error) {
        tracing::error!("Disconnected: {error}");

        std::process::exit(1);
    }
}

fn get_system_default_resolvers_resolv_conf() -> Result<Vec<IpAddr>> {
    // Assume that `configure_resolv_conf` has run in `tun_linux.rs`

    let s = std::fs::read_to_string(etc_resolv_conf::ETC_RESOLV_CONF_BACKUP)
        .or_else(|_| std::fs::read_to_string(etc_resolv_conf::ETC_RESOLV_CONF))
        .context("`resolv.conf` should be readable")?;
    let parsed = resolv_conf::Config::parse(s).context("`resolv.conf` should be parsable")?;

    // Drop the scoping info for IPv6 since connlib doesn't take it
    let nameservers = parsed
        .nameservers
        .into_iter()
        .map(|addr| addr.into())
        .collect();
    Ok(nameservers)
}

#[allow(clippy::unnecessary_wraps)]
fn get_system_default_resolvers_network_manager() -> Result<Vec<IpAddr>> {
    tracing::error!("get_system_default_resolvers_network_manager not implemented yet");
    Ok(vec![])
}

/// Returns the DNS servers listed in `resolvectl dns`
fn get_system_default_resolvers_systemd_resolved() -> Result<Vec<IpAddr>> {
    // Unfortunately systemd-resolved does not have a machine-readable
    // text output for this command: <https://github.com/systemd/systemd/issues/29755>
    //
    // The officially supported way is probably to use D-Bus.
    let output = std::process::Command::new("resolvectl")
        .arg("dns")
        .output()
        .context("Failed to run `resolvectl dns` and read output")?;
    if !output.status.success() {
        anyhow::bail!("`resolvectl dns` returned non-zero exit code");
    }
    let output = String::from_utf8(output.stdout).context("`resolvectl` output was not UTF-8")?;
    Ok(parse_resolvectl_output(&output))
}

/// Parses the text output of `resolvectl dns`
///
/// Cannot fail. If the parsing code is wrong, the IP address vec will just be incomplete.
fn parse_resolvectl_output(s: &str) -> Vec<IpAddr> {
    s.lines()
        .flat_map(|line| line.split(' '))
        .filter_map(|word| IpAddr::from_str(word).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    #[test]
    fn parse_resolvectl_output() {
        let cases = [
            // WSL
            (
                r"Global: 172.24.80.1
Link 2 (eth0):
Link 3 (docker0):
Link 24 (br-fc0b71997a3c):
Link 25 (br-0c129dafb204):
Link 26 (br-e67e83b19dce):
",
                [IpAddr::from([172, 24, 80, 1])],
            ),
            // Ubuntu 20.04
            (
                r"Global:
Link 2 (enp0s3): 192.168.1.1",
                [IpAddr::from([192, 168, 1, 1])],
            ),
        ];

        for (i, (input, expected)) in cases.iter().enumerate() {
            let actual = super::parse_resolvectl_output(input);
            assert_eq!(actual, expected, "Case {i} failed");
        }
    }
}
