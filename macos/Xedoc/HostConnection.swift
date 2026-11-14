import AppKit
import SwiftUI
import WebKit

@MainActor
final class HostConnection: NSObject, ObservableObject, WKNavigationDelegate, WKUIDelegate {
    let url: URL
    let webView: WKWebView
    var window: NSWindow?
    var showHosts: (() -> Void)?
    var openHost: ((URL) -> Void)?
    var authenticationChanged: ((Bool) -> Void)?
    @Published var error: String?
    @Published private(set) var certificateError = false
    private var username: String?
    private var pendingRoute: SessionRoute?

    init(url: URL) {
        self.url = url
        let defaults = UserDefaults.standard
        let key = "hostStore:\(url.absoluteString)"
        let identifier = defaults.string(forKey: key).flatMap(UUID.init(uuidString:)) ?? UUID()
        defaults.set(identifier.uuidString, forKey: key)
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = WKWebsiteDataStore(forIdentifier: identifier)
        let scriptURL = Bundle.main.url(forResource: "Native", withExtension: "js")!
        let script = try! String(contentsOf: scriptURL, encoding: .utf8)
        configuration.userContentController.addUserScript(WKUserScript(
            source: script, injectionTime: .atDocumentStart, forMainFrameOnly: true))
        webView = WKWebView(frame: .zero, configuration: configuration)
        super.init()
        configuration.userContentController.add(DesktopBridge(connection: self), name: "xedocDesktop")
        webView.navigationDelegate = self
        webView.uiDelegate = self
    }

    func load() {
        error = nil
        certificateError = false
        (window as? HostWindow)?.headerDragStart = nil
        webView.load(URLRequest(url: url))
    }

    func close() {
        webView.stopLoading()
        webView.configuration.userContentController.removeScriptMessageHandler(forName: "xedocDesktop")
        window?.contentView = nil
        window = nil
    }

    func receive(_ message: WKScriptMessage) {
        guard message.frameInfo.isMainFrame, let source = message.frameInfo.request.url,
              sameOrigin(source), let event = message.body as? [String: Any]
        else { return }
        switch event["kind"] as? String {
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
            let wasAuthenticated = username != nil
            username = nil
            if wasAuthenticated || window?.isVisible == false { authenticationChanged?(false) }
        case "hosts":
            showHosts?()
        case "openHost":
            if let origin = event["url"] as? String, let target = DiscoveredHost.validatedURL(origin) {
                openHost?(target)
            }
        case "titlebar":
            guard let window = window as? HostWindow,
                  let start = event["dragStart"] as? Double, start.isFinite, start >= 80 else { return }
            window.headerDragStart = start
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

    private func showError(_ error: Error) {
        let code = (error as NSError).code
        guard code != NSURLErrorCancelled else { return }
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

    func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction,
                 decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
        guard let target = action.request.url else { decisionHandler(.cancel); return }
        if sameOrigin(target) || target.absoluteString == "about:blank" {
            decisionHandler(.allow)
        } else {
            decisionHandler(.cancel)
            if action.navigationType == .linkActivated, ["https", "http"].contains(target.scheme) {
                NSWorkspace.shared.open(target)
            }
        }
    }

    func webView(_ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration,
                 for action: WKNavigationAction, windowFeatures: WKWindowFeatures) -> WKWebView? {
        guard let target = action.request.url else { return nil }
        if ["http", "https"].contains(target.scheme) { NSWorkspace.shared.open(target) }
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
            .ignoresSafeArea()
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
    func makeNSView(context: Context) -> WKWebView { view }
    func updateNSView(_ nsView: WKWebView, context: Context) {}
}
