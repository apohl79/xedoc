import Combine
import Security
import UserNotifications
import WebKit

@MainActor
final class PushSubscription: ObservableObject {
    static let shared = PushSubscription()
    @Published var notice: String?
    private(set) var usesRemotePush = false
    private var token: String?
    private weak var webView: WKWebView?
    private var refreshTask: Task<Void, Never>?
    private var refreshAgain = false
    private var appleRegistrationFailed = false
    private var switching = false
    private var storageError: Error?
    private var record = Record()
    private let session = URLSession(configuration: .ephemeral, delegate: PushRedirectGuard(), delegateQueue: nil)
    private let keychain: [String: Any] = [
        kSecClass as String: kSecClassGenericPassword,
        kSecAttrService as String: "org.e47.xedoc.mobile.push",
        kSecAttrAccount as String: "subscription"
    ]

    private struct Record: Codable {
        var installationId = UUID().uuidString
        var origin: String?
        var subscriptionId: String?
    }

    private init() {
        var query = keychain
        query[kSecReturnData as String] = true
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecSuccess, let data = result as? Data {
            do {
                record = try JSONDecoder().decode(Record.self, from: data)
                usesRemotePush = record.subscriptionId != nil
            }
            catch { storageError = error }
        } else if status != errSecItemNotFound {
            storageError = failure("Could not read the saved push subscription from Keychain.")
        }
    }

    static func origin(_ url: URL) -> String {
        var parts = URLComponents()
        parts.scheme = url.scheme?.lowercased()
        parts.host = url.host?.lowercased()
        if let port = url.port, port != 443 { parts.port = port }
        return parts.string ?? ""
    }

    func select(_ url: URL?) async throws {
        guard !switching else { throw failure("A host change is already in progress.") }
        switching = true
        defer { switching = false }
        await refreshTask?.value
        if let storageError { throw storageError }
        let next = url.map(Self.origin)
        if record.origin != next {
            try await revoke()
            var updated = record
            updated.origin = next
            try persist(updated)
            usesRemotePush = false
            webView = nil
            notice = nil
            UNUserNotificationCenter.current().removeAllDeliveredNotifications()
            UNUserNotificationCenter.current().removeAllPendingNotificationRequests()
        }
    }

    func ready(_ view: WKWebView) {
        guard !switching, let url = view.url, Self.origin(url) == record.origin else { return }
        webView = view
        guard refreshTask == nil else { return }
        refreshTask = Task {
            defer {
                refreshTask = nil
                if refreshAgain {
                    refreshAgain = false
                    ready(view)
                }
            }
            do { try await refresh(view) }
            catch { notice = "Push notifications: \(error.localizedDescription)" }
        }
    }

    func receivedToken(_ data: Data) {
        token = data.map { String(format: "%02x", $0) }.joined()
        appleRegistrationFailed = false
        NSLog("Xedoc APNs device registration succeeded")
        if refreshTask != nil { refreshAgain = true }
        if let webView { ready(webView) }
    }

    func registrationFailed() {
        appleRegistrationFailed = true
        notice = "Background push is unavailable: Apple device registration failed. Open-app alerts only."
    }

    func permissionChanged() async {
        let status = await UNUserNotificationCenter.current().notificationSettings().authorizationStatus
        if status == .denied {
            guard !switching else { return }
            switching = true
            await refreshTask?.value
            do {
                try await revoke()
                usesRemotePush = false
                notice = "Notifications are disabled. Enable them in iOS Settings."
            } catch {
                notice = "Notifications are disabled; could not revoke the old subscription. Reconnect this host and retry."
            }
            switching = false
        } else if let webView {
            ready(webView)
        }
    }

    func matches(_ info: [AnyHashable: Any]) -> Bool {
        info["xedocHost"] as? String == record.origin
            && info["installationId"] as? String == record.installationId
    }

    func isSelected(_ url: URL) -> Bool { Self.origin(url) == record.origin }

    var notificationInfo: [String: String] {
        ["xedocHost": record.origin ?? "", "installationId": record.installationId]
    }

    private func refresh(_ view: WKWebView) async throws {
        guard let origin = record.origin, let url = URL(string: origin) else { return }
        let settings = await UNUserNotificationCenter.current().notificationSettings()
        guard [.authorized, .provisional, .ephemeral].contains(settings.authorizationStatus) else { return }
        let cookies = await view.configuration.websiteDataStore.httpCookieStore.allCookies()
        let status = try await request("/api/push/status", origin: url, cookies: cookies)
        if status.code == 404 {
            usesRemotePush = false
            notice = "This host has no background push. Alerts work only while the app is running."
            return
        }
        guard status.code == 200, let available = status.json["available"] as? Bool else {
            throw failure("Could not check this host’s push service. Sign in and retry.")
        }
        guard available else {
            if record.subscriptionId != nil { try await revoke() }
            usesRemotePush = false
            notice = "This host has no APNs configuration. Alerts work only while the app is running."
            return
        }
        guard let token else {
            notice = appleRegistrationFailed
                ? "Apple push registration failed. Alerts work only while the app is running."
                : "Waiting for Apple push registration. Background alerts are not ready."
            return
        }
        for attempt in 0..<2 {
            if record.subscriptionId == nil {
                var bytes = [UInt8](repeating: 0, count: 32)
                guard SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes) == errSecSuccess else {
                    throw failure("Could not create a secure push subscription.")
                }
                var updated = record
                updated.subscriptionId = bytes.map { String(format: "%02x", $0) }.joined()
                try persist(updated)
            }
            // A lost response can still leave an active server registration.
            usesRemotePush = true
            let response = try await request("/api/push/register", origin: url, cookies: cookies, body: [
                "installationId": record.installationId,
                "subscriptionId": record.subscriptionId!,
                "deviceToken": token,
                "environment": Self.environment
            ])
            if response.code == 200, response.json["registered"] as? Bool == true {
                notice = nil
                return
            }
            if response.code == 400, attempt == 0 {
                try await revoke()
                continue
            }
            throw failure("Registration failed. Sign in to this host and retry.")
        }
    }

    private func revoke() async throws {
        guard let secret = record.subscriptionId, let origin = record.origin, let url = URL(string: origin) else { return }
        let cookies = await WKWebsiteDataStore.default().httpCookieStore.allCookies()
        let response = try await request("/api/push/unregister", origin: url, cookies: cookies,
                                         body: ["subscriptionId": secret])
        guard response.code == 200 else {
            throw failure("The old host could not confirm push removal. Stay connected and retry before switching hosts.")
        }
        var updated = record
        updated.subscriptionId = nil
        try persist(updated)
    }

    private func persist(_ updated: Record) throws {
        let data = try JSONEncoder().encode(updated)
        var attributes: [String: Any] = [
            kSecValueData as String: data,
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        ]
        let status = SecItemUpdate(keychain as CFDictionary, attributes as CFDictionary)
        if status == errSecItemNotFound {
            attributes.merge(keychain) { current, _ in current }
            guard SecItemAdd(attributes as CFDictionary, nil) == errSecSuccess else {
                throw failure("Could not save the push subscription in Keychain.")
            }
        } else if status != errSecSuccess {
            throw failure("Could not save the push subscription in Keychain.")
        }
        record = updated
    }

    private func request(_ path: String, origin: URL, cookies: [HTTPCookie] = [],
                         body: [String: String]? = nil) async throws -> (code: Int, json: [String: Any]) {
        let url = origin.appendingPathComponent(String(path.dropFirst()))
        var request = URLRequest(url: url)
        request.timeoutInterval = 15
        request.httpShouldHandleCookies = false
        request.setValue(Self.origin(origin), forHTTPHeaderField: "Origin")
        if let body {
            request.httpMethod = "POST"
            request.httpBody = try JSONSerialization.data(withJSONObject: body)
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        let matching = cookies.filter { cookie in
            let domain = cookie.domain.lowercased()
            let host = url.host?.lowercased() ?? ""
            let domainMatches = domain.hasPrefix(".")
                ? host == String(domain.dropFirst()) || host.hasSuffix(domain)
                : host == domain
            let pathMatches = url.path == cookie.path || url.path.hasPrefix(
                cookie.path.hasSuffix("/") ? cookie.path : cookie.path + "/"
            )
            return domainMatches && pathMatches && (!cookie.isSecure || url.scheme == "https")
                && (cookie.expiresDate.map { $0 > Date() } ?? true)
        }
        for (name, value) in HTTPCookie.requestHeaderFields(with: matching) {
            request.setValue(value, forHTTPHeaderField: name)
        }
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse else { throw failure("The host returned an invalid response.") }
        return (http.statusCode, (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] ?? [:])
    }

    private func failure(_ message: String) -> NSError {
        NSError(domain: "XedocPush", code: 1, userInfo: [NSLocalizedDescriptionKey: message])
    }

    private static var environment: String {
        if let profile = Bundle.main.url(forResource: "embedded", withExtension: "mobileprovision"),
           let data = try? Data(contentsOf: profile),
           let start = data.range(of: Data("<?xml".utf8)),
           let end = data.range(of: Data("</plist>".utf8), in: start.lowerBound..<data.endIndex),
           let plist = try? PropertyListSerialization.propertyList(
               from: data.subdata(in: start.lowerBound..<end.upperBound), format: nil
           ) as? [String: Any],
           let entitlements = plist["Entitlements"] as? [String: Any],
           let value = entitlements["aps-environment"] as? String,
           ["development", "production"].contains(value) {
            return value
        }
        #if DEBUG
        return "development"
        #else
        return "production"
        #endif
    }
}

private final class PushRedirectGuard: NSObject, URLSessionTaskDelegate {
    func urlSession(_ session: URLSession, task: URLSessionTask,
                    willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest,
                    completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(nil)
    }
}
