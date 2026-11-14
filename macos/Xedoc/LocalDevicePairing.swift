import Foundation

@MainActor
final class LocalDevicePairing {
    private let session = URLSession(configuration: .ephemeral)
    private var authenticatedToken: String?

    deinit { session.invalidateAndCancel() }

    func request(for host: URL, path: String, body: [String: String]?) async throws -> [String: Any]? {
        guard let local = HostSelector.localAccess() else { return nil }
        let connectedOrigin = HostDevicePairing.origin(host)
        do {
            if authenticatedToken != local.token {
                var login = URLRequest(url: local.url)
                login.timeoutInterval = 15
                login.setValue("Bearer \(local.token)", forHTTPHeaderField: "Authorization")
                let (_, response) = try await session.data(for: login)
                guard (response as? HTTPURLResponse)?.statusCode == 200 else { return nil }
                authenticatedToken = local.token
            }
            let protection = try await send(local.url, path: "device/pairing", body: nil)
            guard connectedOrigin == HostDevicePairing.origin(local.url)
                    || connectedOrigin == protection["origin"] as? String else { return nil }
            if path == "device/pairing", body == nil { return protection }
        } catch {
            if connectedOrigin == HostDevicePairing.origin(local.url) { throw error }
            return nil
        }
        return try await send(local.url, path: path, body: body)
    }

    private func send(_ origin: URL, path: String, body: [String: String]?) async throws -> [String: Any] {
        var request = URLRequest(url: origin.appendingPathComponent("api/\(path)"))
        request.timeoutInterval = 15
        if let body {
            request.httpMethod = "POST"
            request.setValue(HostDevicePairing.origin(origin), forHTTPHeaderField: "Origin")
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.httpBody = try JSONEncoder().encode(body)
        }
        let (bytes, response) = try await session.bytes(for: request)
        var data = Data()
        for try await byte in bytes {
            guard data.count < 16_384 else { throw pairingError("Pairing response is too large.") }
            data.append(byte)
        }
        guard let result = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw pairingError("Unexpected response from this Mac’s host.")
        }
        if let message = result["error"] as? String { throw pairingError(String(message.prefix(1024))) }
        guard (response as? HTTPURLResponse)?.statusCode == 200 else {
            throw pairingError("Could not access this Mac’s host. Reconnect using This Mac in Hosts.")
        }
        return result
    }
}
