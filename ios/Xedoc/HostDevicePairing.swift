import CryptoKit
import Foundation
import Security
import WebKit

struct PairedHost: Codable {
    let origin: String
    let serverCA: Data
    let serverCertificate: Data
    let keyTag: String
    var certificate: Data?
    let storeID: UUID
}

struct HostInvitation: Decodable {
    let version: Int
    let origin: String
    let serverCA: Data
    let serverCertificate: Data
    let token: String

    init(_ text: String) throws {
        let encoded = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard encoded.count <= 16_384 else { throw pairingError("Invitation is too large.") }
        let base64 = encoded.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        guard let data = Data(base64Encoded: base64 + String(repeating: "=", count: (4 - base64.count % 4) % 4))
        else { throw pairingError("Paste the invitation from your trusted host.") }
        self = try JSONDecoder().decode(Self.self, from: data)
        guard version == 1, let url = DiscoveredHost.validatedURL(origin), url.scheme == "https",
              HostDevicePairing.origin(url) == origin, (32...128).contains(token.count),
              SecCertificateCreateWithData(nil, serverCA as CFData) != nil,
              SecCertificateCreateWithData(nil, serverCertificate as CFData) != nil
        else { throw pairingError("Invalid host invitation.") }
    }

    var fingerprint: String {
        SHA256.hash(data: serverCertificate).map { String(format: "%02x", $0) }.joined(separator: ":")
    }
}

final class HostDevicePairing: NSObject, URLSessionTaskDelegate {
    private let trust: PairedHost
    private static let service = "org.e47.xedoc.paired-host"

    private init(trust: PairedHost) { self.trust = trust }

    static func origin(_ url: URL) -> String {
        var parts = URLComponents(url: url, resolvingAgainstBaseURL: false)!
        parts.host = parts.host?.lowercased()
        parts.path = ""
        parts.query = nil
        parts.fragment = nil
        if parts.port == 443 { parts.port = nil }
        return parts.string!
    }

    static var hosts: [DiscoveredHost] {
        (UserDefaults.standard.stringArray(forKey: "pairedHosts") ?? []).prefix(128).compactMap {
            guard let url = DiscoveredHost.validatedURL($0) else { return nil }
            return DiscoveredHost(hostname: url.host ?? $0, url: url)
        }
    }

    static func stored(for url: URL) throws -> PairedHost? {
        var query = storageQuery(origin(url))
        query[kSecReturnData as String] = true
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let data = result as? Data, data.count <= 32_768
        else { throw pairingError("Could not read this host’s pairing from Keychain.") }
        let record = try JSONDecoder().decode(PairedHost.self, from: data)
        guard record.origin == origin(url), record.certificate != nil
        else { throw pairingError("Invalid saved host pairing.") }
        return record
    }

    static func dataStore(for url: URL) -> WKWebsiteDataStore? {
        guard url.scheme == "https" else { return nil }
        guard let record = try? stored(for: url) else { return nil }
        return WKWebsiteDataStore(forIdentifier: record.storeID)
    }

    static func authenticate(_ challenge: URLAuthenticationChallenge, for url: URL,
                             completion: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void) {
        guard url.scheme == "https" else {
            completion(.performDefaultHandling, nil)
            return
        }
        do {
            guard let record = try stored(for: url) else {
                completion(.performDefaultHandling, nil)
                return
            }
            respond(challenge, trust: record, completion: completion)
        } catch {
            completion(.cancelAuthenticationChallenge, nil)
        }
    }

    static func pair(_ invitation: HostInvitation, name: String) async throws -> URL {
        guard let url = URL(string: invitation.origin), hosts.count < 128 || hosts.contains(where: { $0.id == url.absoluteString })
        else { throw pairingError("Paired host limit reached.") }
        let previous = try stored(for: url)
        let tag = "\(service).\(UUID().uuidString)"
        var attributes: [String: Any] = [
            kSecAttrIsPermanent as String: true, kSecAttrApplicationTag as String: Data(tag.utf8),
            kSecAttrLabel as String: tag, kSecAttrAccessible as String: kSecAttrAccessibleWhenUnlockedThisDeviceOnly
        ]
        #if os(macOS)
        attributes[kSecAttrIsExtractable as String] = false
        #endif
        var keyError: Unmanaged<CFError>?
        guard let key = SecKeyCreateRandomKey([
            kSecAttrKeyType as String: kSecAttrKeyTypeECSECPrimeRandom,
            kSecAttrKeySizeInBits as String: 256, kSecPrivateKeyAttrs as String: attributes
        ] as CFDictionary, &keyError), let publicKey = SecKeyCopyPublicKey(key),
              let publicData = SecKeyCopyExternalRepresentation(publicKey, &keyError) as Data?
        else { throw pairingError("Could not create a device key in Keychain.") }
        var record = PairedHost(origin: invitation.origin, serverCA: invitation.serverCA,
                                serverCertificate: invitation.serverCertificate, keyTag: tag, storeID: UUID())
        var saved = false
        defer {
            if !saved { removeIdentity(for: record) }
        }
        let deviceName = String(name.replacingOccurrences(of: "\n", with: " ").prefix(64))
        let encodedKey = publicData.base64EncodedString()
        let proof = "xedoc-device-enroll-v1\n\(record.origin)\n\(invitation.token)\n\(encodedKey)\n\(deviceName)"
        guard let signature = SecKeyCreateSignature(key, .ecdsaSignatureMessageX962SHA256,
                                                   Data(proof.utf8) as CFData, &keyError) as Data?
        else { throw pairingError("Could not sign the pairing request.") }
        let session = URLSession(configuration: .ephemeral, delegate: HostDevicePairing(trust: record), delegateQueue: nil)
        defer { session.invalidateAndCancel() }
        var request = URLRequest(url: url.appendingPathComponent("api/device/enroll"))
        request.httpMethod = "POST"
        request.timeoutInterval = 15
        request.httpShouldHandleCookies = false
        request.setValue(record.origin, forHTTPHeaderField: "Origin")
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONSerialization.data(withJSONObject: [
            "token": invitation.token, "publicKey": encodedKey,
            "signature": signature.base64EncodedString(), "name": deviceName
        ])
        let (bytes, response) = try await session.bytes(for: request)
        var body = Data()
        for try await byte in bytes {
            guard body.count < 16_384 else { throw pairingError("Pairing response is too large.") }
            body.append(byte)
        }
        guard (response as? HTTPURLResponse)?.statusCode == 200,
              let json = try JSONSerialization.jsonObject(with: body) as? [String: String],
              let encoded = json["certificate"], let certificateData = Data(base64Encoded: encoded),
              let certificate = SecCertificateCreateWithData(nil, certificateData as CFData),
              let certifiedKey = SecCertificateCopyKey(certificate),
              SecKeyCopyExternalRepresentation(certifiedKey, nil) as Data? == publicData
        else { throw pairingError("Pairing failed. Create a new invitation on the trusted host.") }
        record.certificate = certificateData
        let status = SecItemAdd([kSecClass as String: kSecClassCertificate,
                                 kSecValueRef as String: certificate, kSecAttrLabel as String: tag] as CFDictionary, nil)
        guard status == errSecSuccess else { throw pairingError("Could not save the device certificate.") }
        _ = try identity(for: record)
        let data = try JSONEncoder().encode(record)
        let query = storageQuery(record.origin)
        let update = SecItemUpdate(query as CFDictionary, [kSecValueData as String: data] as CFDictionary)
        if update == errSecItemNotFound {
            var item = query
            item[kSecValueData as String] = data
            item[kSecAttrAccessible as String] = kSecAttrAccessibleWhenUnlockedThisDeviceOnly
            guard SecItemAdd(item as CFDictionary, nil) == errSecSuccess else { throw pairingError("Could not save host trust.") }
        } else if update != errSecSuccess { throw pairingError("Could not update host trust.") }
        saved = true
        if let previous { removeIdentity(for: previous) }
        var origins = hosts.map { origin($0.url) }.filter { $0 != record.origin }
        origins.append(record.origin)
        UserDefaults.standard.set(origins, forKey: "pairedHosts")
        return url
    }

    private static func storageQuery(_ origin: String) -> [String: Any] {
        [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service,
         kSecAttrAccount as String: origin]
    }

    private static func identity(for record: PairedHost) throws -> SecIdentity {
        #if os(macOS)
        guard let data = record.certificate, let certificate = SecCertificateCreateWithData(nil, data as CFData)
        else { throw pairingError("Device certificate is unavailable. Pair again.") }
        var selected: SecIdentity?
        let status = SecIdentityCreateWithCertificate(nil, certificate, &selected)
        guard status == errSecSuccess, let identity = selected
        else { throw pairingError("Device identity is unavailable. Pair again.") }
        #else
        var result: CFTypeRef?
        let status = SecItemCopyMatching([
            kSecClass as String: kSecClassIdentity, kSecAttrLabel as String: record.keyTag,
            kSecReturnRef as String: true
        ] as CFDictionary, &result)
        guard status == errSecSuccess, let result else { throw pairingError("Device identity is unavailable. Pair again.") }
        let identity = result as! SecIdentity
        #endif
        var selectedCertificate: SecCertificate?
        guard SecIdentityCopyCertificate(identity, &selectedCertificate) == errSecSuccess,
              let selectedCertificate, SecCertificateCopyData(selectedCertificate) as Data == record.certificate
        else { throw pairingError("Device identity does not match the pairing.") }
        return identity
    }

    private static func removeIdentity(for record: PairedHost) {
        SecItemDelete([kSecClass as String: kSecClassKey,
                       kSecAttrApplicationTag as String: Data(record.keyTag.utf8)] as CFDictionary)
        guard let data = record.certificate, let certificate = SecCertificateCreateWithData(nil, data as CFData)
        else { return }
        #if os(macOS)
        SecItemDelete([kSecClass as String: kSecClassCertificate,
                       kSecMatchItemList as String: [certificate]] as CFDictionary)
        #else
        SecItemDelete([kSecClass as String: kSecClassCertificate,
                       kSecValueRef as String: certificate] as CFDictionary)
        #endif
    }

    private static func respond(_ challenge: URLAuthenticationChallenge, trust: PairedHost,
                                completion: (URLSession.AuthChallengeDisposition, URLCredential?) -> Void) {
        let space = challenge.protectionSpace
        guard let url = URL(string: trust.origin), space.host.lowercased() == url.host?.lowercased(),
              space.port == (url.port ?? 443), space.protocol == "https", challenge.previousFailureCount == 0
        else { completion(.cancelAuthenticationChallenge, nil); return }
        switch space.authenticationMethod {
        case NSURLAuthenticationMethodServerTrust:
            guard let serverTrust = space.serverTrust,
                  let chain = SecTrustCopyCertificateChain(serverTrust) as? [SecCertificate], let leaf = chain.first,
                  SecCertificateCopyData(leaf) as Data == trust.serverCertificate,
                  let authority = SecCertificateCreateWithData(nil, trust.serverCA as CFData),
                  SecTrustSetPolicies(serverTrust, SecPolicyCreateSSL(true, space.host as CFString)) == errSecSuccess,
                  SecTrustSetAnchorCertificates(serverTrust, [authority] as CFArray) == errSecSuccess,
                  SecTrustSetAnchorCertificatesOnly(serverTrust, true) == errSecSuccess,
                  SecTrustSetNetworkFetchAllowed(serverTrust, false) == errSecSuccess,
                  SecTrustEvaluateWithError(serverTrust, nil)
            else { completion(.cancelAuthenticationChallenge, nil); return }
            completion(.useCredential, URLCredential(trust: serverTrust))
        case NSURLAuthenticationMethodClientCertificate:
            guard trust.certificate != nil else { completion(.performDefaultHandling, nil); return }
            guard let identity = try? identity(for: trust) else { completion(.cancelAuthenticationChallenge, nil); return }
            completion(.useCredential, URLCredential(identity: identity, certificates: nil, persistence: .forSession))
        default:
            completion(.performDefaultHandling, nil)
        }
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didReceive challenge: URLAuthenticationChallenge,
                    completionHandler: @escaping @Sendable (URLSession.AuthChallengeDisposition, URLCredential?) -> Void) {
        Self.respond(challenge, trust: trust, completion: completionHandler)
    }

    func urlSession(_ session: URLSession, task: URLSessionTask,
                    willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest,
                    completionHandler: @escaping @Sendable (URLRequest?) -> Void) {
        completionHandler(nil)
    }
}

func pairingError(_ message: String) -> NSError {
    NSError(domain: "XedocPairing", code: 1, userInfo: [NSLocalizedDescriptionKey: message])
}
