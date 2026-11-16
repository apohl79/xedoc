import AppKit
import SwiftUI
import WebKit

@MainActor
final class HostConnection: NSObject, ObservableObject, WKNavigationDelegate, WKUIDelegate {
    private(set) var url: URL
    let isLocalHost: Bool
    let webView: WKWebView
    var window: NSWindow?
    var showHosts: (() -> Void)?
    var openHost: ((URL) -> Void)?
    var authenticationChanged: ((Bool) -> Void)?
    @Published var error: String?
    @Published var pairingDevice = false
    var websites: [String: WebsitePanel] = [:]
    @Published private(set) var certificateError = false
    private var username: String?
    private var pendingRoute: SessionRoute?
    private let viewStateKey: String
    private var localAccessToken: String?
    private var localMonitor: Task<Void, Never>?
    private var retryLocalConnection = false

    init(url: URL) {
        self.url = url
        let defaults = UserDefaults.standard
        isLocalHost = HostSelector.readLocalHost()?.url.appendingPathComponent("") == url
        viewStateKey = "hostView:\(isLocalHost ? "local" : url.absoluteString)"
        let key = "hostStore:\(url.absoluteString)"
        let identifier = defaults.string(forKey: key).flatMap(UUID.init(uuidString:)) ?? UUID()
        defaults.set(identifier.uuidString, forKey: key)
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = HostDevicePairing.dataStore(for: url) ?? WKWebsiteDataStore(forIdentifier: identifier)
        webView = WKWebView(frame: .zero, configuration: configuration)
        super.init()
        configuration.userContentController.add(DesktopBridge(connection: self), name: "xedocDesktop")
        configuration.userContentController.add(WebsiteBridge(connection: self),
                                                contentWorld: .defaultClient, name: "xedocWebsite")
        webView.navigationDelegate = self
        webView.uiDelegate = self
        if isLocalHost {
            localMonitor = Task { [weak self] in
                while !Task.isCancelled {
                    do { try await Task.sleep(for: .seconds(2)) }
                    catch { return }
                    guard let self else { return }
                    guard let local = HostSelector.localAccess() else { continue }
                    if local.url.appendingPathComponent("") != self.url
                        || local.token != self.localAccessToken || self.retryLocalConnection {
                        self.load()
                    }
                }
            }
        }
    }

    func load() {
        websites.values.forEach { $0.close() }
        websites.removeAll()
        error = nil
        certificateError = false
        retryLocalConnection = false
        (window as? HostWindow)?.headerDragStart = nil
        if isLocalHost, let local = HostSelector.localAccess() {
            url = local.url.appendingPathComponent("")
            localAccessToken = local.token
            window?.title = url.host.map { "\($0):\(url.port ?? 80)" } ?? "Xedoc"
        }
        let controller = webView.configuration.userContentController
        controller.removeAllUserScripts()
        let scriptURL = Bundle.main.url(forResource: "Native", withExtension: "js")!
        let script = try! String(contentsOf: scriptURL, encoding: .utf8)
        controller.addUserScript(WKUserScript(
            source: script, injectionTime: .atDocumentStart, forMainFrameOnly: true))
        let linksURL = Bundle.main.url(forResource: "WebsiteLinks", withExtension: "js")!
        let links = try! String(contentsOf: linksURL, encoding: .utf8)
        controller.addUserScript(WKUserScript(
            source: links, injectionTime: .atDocumentStart, forMainFrameOnly: false,
            in: .defaultClient))
        if let data = UserDefaults.standard.data(forKey: viewStateKey),
           let _ = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
           let encoded = String(data: data, encoding: .utf8),
           let originData = try? JSONSerialization.data(withJSONObject: [url.absoluteString]),
           let origin = String(data: originData, encoding: .utf8) {
            let restore = """
            if (location.origin === new URL(\(origin)[0]).origin) {
                try {
                    if (!sessionStorage.getItem("xedoc-host-view-v1")) {
                        sessionStorage.setItem("xedoc-host-view-v1", JSON.stringify(\(encoded)));
                    }
                } catch {}
            }
            """
            controller.addUserScript(WKUserScript(
                source: restore, injectionTime: .atDocumentStart, forMainFrameOnly: true))
        }
        var request = URLRequest(url: url)
        if let local = HostSelector.localAccess(), sameOrigin(local.url) {
            request.setValue("Bearer \(local.token)", forHTTPHeaderField: "Authorization")
        }
        webView.load(request)
    }

    func close() {
        localMonitor?.cancel()
        localMonitor = nil
        webView.stopLoading()
        webView.configuration.userContentController.removeScriptMessageHandler(forName: "xedocDesktop")
        webView.configuration.userContentController.removeScriptMessageHandler(
            forName: "xedocWebsite", contentWorld: .defaultClient)
        websites.values.forEach { $0.close() }
        websites.removeAll()
        window?.contentView = nil
        window = nil
    }

    func receive(_ message: WKScriptMessage) {
        guard message.frameInfo.isMainFrame, let source = message.frameInfo.request.url,
              sameOrigin(source), let event = message.body as? [String: Any]
        else { return }
        switch event["kind"] as? String {
        case "viewState":
            guard let view = event["view"] as? [String: Any],
                  let name = view["username"] as? String, !name.isEmpty, name.count <= 64,
                  ["workspaceId", "projectName", "threadId"].allSatisfy({ key in
                      view[key] is NSNull || (view[key] as? String).map { $0.utf8.count <= 1024 } == true
                  }) else { return }
            let saved = view.filter { ["username", "workspaceId", "projectName", "threadId"].contains($0.key) }
            if let data = try? JSONSerialization.data(withJSONObject: saved) {
                UserDefaults.standard.set(data, forKey: viewStateKey)
            }
        case "authenticated":
            guard let name = event["username"] as? String, !name.isEmpty, name.count <= 64 else { return }
            let changed = username != name
            username = name
            if changed {
                authenticationChanged?(true)
                DesktopNotifications.shared.requestPermission()
            }
            if let route = pendingRoute { navigate(to: route) }
        case "signedOut":
            if let local = HostSelector.localAccess(), sameOrigin(local.url) {
                load()
                return
            }
            let wasAuthenticated = username != nil
            username = nil
            if wasAuthenticated || window?.isVisible == false { authenticationChanged?(false) }
        case "hosts":
            showHosts?()
        case "pairDevice":
            pairingDevice = true
        case "openHost":
            if let origin = event["url"] as? String, let target = DiscoveredHost.validatedURL(origin) {
                openHost?(target)
            }
        case "titlebar":
            guard let window = window as? HostWindow,
                  let start = event["dragStart"] as? Double, start.isFinite, start >= 80 else { return }
            window.headerDragStart = start
        case "website":
            receiveWebsiteCommand(event)
        default:
            DesktopNotifications.shared.receive(event, host: url, username: username)
        }
    }

    func navigate(to route: SessionRoute) {
        pendingRoute = route
        guard let username else { return }
        guard username == route.username else {
            pendingRoute = nil
            error = "This notification belongs to a different signed-in account."
            return
        }
        pendingRoute = nil
        webView.callAsyncJavaScript("return await window.xedocOpenSession(route)",
                                   arguments: ["route": route.info], in: nil, in: .page) { [weak self] result in
            if case .failure(let error) = result {
                self?.error = "Could not open the notified session: \(error.localizedDescription)"
            }
        }
    }

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        showError(error)
    }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        showError(error)
    }

    func webView(_ webView: WKWebView, didReceive challenge: URLAuthenticationChallenge,
                 completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void) {
        HostDevicePairing.authenticate(challenge, for: url) { disposition, credential in
            if disposition == .cancelAuthenticationChallenge {
                self.certificateError = false
                self.error = "Host identity verification failed. Obtain a new pairing invitation directly from your trusted host."
            }
            completionHandler(disposition, credential)
        }
    }

    private func showError(_ error: Error) {
        let code = (error as NSError).code
        guard code != NSURLErrorCancelled else { return }
        retryLocalConnection = isLocalHost && (error as NSError).domain == NSURLErrorDomain
        certificateError = [NSURLErrorServerCertificateUntrusted, NSURLErrorServerCertificateHasBadDate,
                            NSURLErrorServerCertificateHasUnknownRoot, NSURLErrorServerCertificateNotYetValid].contains(code)
        self.error = certificateError
            ? "This host’s certificate is not trusted. Verify its CA fingerprint with the administrator, then install and trust the CA in Keychain Access."
            : error.localizedDescription
        if window?.isVisible == false { showHosts?() }
    }

    private func sameOrigin(_ target: URL) -> Bool {
        target.scheme == url.scheme && target.host == url.host && (target.port ?? 443) == (url.port ?? 443)
    }

    func openWebsite(_ url: URL, external: Bool = false) {
        guard WebsitePanel.validatedURL(url.absoluteString) != nil else { return }
        if external {
            NSWorkspace.shared.open(url)
            return
        }
        webView.callAsyncJavaScript("window.xedocOpenWebsite?.(url)",
                                   arguments: ["url": url.absoluteString], in: nil, in: .page) { _ in }
    }

    func receiveWebsite(_ message: WKScriptMessage) {
        guard let source = message.frameInfo.request.url, sameOrigin(source),
              let body = message.body as? [String: Any],
              let text = body["url"] as? String, let url = WebsitePanel.validatedURL(text)
        else { return }
        openWebsite(url, external: body["external"] as? Bool ?? false)
    }

    func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction,
                 decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
        guard let target = action.request.url else { decisionHandler(.cancel); return }
        if sameOrigin(target) || target.absoluteString == "about:blank" {
            decisionHandler(.allow)
        } else {
            decisionHandler(.cancel)
            if action.navigationType == .linkActivated {
                if target.scheme?.lowercased() == "slack",
                   let source = action.sourceFrame.request.url, sameOrigin(source) {
                    NSWorkspace.shared.open(target)
                } else if ["https", "http"].contains(target.scheme) {
                    openWebsite(target, external: action.modifierFlags.contains(.command))
                }
            }
        }
    }

    func webView(_ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration,
                 for action: WKNavigationAction, windowFeatures: WKWindowFeatures) -> WKWebView? {
        guard let target = action.request.url else { return nil }
        if ["http", "https"].contains(target.scheme) {
            openWebsite(target, external: sameOrigin(target) || action.modifierFlags.contains(.command))
        }
        return nil
    }

    func webView(_ webView: WKWebView, runJavaScriptAlertPanelWithMessage message: String,
                 initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping () -> Void) {
        let alert = NSAlert()
        alert.messageText = String(message.prefix(2048))
        if let window { alert.beginSheetModal(for: window) { _ in completionHandler() } }
        else { completionHandler() }
    }

    func webView(_ webView: WKWebView, runJavaScriptConfirmPanelWithMessage message: String,
                 initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping (Bool) -> Void) {
        let alert = NSAlert()
        alert.messageText = String(message.prefix(2048))
        alert.addButton(withTitle: "OK")
        alert.addButton(withTitle: "Cancel")
        if let window {
            alert.beginSheetModal(for: window) { completionHandler($0 == .alertFirstButtonReturn) }
        } else { completionHandler(false) }
    }
}

@MainActor
private final class WebsiteBridge: NSObject, WKScriptMessageHandler {
    weak var connection: HostConnection?
    init(connection: HostConnection) { self.connection = connection }
    func userContentController(_ controller: WKUserContentController, didReceive message: WKScriptMessage) {
        connection?.receiveWebsite(message)
    }
}

@MainActor
private final class DesktopBridge: NSObject, WKScriptMessageHandler {
    weak var connection: HostConnection?
    init(connection: HostConnection) { self.connection = connection }
    func userContentController(_ controller: WKUserContentController, didReceive message: WKScriptMessage) {
        connection?.receive(message)
    }
}

struct HostScreen: View {
    @ObservedObject var connection: HostConnection

    var body: some View {
        DesktopWebView(view: connection.webView)
            .frame(minWidth: 320)
            .ignoresSafeArea()
            .sheet(isPresented: $connection.pairingDevice) {
                PairDeviceView(connection: connection)
            }
            .overlay {
                if let error = connection.error {
                    VStack(spacing: 16) {
                        Image(systemName: "exclamationmark.triangle").font(.largeTitle)
                        Text(error).multilineTextAlignment(.center).frame(maxWidth: 460)
                        if connection.certificateError {
                            Button("Open Certificate Setup") {
                                NSWorkspace.shared.open(connection.url.appendingPathComponent("trust"))
                            }
                        }
                        HStack {
                            Button("Hosts") { connection.showHosts?() }
                            Button("Retry") { connection.load() }
                            Button("Dismiss") { connection.error = nil }
                        }
                    }.padding(24).frame(maxWidth: .infinity, maxHeight: .infinity).background(.background)
                }
            }
    }
}

private struct DesktopWebView: NSViewRepresentable {
    let view: WKWebView
    func makeNSView(context: Context) -> NSView {
        let container = NSView()
        view.frame = container.bounds
        view.autoresizingMask = [.width, .height]
        container.addSubview(view)
        return container
    }
    func updateNSView(_ nsView: NSView, context: Context) {}
}
