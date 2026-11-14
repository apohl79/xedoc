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

    override init() {
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .nonPersistent()
        webView = WKWebView(frame: .zero, configuration: configuration)
        super.init()
        webView.navigationDelegate = self
        webView.uiDelegate = self
        observations = [
            webView.observe(\.title, options: [.new]) { [weak self] view, _ in
                self?.title = view.title ?? view.url?.host ?? "Website"
            },
            webView.observe(\.url, options: [.new]) { [weak self] view, _ in
                self?.address = view.url?.absoluteString ?? ""
            },
            webView.observe(\.canGoBack, options: [.new]) { [weak self] view, _ in
                self?.canGoBack = view.canGoBack
            },
            webView.observe(\.canGoForward, options: [.new]) { [weak self] view, _ in
                self?.canGoForward = view.canGoForward
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
        else { load(url) }
        return nil
    }

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        if (error as NSError).code != NSURLErrorCancelled { self.error = error.localizedDescription }
    }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        if (error as NSError).code != NSURLErrorCancelled { self.error = error.localizedDescription }
    }
}

struct WebsitePane: View {
    @ObservedObject var connection: HostConnection

    var body: some View {
        VStack(spacing: 0) {
            ScrollView(.horizontal) {
                HStack(spacing: 4) {
                    ForEach(connection.websites) { panel in
                        WebsiteTab(panel: panel, selected: connection.selectedWebsite == panel.id,
                                   select: { connection.selectedWebsite = panel.id },
                                   close: { connection.closeWebsite(panel) })
                    }
                }.padding(6)
            }
            if let panel = connection.websites.first(where: { $0.id == connection.selectedWebsite }) {
                WebsiteContent(panel: panel).id(panel.id)
            }
        }
        .frame(minWidth: 320, idealWidth: 520)
        .background(.background)
    }
}

private struct WebsiteTab: View {
    @ObservedObject var panel: WebsitePanel
    let selected: Bool
    let select: () -> Void
    let close: () -> Void

    var body: some View {
        HStack(spacing: 6) {
            Button(action: select) { Text(panel.title).lineLimit(1).frame(maxWidth: 180) }
                .buttonStyle(.plain)
            Button(action: close) { Image(systemName: "xmark") }
                .buttonStyle(.borderless).help("Close website")
        }
        .padding(6)
        .background(selected ? Color.primary.opacity(0.12) : .clear, in: RoundedRectangle(cornerRadius: 4))
    }
}

private struct WebsiteContent: View {
    @ObservedObject var panel: WebsitePanel

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                Button { panel.webView.goBack() } label: { Image(systemName: "chevron.left") }
                    .disabled(!panel.canGoBack).help("Back")
                Button { panel.webView.goForward() } label: { Image(systemName: "chevron.right") }
                    .disabled(!panel.canGoForward).help("Forward")
                Button { panel.webView.reload() } label: { Image(systemName: "arrow.clockwise") }
                    .help("Reload website")
                TextField("Website address", text: $panel.address).onSubmit { panel.submitAddress() }
                    .textFieldStyle(.roundedBorder)
                Button {
                    if let url = WebsitePanel.validatedURL(panel.address) { NSWorkspace.shared.open(url) }
                } label: { Image(systemName: "arrow.up.right.square") }
                    .help("Open in browser")
            }.buttonStyle(.borderless).padding(8)
            if let error = panel.error {
                Text(error).foregroundStyle(.red).textSelection(.enabled).padding(8)
            }
            WebsiteWebView(view: panel.webView)
        }
    }
}

private struct WebsiteWebView: NSViewRepresentable {
    let view: WKWebView
    func makeNSView(context: Context) -> WKWebView { view }
    func updateNSView(_ nsView: WKWebView, context: Context) {}
}
