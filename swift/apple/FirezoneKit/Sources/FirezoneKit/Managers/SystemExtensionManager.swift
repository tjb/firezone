//
//  SystemExtensionManager.swift
//  (c) 2024 Firezone, Inc.
//  LICENSE: Apache-2.0
//

#if os(macOS)
import SystemExtensions

public enum SystemExtensionError: Error {
  case unknownResult(OSSystemExtensionRequest.Result)

  var description: String {
    switch self {
    case .unknownResult(let result):
      return "Unknown result: \(result)"
    }
  }
}

public enum SystemExtensionStatus {
  // Not installed or enabled at all
  case needsInstall

  // Version of the extension is installed that differs from our bundle version.
  // "Installing" it will replace it without prompting the user.
  case needsReplacement

  // Installed and version is current with our app bundle
  case installed
}

public class SystemExtensionManager: NSObject, OSSystemExtensionRequestDelegate, ObservableObject {
  // Delegate methods complete with either a true or false outcome or an Error
  private var continuation: CheckedContinuation<SystemExtensionStatus, Error>?

  public func installSystemExtension(
    identifier: String,
    continuation: CheckedContinuation<SystemExtensionStatus, Error>
  ) {
    self.continuation = continuation

    let request = OSSystemExtensionRequest.activationRequest(forExtensionWithIdentifier: identifier, queue: .main)
    request.delegate = self

    // Install extension
    OSSystemExtensionManager.shared.submitRequest(request)
  }

  public func checkStatus(
    identifier: String,
    continuation: CheckedContinuation<SystemExtensionStatus, Error>
  ) {
    self.continuation = continuation

    let request = OSSystemExtensionRequest.propertiesRequest(
      forExtensionWithIdentifier: identifier,
      queue: .main
    )
    request.delegate = self

    // Send request
    OSSystemExtensionManager.shared.submitRequest(request)
  }

  // MARK: - OSSystemExtensionRequestDelegate

  // Result of system extension installation
  public func request(
    _ request: OSSystemExtensionRequest,
    didFinishWithResult result: OSSystemExtensionRequest.Result
  ) {
    guard result == .completed else {
      resume(throwing: SystemExtensionError.unknownResult(result))

      return
    }

    // Installation succeeded
    resume(returning: .installed)
  }

  // Result of properties request
  public func request(
    _ request: OSSystemExtensionRequest,
    foundProperties properties: [OSSystemExtensionProperties]
  ) {
    // Standard keys in any bundle. If missing, we've got bigger issues.
    guard let ourBundleVersion = Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String,
          let ourBundleShortVersion = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String
    else {
      fatalError("Version should exist in bundle")
    }

    // Up to date if version and build number match
    let isCurrentVersionInstalled = properties.contains { sysex in
      sysex.isEnabled
      && sysex.bundleVersion == ourBundleVersion
      && sysex.bundleShortVersion == ourBundleShortVersion
    }
    if isCurrentVersionInstalled {
      resume(returning: .installed)

      return
    }

    // Needs replacement if we found our extension, but its version doesn't match
    // Note this can happen for upgrades _or_ downgrades
    let isAnyVersionInstalled = properties.contains { $0.isEnabled }
    if isAnyVersionInstalled {
      resume(returning: .needsReplacement)

      return
    }

    resume(returning: .needsInstall)
  }

  public func request(_ request: OSSystemExtensionRequest, didFailWithError error: Error) {
    resume(throwing: error)
  }

  public func requestNeedsUserApproval(_ request: OSSystemExtensionRequest) {
    // We assume this state until we receive a success response.
  }

  public func request(
    _ request: OSSystemExtensionRequest,
    actionForReplacingExtension existing: OSSystemExtensionProperties,
    withExtension ext: OSSystemExtensionProperties
  ) -> OSSystemExtensionRequest.ReplacementAction {
    return .replace
  }

  private func resume(throwing error: Error) {
    self.continuation?.resume(throwing: error)
    self.continuation = nil
  }

  private func resume(returning val: SystemExtensionStatus) {
    self.continuation?.resume(returning: val)
    self.continuation = nil
  }
}
#endif
