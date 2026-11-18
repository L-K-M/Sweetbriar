// Human verification's child sheet (plan M2.14b; spec/10 "The Proton
// API", spec/08 "Proton sign-in"): Proton's verification page in a
// `WKWebView` with a non-persistent data store, so it shares no cookies
// and keeps nothing. Navigations follow `HumanVerificationPolicy`, its
// content rules block loads from any other host, no window opens, and
// only the page's own messages are read. Closing tears the view down.

import AppKit
import SweetbriarBridge
import WebKit

@MainActor
final class HumanVerificationSheet: NSObject, WKNavigationDelegate, WKUIDelegate, WKScriptMessageHandler {
    private let url: URL
    private let onMessage: @MainActor (HumanVerificationMessage) -> Void
    private var window: NSWindow?
    private var webView: WKWebView?
    private var status = NSTextField(wrappingLabelWithString: "Loading Proton's verification page…")

    init(url: URL, onMessage: @escaping @MainActor (HumanVerificationMessage) -> Void) {
        self.url = url
        self.onMessage = onMessage
    }

    /// Attach to `parent` and load the page once its rules compile; a
    /// page that can't be held to them doesn't load at all.
    func present(on parent: NSWindow) {
        let content = NSView(frame: NSRect(x: 0, y: 0, width: 480, height: 640))
        let cancel = NSButton(title: "Cancel", target: self, action: #selector(cancel(_:)))
        cancel.keyEquivalent = "\u{1b}"
        for view in [status, cancel] {
            view.translatesAutoresizingMaskIntoConstraints = false
            content.addSubview(view)
        }
        NSLayoutConstraint.activate([
            status.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 20),
            status.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -20),
            status.topAnchor.constraint(equalTo: content.topAnchor, constant: 20),
            cancel.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -20),
            cancel.bottomAnchor.constraint(equalTo: content.bottomAnchor, constant: -20),
        ])
        let window = NSWindow(
            contentRect: content.frame, styleMask: [.titled, .resizable], backing: .buffered, defer: false
        )
        window.title = "Verify You're Human"
        window.contentView = content
        window.minSize = NSSize(width: 400, height: 520)
        self.window = window
        parent.beginSheet(window)
        Task { await load(into: content, above: cancel) }
    }

    /// End the sheet and drop the page with everything it stored.
    func close() {
        if let webView {
            webView.stopLoading()
            webView.navigationDelegate = nil
            webView.uiDelegate = nil
            webView.configuration.userContentController.removeAllScriptMessageHandlers()
            webView.removeFromSuperview()
        }
        webView = nil
        if let window, let parent = window.sheetParent {
            parent.endSheet(window)
        }
        window = nil
    }

    private func load(into content: NSView, above cancel: NSView) async {
        let rules: WKContentRuleList
        do {
            rules = try await HumanVerificationRules.compiled()
        } catch {
            status.stringValue = "Sweetbriar couldn't restrict the verification page, so it won't open it: \(error.localizedDescription)"
            return
        }
        guard window != nil else { return }
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .nonPersistent()
        configuration.preferences.javaScriptCanOpenWindowsAutomatically = false
        let controller = WKUserContentController()
        controller.add(rules)
        controller.addUserScript(WKUserScript(
            source: HumanVerificationPolicy.pageScript, injectionTime: .atDocumentStart, forMainFrameOnly: true
        ))
        controller.add(WeakScriptHandler(self), name: HumanVerificationPolicy.messageHandlerName)
        configuration.userContentController = controller
        let webView = WKWebView(frame: .zero, configuration: configuration)
        webView.navigationDelegate = self
        webView.uiDelegate = self
        webView.allowsBackForwardNavigationGestures = false
        webView.translatesAutoresizingMaskIntoConstraints = false
        content.addSubview(webView)
        NSLayoutConstraint.activate([
            webView.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            webView.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            webView.topAnchor.constraint(equalTo: content.topAnchor),
            webView.bottomAnchor.constraint(equalTo: cancel.topAnchor, constant: -12),
        ])
        self.webView = webView
        webView.load(URLRequest(url: url))
    }

    @objc private func cancel(_ sender: Any?) {
        onMessage(.closed)
    }

    // MARK: WKNavigationDelegate

    func webView(
        _ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction,
        decisionHandler: @escaping @MainActor @Sendable (WKNavigationActionPolicy) -> Void
    ) {
        // No target frame is a new window: refused like any other.
        guard let target = navigationAction.targetFrame, let url = navigationAction.request.url,
              !navigationAction.shouldPerformDownload,
              HumanVerificationPolicy.allows(url, mainFrame: target.isMainFrame)
        else {
            decisionHandler(.cancel)
            return
        }
        decisionHandler(.allow)
    }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: any Error) {
        showFailure(error, over: webView)
    }

    func webView(
        _ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: any Error
    ) {
        showFailure(error, over: webView)
    }

    /// The web view covers the status line; hide it so the reason shows.
    private func showFailure(_ error: any Error, over webView: WKWebView) {
        webView.isHidden = true
        status.stringValue = "The verification page didn't load: \(error.localizedDescription)"
    }

    // MARK: WKUIDelegate

    func webView(
        _ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration,
        for navigationAction: WKNavigationAction, windowFeatures: WKWindowFeatures
    ) -> WKWebView? {
        nil
    }

    // MARK: WKScriptMessageHandler

    /// Only the verification page's main frame speaks; the CAPTCHA's
    /// frame and anything else are ignored.
    func userContentController(
        _ userContentController: WKUserContentController, didReceive message: WKScriptMessage
    ) {
        let origin = message.frameInfo.securityOrigin
        guard message.frameInfo.isMainFrame, origin.protocol == "https",
              origin.host == HumanVerificationPolicy.pageHost, origin.port == 0,
              let parsed = HumanVerificationMessage.parse(message.body)
        else { return }
        onMessage(parsed)
    }
}

/// Compiled once per launch, like the reader's rules. A failed
/// compilation isn't kept, so the next sheet tries again.
@MainActor
enum HumanVerificationRules {
    private static var compilation: Task<WKContentRuleList, any Error>?

    static func compiled() async throws -> WKContentRuleList {
        let task = compilation ?? Task { @MainActor in
            // WebKit keeps compiled lists by identifier across launches:
            // a change to the rules needs a new identifier.
            guard let rules = try await WKContentRuleListStore.default().compileContentRuleList(
                forIdentifier: "SweetbriarHumanVerification-v1",
                encodedContentRuleList: HumanVerificationPolicy.contentRules
            ) else { throw URLError(.cannotLoadFromNetwork) }
            return rules
        }
        compilation = task
        do {
            return try await task.value
        } catch {
            // Only the failed task: a caller may already have started
            // the next one.
            if compilation == task { compilation = nil }
            throw error
        }
    }
}

/// `WKUserContentController` holds its handlers strongly; this keeps
/// the sheet free to go.
@MainActor
private final class WeakScriptHandler: NSObject, WKScriptMessageHandler {
    private weak var target: (any WKScriptMessageHandler)?

    init(_ target: any WKScriptMessageHandler) {
        self.target = target
    }

    func userContentController(
        _ userContentController: WKUserContentController, didReceive message: WKScriptMessage
    ) {
        target?.userContentController(userContentController, didReceive: message)
    }
}
