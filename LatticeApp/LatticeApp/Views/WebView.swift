import SwiftUI
import WebKit

#if os(macOS)
struct WebView: NSViewRepresentable {
    let url: URL
    var onNavigate: ((URL) -> Bool)?

    func makeCoordinator() -> Coordinator {
        Coordinator(origin: url.host, onNavigate: onNavigate)
    }

    func makeNSView(context: Context) -> WKWebView {
        let config = WKWebViewConfiguration()
        config.preferences.setValue(true, forKey: "developerExtrasEnabled")
        let webView = WKWebView(frame: .zero, configuration: config)
        webView.navigationDelegate = context.coordinator
        webView.load(URLRequest(url: url))
        return webView
    }

    func updateNSView(_ webView: WKWebView, context: Context) {
        context.coordinator.onNavigate = onNavigate
    }
}
#else
struct WebView: UIViewRepresentable {
    let url: URL
    var onNavigate: ((URL) -> Bool)?

    func makeCoordinator() -> Coordinator {
        Coordinator(origin: url.host, onNavigate: onNavigate)
    }

    func makeUIView(context: Context) -> WKWebView {
        let config = WKWebViewConfiguration()
        let webView = WKWebView(frame: .zero, configuration: config)
        webView.navigationDelegate = context.coordinator
        webView.load(URLRequest(url: url))
        return webView
    }

    func updateUIView(_ webView: WKWebView, context: Context) {
        context.coordinator.onNavigate = onNavigate
    }
}
#endif

class Coordinator: NSObject, WKNavigationDelegate {
    let origin: String?
    var onNavigate: ((URL) -> Bool)?

    init(origin: String?, onNavigate: ((URL) -> Bool)?) {
        self.origin = origin
        self.onNavigate = onNavigate
    }

    func webView(
        _ webView: WKWebView,
        decidePolicyFor navigationAction: WKNavigationAction,
        decisionHandler: @escaping (WKNavigationActionPolicy) -> Void
    ) {
        if let url = navigationAction.request.url,
           let onNavigate,
           url.host != origin,
           !onNavigate(url) {
            decisionHandler(.cancel)
            return
        }
        decisionHandler(.allow)
    }
}
