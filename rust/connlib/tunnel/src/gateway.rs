use crate::ip_packet::{IpPacket, MutableIpPacket};
use crate::peer::{PacketTransformGateway, Peer};
use crate::peer_store::PeerStore;
use crate::utils::{earliest, stun, turn};
use crate::{GatewayEvent, GatewayTunnel};
use boringtun::x25519::PublicKey;
use chrono::{DateTime, Utc};
use connlib_shared::messages::{
    Answer, ClientId, ConnectionAccepted, DomainResponse, Interface as InterfaceConfig, Key, Offer,
    Relay, ResolvedResourceDescriptionDns, ResourceDescription, ResourceId,
};
use connlib_shared::{Callbacks, Dname, Error, Result, StaticSecret};
use ip_network::IpNetwork;
use secrecy::{ExposeSecret as _, Secret};
use snownet::ServerNode;
use std::collections::{HashSet, VecDeque};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

const PEERS_IPV4: &str = "100.64.0.0/11";
const PEERS_IPV6: &str = "fd00:2021:1111::/107";

const EXPIRE_RESOURCES_INTERVAL: Duration = Duration::from_secs(1);

impl<CB> GatewayTunnel<CB>
where
    CB: Callbacks + 'static,
{
    #[tracing::instrument(level = "trace", skip(self))]
    pub fn set_interface(&mut self, config: &InterfaceConfig) -> connlib_shared::Result<()> {
        // Note: the dns fallback strategy is irrelevant for gateways
        let callbacks = self.callbacks.clone();
        self.io
            .device_mut()
            .set_config(config, vec![], &callbacks)?;
        self.io.device_mut().set_routes(
            HashSet::from([PEERS_IPV4.parse().unwrap(), PEERS_IPV6.parse().unwrap()]),
            &callbacks,
        )?;

        let name = self.io.device_mut().name().to_owned();

        tracing::debug!(ip4 = %config.ipv4, ip6 = %config.ipv6, %name, "TUN device initialized");

        Ok(())
    }

    /// Accept a connection request from a client.
    #[allow(clippy::too_many_arguments)]
    pub fn accept(
        &mut self,
        client_id: ClientId,
        key: Secret<Key>,
        offer: Offer,
        client: PublicKey,
        ips: Vec<IpNetwork>,
        relays: Vec<Relay>,
        domain: Option<Dname>,
        expires_at: Option<DateTime<Utc>>,
        resource: ResourceDescription<ResolvedResourceDescriptionDns>,
    ) -> Result<ConnectionAccepted> {
        let (resource_addresses, id) = match &resource {
            ResourceDescription::Dns(r) => {
                let Some(domain) = domain.clone() else {
                    return Err(Error::ControlProtocolError);
                };

                if !crate::dns::is_subdomain(&domain, &r.domain) {
                    return Err(Error::InvalidResource);
                }

                (r.addresses.clone(), r.id)
            }
            ResourceDescription::Cidr(ref cidr) => (vec![cidr.address], cidr.id),
        };

        let answer = self.role_state.node.accept_connection(
            client_id,
            snownet::Offer {
                session_key: key.expose_secret().0.into(),
                credentials: snownet::Credentials {
                    username: offer.username,
                    password: offer.password,
                },
            },
            client,
            stun(&relays, |addr| self.io.sockets_ref().can_handle(addr)),
            turn(&relays, |addr| self.io.sockets_ref().can_handle(addr)),
            Instant::now(),
        );

        self.new_peer(ips, client_id, id, expires_at, resource_addresses.clone());

        Ok(ConnectionAccepted {
            ice_parameters: Answer {
                username: answer.credentials.username,
                password: answer.credentials.password,
            },
            domain_response: domain.map(|domain| DomainResponse {
                domain,
                address: resource_addresses
                    .into_iter()
                    .map(|ip| ip.network_address())
                    .collect(),
            }),
        })
    }

    pub fn cleanup_connection(&mut self, id: &ClientId) {
        self.role_state.peers.remove(id);
    }

    pub fn allow_access(
        &mut self,
        resource: ResourceDescription<ResolvedResourceDescriptionDns>,
        client: ClientId,
        expires_at: Option<DateTime<Utc>>,
        domain: Option<Dname>,
    ) -> Option<DomainResponse> {
        let peer = self.role_state.peers.get_mut(&client)?;

        let (addresses, resource_id) = match &resource {
            ResourceDescription::Dns(r) => {
                let domain = domain.clone()?;

                if !crate::dns::is_subdomain(&domain, &r.domain) {
                    return None;
                }

                (r.addresses.clone(), r.id)
            }
            ResourceDescription::Cidr(cidr) => (vec![cidr.address], cidr.id),
        };

        for address in &addresses {
            peer.transform
                .add_resource(*address, resource_id, expires_at);
        }

        tracing::info!(%client, resource = %resource_id, expires = ?expires_at.map(|e| e.to_rfc3339()), "Allowing access to resource");

        if let Some(domain) = domain {
            return Some(DomainResponse {
                domain,
                address: addresses.iter().map(|i| i.network_address()).collect(),
            });
        }

        None
    }

    #[tracing::instrument(level = "debug", skip_all, fields(%resource, %client))]
    pub fn remove_access(&mut self, client: &ClientId, resource: &ResourceId) {
        let Some(peer) = self.role_state.peers.get_mut(client) else {
            return;
        };

        peer.transform.remove_resource(resource);
        if peer.transform.is_emptied() {
            self.role_state.peers.remove(client);
        }

        tracing::debug!("Access removed");
    }

    pub fn add_ice_candidate(&mut self, conn_id: ClientId, ice_candidate: String) {
        self.role_state
            .node
            .add_remote_candidate(conn_id, ice_candidate, Instant::now());
    }

    fn new_peer(
        &mut self,
        ips: Vec<IpNetwork>,
        client_id: ClientId,
        resource: ResourceId,
        expires_at: Option<DateTime<Utc>>,
        resource_addresses: Vec<IpNetwork>,
    ) {
        let mut peer = Peer::new(client_id, PacketTransformGateway::default(), &ips, ());

        for address in resource_addresses {
            peer.transform.add_resource(address, resource, expires_at);
        }

        self.role_state.peers.insert(peer, &ips);
    }
}

pub struct GatewayState {
    pub peers: PeerStore<ClientId, PacketTransformGateway, ()>,
    node: ServerNode<ClientId>,

    next_expiry_resources_check: Option<Instant>,
    buffered_events: VecDeque<GatewayEvent>,
}

impl GatewayState {
    pub(crate) fn new(private_key: StaticSecret) -> Self {
        Self {
            peers: Default::default(),
            node: ServerNode::new(private_key),
            next_expiry_resources_check: Default::default(),
            buffered_events: VecDeque::default(),
        }
    }

    pub(crate) fn encapsulate<'s>(
        &'s mut self,
        packet: MutableIpPacket<'_>,
    ) -> Option<snownet::Transmit<'s>> {
        let dest = packet.destination();

        let peer = self.peers.peer_by_ip_mut(dest)?;
        let packet = peer.transform(packet)?;

        let transmit = self
            .node
            .encapsulate(peer.conn_id, packet.as_immutable().into(), Instant::now())
            .inspect_err(|e| tracing::debug!("Failed to encapsulate: {e}"))
            .ok()??;

        Some(transmit)
    }

    pub(crate) fn decapsulate<'b>(
        &mut self,
        local: SocketAddr,
        from: SocketAddr,
        packet: &[u8],
        now: Instant,
        buffer: &'b mut [u8],
    ) -> Option<IpPacket<'b>> {
        let (conn_id, packet) = self.node.decapsulate(
            local,
            from,
            packet,
            now,
            buffer,
        )
        .inspect_err(|e| tracing::warn!(%local, %from, num_bytes = %packet.len(), "Failed to decapsulate incoming packet: {e}"))
        .ok()??;

        let Some(peer) = self.peers.get_mut(&conn_id) else {
            tracing::error!(%conn_id, %local, %from, "Couldn't find connection");

            return None;
        };

        let packet = match peer.untransform(packet.into()) {
            Ok(packet) => packet,
            Err(e) => {
                // Note: this can happen with apps such as cURL that if started before the tunnel routes are address
                // source ips can be sticky.
                tracing::warn!(%conn_id, %local, %from, "Failed to transform packet: {e}");

                return None;
            }
        };

        Some(packet.into_immutable())
    }

    pub fn poll_timeout(&mut self) -> Option<Instant> {
        // TODO: This should check when the next resource actually expires instead of doing it at a fixed interval.
        earliest(self.next_expiry_resources_check, self.node.poll_timeout())
    }

    pub fn handle_timeout(&mut self, now: Instant) {
        self.node.handle_timeout(now);

        match self.next_expiry_resources_check {
            Some(next_expiry_resources_check) if now >= next_expiry_resources_check => {
                self.peers
                    .iter_mut()
                    .for_each(|p| p.transform.expire_resources());
                self.peers.retain(|_, p| !p.transform.is_emptied());

                self.next_expiry_resources_check = Some(now + EXPIRE_RESOURCES_INTERVAL);
            }
            None => self.next_expiry_resources_check = Some(now + EXPIRE_RESOURCES_INTERVAL),
            Some(_) => {}
        }

        while let Some(event) = self.node.poll_event() {
            match event {
                snownet::Event::ConnectionFailed(id) => {
                    self.peers.remove(&id);
                }
                snownet::Event::SignalIceCandidate {
                    connection,
                    candidate,
                } => {
                    self.buffered_events
                        .push_back(GatewayEvent::SignalIceCandidate {
                            conn_id: connection,
                            candidate,
                        });
                }
                _ => {}
            }
        }
    }

    pub(crate) fn poll_transmit(&mut self) -> Option<snownet::Transmit<'_>> {
        self.node.poll_transmit()
    }

    pub(crate) fn poll_event(&mut self) -> Option<GatewayEvent> {
        self.buffered_events.pop_front()
    }
}
