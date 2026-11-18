import AppKit
import SwiftUI
import WebKit

@MainActor
final class WebsitePanel: NSObject, ObservableObject, Identifiable, WKNavigationDelegate, WKUIDelegate {
    let id = UUID()
    let webView: WKWebView
    @Published private(set) var title = "Website"
    @Published var address = ""
    @Published private(set) var canGoBack = false
    @Published private(set) var canGoForward = false
    @Published private(set) var error: String?
    private var observations: [NSKeyValueObservation] = []
    var changed: (() -> Void)?
    var openNew: ((URL) -> Void)?

    override init() {
        let configuration = WKWebViewConfiguration()
        // Panels share the persistent default store so logins carry across panels and launches.
        configuration.websiteDataStore = WKWebsiteDataStore.default()
        webView = WKWebView(frame: .zero, configuration: configuration)
        super.init()
        webView.navigationDelegate = self
        webView.uiDelegate = self
        observations = [
            webView.observe(\.title, options: [.new]) { [weak self] view, _ in
                MainActor.assumeIsolated {
                    self?.title = view.title ?? view.url?.host ?? "Website"
                    self?.changed?()
                }
            },
            webView.observe(\.url, options: [.new]) { [weak self] view, _ in
                MainActor.assumeIsolated {
                    self?.address = view.url?.absoluteString ?? ""
                    self?.changed?()
                }
            },
            webView.observe(\.canGoBack, options: [.new]) { [weak self] view, _ in
                MainActor.assumeIsolated {
                    self?.canGoBack = view.canGoBack
                    self?.changed?()
                }
            },
            webView.observe(\.canGoForward, options: [.new]) { [weak self] view, _ in
                MainActor.assumeIsolated {
                    self?.canGoForward = view.canGoForward
                    self?.changed?()
                }
            },
        ]
    }

    static func validatedURL(_ text: String) -> URL? {
        guard text.utf8.count <= 16_384, let url = URL(string: text),
              ["http", "https"].contains(url.scheme?.lowercased()),
              url.host != nil, url.user == nil, url.password == nil else { return nil }
        return url
    }

    func load(_ url: URL) {
        guard Self.validatedURL(url.absoluteString) != nil else { return }
        error = nil
        address = url.absoluteString
        title = url.host ?? "Website"
        webView.load(URLRequest(url: url))
    }

    func submitAddress() {
        let text = address.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let url = Self.validatedURL(text.contains("://") ? text : "https://\(text)") else {
            error = "Enter an HTTP or HTTPS website address."
            return
        }
        load(url)
    }

    func close() {
        webView.stopLoading()
        observations.removeAll()
        webView.navigationDelegate = nil
        webView.uiDelegate = nil
        webView.removeFromSuperview()
    }

    func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction,
                 decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
        guard let url = action.request.url, Self.validatedURL(url.absoluteString) != nil else {
            decisionHandler(.cancel)
            return
        }
        if action.navigationType == .linkActivated && action.modifierFlags.contains(.command) {
            decisionHandler(.cancel)
            NSWorkspace.shared.open(url)
        } else {
            error = nil
            decisionHandler(.allow)
        }
    }

    func webView(_ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration,
                 for action: WKNavigationAction, windowFeatures: WKWindowFeatures) -> WKWebView? {
        guard let url = action.request.url, Self.validatedURL(url.absoluteString) != nil else { return nil }
        if action.modifierFlags.contains(.command) { NSWorkspace.shared.open(url) }
        else { openNew?(url) }
        return nil
    }

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        if (error as NSError).code != NSURLErrorCancelled {
            self.error = error.localizedDescription
            changed?()
        }
    }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        if (error as NSError).code != NSURLErrorCancelled {
            self.error = error.localizedDescription
            changed?()
        }
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        error = nil
        changed?()
    }
}

extension HostConnection {
    func receiveWebsiteCommand(_ event: [String: Any]) {
        let id = event["id"] as? String ?? ""
        switch event["command"] as? String {
        case "open":
            guard websites.count < 64, websites[id] == nil, UUID(uuidString: id) != nil,
                  let container = webView.superview,
                  let text = event["url"] as? String, let url = WebsitePanel.validatedURL(text) else { return }
            let panel = WebsitePanel()
            websites[id] = panel
            panel.openNew = { [weak self, weak panel] url in
                guard panel?.webView.isHidden == false else { return }
                self?.openWebsite(url)
            }
            panel.webView.isHidden = true
            container.addSubview(panel.webView)
            panel.changed = { [weak self, weak panel] in
                guard let self, let panel else { return }
                let state: [String: Any] = [
                    "id": id, "title": panel.title, "address": panel.address,
                    "canGoBack": panel.canGoBack, "canGoForward": panel.canGoForward,
                    "error": panel.error as Any? ?? NSNull(),
                ]
                self.webView.callAsyncJavaScript("window.xedocWebsiteState?.(state)",
                    arguments: ["state": state], in: nil, in: .page) { _ in }
            }
            panel.load(url)
        case "layout":
            for (key, panel) in websites { panel.webView.isHidden = key != id }
            guard let panel = websites[id], let rect = event["rect"] as? [String: Double],
                  let x = rect["x"], let y = rect["y"],
                  let width = rect["width"], let height = rect["height"],
                  [x, y, width, height].allSatisfy(\.isFinite),
                  x >= 0, y >= 0, width > 0, height > 0 else { return }
            let frame = NSRect(x: x, y: webView.isFlipped ? y : webView.bounds.height - y - height,
                               width: width, height: height)
            panel.webView.frame = webView.convert(frame, to: webView.superview)
        case "close":
            websites.removeValue(forKey: id)?.close()
        case "clear":
            websites.values.forEach { $0.close() }
            websites.removeAll()
        case "back": websites[id]?.webView.goBack()
        case "forward": websites[id]?.webView.goForward()
        case "reload": websites[id]?.webView.reload()
        case "external":
            if let text = websites[id]?.address, let url = WebsitePanel.validatedURL(text) {
                NSWorkspace.shared.open(url)
            }
        case "navigate":
            guard let panel = websites[id], let text = event["url"] as? String else { return }
            panel.address = text
            panel.submitAddress()
            panel.changed?()
        default: break
        }
    }
}
