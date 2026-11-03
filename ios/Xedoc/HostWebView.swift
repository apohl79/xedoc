import SwiftUI
import WebKit

enum HostLoadError {
    case certificate
    case connection(String)

    var message: String {
        switch self {
        case .certificate:
            return "This host’s certificate is not trusted. Set up its verified Xedoc CA, then retry."
        case .connection(let message):
            return message
        }
    }
}

struct HostWebView: UIViewRepresentable {
    let url: URL
    let reload: Int
    @Binding var error: HostLoadError?

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeUIView(context: Context) -> MobileWebView {
        let configuration = WKWebViewConfiguration()
        do {
            for name in ["Mobile", "TUIControls"] {
                guard let scriptURL = Bundle.main.url(forResource: name, withExtension: "js")
                else { throw CocoaError(.fileNoSuchFile) }
                let script = try String(contentsOf: scriptURL, encoding: .utf8)
                configuration.userContentController.addUserScript(WKUserScript(
                    source: script, injectionTime: .atDocumentEnd, forMainFrameOnly: false
                ))
            }
        } catch {
            DispatchQueue.main.async { self.error = .connection("Could not load mobile controls: \(error.localizedDescription)") }
        }
        let view = MobileWebView(frame: .zero, configuration: configuration)
        view.navigationDelegate = context.coordinator
        view.uiDelegate = context.coordinator
        view.isOpaque = false
        view.backgroundColor = .systemBackground
        view.scrollView.contentInsetAdjustmentBehavior = .never
        view.load(URLRequest(url: url))
        return view
    }

    func updateUIView(_ view: MobileWebView, context: Context) {
        context.coordinator.parent = self
        if context.coordinator.reload != reload {
            context.coordinator.reload = reload
            view.load(URLRequest(url: url))
        }
    }

    final class Coordinator: NSObject, WKNavigationDelegate, WKUIDelegate {
        var parent: HostWebView
        var reload: Int

        init(_ parent: HostWebView) {
            self.parent = parent
            reload = parent.reload
        }

        func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
            (webView as? MobileWebView)?.updateSidebar()
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
            if [NSURLErrorServerCertificateUntrusted, NSURLErrorServerCertificateHasBadDate,
                NSURLErrorServerCertificateHasUnknownRoot, NSURLErrorServerCertificateNotYetValid].contains(code) {
                parent.error = .certificate
            } else {
                parent.error = .connection(error.localizedDescription)
            }
        }

        func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction,
                     decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
            guard let url = action.request.url else { decisionHandler(.cancel); return }
            let sameOrigin = url.scheme == parent.url.scheme && url.host == parent.url.host
                && (url.port ?? 443) == (parent.url.port ?? 443)
            if sameOrigin || url.absoluteString == "about:blank" {
                decisionHandler(.allow)
            } else {
                decisionHandler(.cancel)
                if action.navigationType == .linkActivated, ["http", "https"].contains(url.scheme) {
                    UIApplication.shared.open(url)
                }
            }
        }

        func webView(_ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration,
                     for action: WKNavigationAction, windowFeatures: WKWindowFeatures) -> WKWebView? {
            if action.targetFrame == nil, let url = action.request.url,
               ["http", "https"].contains(url.scheme) {
                UIApplication.shared.open(url)
            }
            return nil
        }
    }
}

final class MobileWebView: WKWebView, UIGestureRecognizerDelegate {
    private var landscape: Bool?

    override init(frame: CGRect, configuration: WKWebViewConfiguration) {
        super.init(frame: frame, configuration: configuration)
        let swipe = UIPanGestureRecognizer(target: self, action: #selector(swiped))
        swipe.maximumNumberOfTouches = 1
        swipe.cancelsTouchesInView = false
        swipe.delegate = self
        addGestureRecognizer(swipe)
    }

    required init?(coder: NSCoder) { nil }

    override func layoutSubviews() {
        super.layoutSubviews()
        let current = window?.windowScene?.interfaceOrientation.isLandscape ?? false
        if landscape != current { updateSidebar() }
    }

    func updateSidebar() {
        landscape = window?.windowScene?.interfaceOrientation.isLandscape ?? false
        let docked = UIDevice.current.userInterfaceIdiom == .pad && landscape == true
        evaluateJavaScript("window.xedocMobileSidebar?.('layout', \(docked))", completionHandler: nil)
    }

    @objc private func swiped(_ recognizer: UIPanGestureRecognizer) {
        guard recognizer.state == .ended else { return }
        let movement = recognizer.translation(in: self)
        guard abs(movement.x) >= 60, abs(movement.x) > abs(movement.y) * 1.5 else { return }
        let action = movement.x > 0 ? "show" : "hide"
        evaluateJavaScript("window.xedocMobileSidebar?.('\(action)')", completionHandler: nil)
    }

    func gestureRecognizer(_ gestureRecognizer: UIGestureRecognizer,
                           shouldRecognizeSimultaneouslyWith otherGestureRecognizer: UIGestureRecognizer) -> Bool {
        true
    }
}
