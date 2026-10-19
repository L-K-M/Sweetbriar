import AppKit
import WebKit
import SweetbriarBridge

/// A process-wide compilation, including a cached failure. No pane can load
/// HTML until it has installed this independent network barrier.
@MainActor
enum ReaderContentRules {
    static let source = #"""
    [{"trigger":{"url-filter":".*"},"action":{"type":"block"}},
     {"trigger":{"url-filter":"^sweetbriar-cid:"},"action":{"type":"ignore-previous-rules"}}]
    """#
    private static var compilation: Task<WKContentRuleList, Error>?

    static func compiled() async throws -> WKContentRuleList {
        if let compilation { return try await compilation.value }
        let task = Task { @MainActor in
            guard let rules = try await WKContentRuleListStore.default().compileContentRuleList(
                forIdentifier: "SweetbriarReader-v1", encodedContentRuleList: source
            ) else { throw URLError(.cannotLoadFromNetwork) }
            return rules
        }
        compilation = task
        return try await task.value
    }
}

@MainActor
final class ReaderCIDHandler: NSObject, WKURLSchemeHandler {
    private struct Pending {
        let task: any WKURLSchemeTask
        let ticket: UInt64
    }
    private var pending: [ObjectIdentifier: Pending] = [:]
    private var resolver: CIDResourceResolver?
    private var ticket: UInt64 = 0
    private let currentTicket: () -> UInt64

    init(currentTicket: @escaping () -> UInt64) {
        self.currentTicket = currentTicket
    }

    func replace(with rendition: HTMLRendition?, ticket: UInt64) {
        // Revoke before answering anything: a callback can reenter WebKit.
        let obsolete = pending.values
        pending.removeAll()
        self.ticket = ticket
        resolver = rendition.map { CIDResourceResolver(rendition: $0, ticket: ticket) }
        for item in obsolete { item.task.didFailWithError(URLError(.cancelled)) }
    }

    func webView(_ webView: WKWebView, start urlSchemeTask: any WKURLSchemeTask) {
        guard let url = urlSchemeTask.request.url,
              let resource = resolver?.resolve(url, currentTicket: currentTicket()) else {
            urlSchemeTask.didFailWithError(URLError(.noPermissionsToReadFile))
            return
        }
        let id = ObjectIdentifier(urlSchemeTask)
        let requestedTicket = ticket
        pending[id] = Pending(task: urlSchemeTask, ticket: requestedTicket)
        Task { @MainActor [weak self] in
            let result = await Task.detached(priority: .userInitiated) {
                Result { try Data(contentsOf: resource.fileURL) }
            }.value
            // Retain task identity across I/O, even if WebKit stops it meanwhile.
            guard let self, let item = self.pending[id], item.task === urlSchemeTask else { return }
            guard self.activeTask(id) != nil else { return }
            switch result {
            case .success(let data):
                let response = URLResponse(
                    url: url, mimeType: resource.mediaType,
                    expectedContentLength: data.count, textEncodingName: nil
                )
                item.task.didReceive(response)
                guard self.activeTask(id) != nil else { return }
                item.task.didReceive(data)
                guard self.activeTask(id) != nil else { return }
                self.pending.removeValue(forKey: id)
                item.task.didFinish()
            case .failure(let error):
                self.pending.removeValue(forKey: id)
                item.task.didFailWithError(error)
            }
        }
    }

    private func activeTask(_ id: ObjectIdentifier) -> Pending? {
        guard let item = pending[id] else { return nil }
        guard item.ticket == ticket, item.ticket == currentTicket() else {
            pending.removeValue(forKey: id)
            item.task.didFailWithError(URLError(.cancelled))
            return nil
        }
        return item
    }

    func webView(_ webView: WKWebView, stop urlSchemeTask: any WKURLSchemeTask) {
        pending.removeValue(forKey: ObjectIdentifier(urlSchemeTask))
    }
}

/// Not installed in the main window until M1.5n. The caller supplies the
/// model's live ticket so invalidation is effective even before the next show.
@MainActor
final class HTMLReaderView: NSView, WKNavigationDelegate, WKUIDelegate {
    let webView: WKWebView
    private let cidHandler: ReaderCIDHandler
    private let currentTicket: () -> UInt64
    private var ticket: UInt64 = 0
    private var bodyURL: URL?
    private var initialNavigationPending = false
    private var navigation: WKNavigation?
    private let compileRules: @MainActor () async throws -> WKContentRuleList

    var linkActivated: ((URL) -> Void)?
    var ruleListUnavailable: ((UInt64) -> Void)?
    var loadFailed: ((UInt64, any Error) -> Void)?

    init(
        currentTicket: @escaping () -> UInt64,
        compileRules: @escaping @MainActor () async throws -> WKContentRuleList = { try await ReaderContentRules.compiled() }
    ) {
        self.currentTicket = currentTicket
        self.compileRules = compileRules
        cidHandler = ReaderCIDHandler(currentTicket: currentTicket)
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .nonPersistent()
        configuration.defaultWebpagePreferences.allowsContentJavaScript = false
        configuration.userContentController = WKUserContentController() // no scripts
        configuration.setURLSchemeHandler(cidHandler, forURLScheme: "sweetbriar-cid")
        webView = WKWebView(frame: .zero, configuration: configuration)
        super.init(frame: .zero)
        webView.navigationDelegate = self
        webView.uiDelegate = self
        webView.allowsBackForwardNavigationGestures = false
        webView.translatesAutoresizingMaskIntoConstraints = false
        webView.isHidden = true
        addSubview(webView)
        NSLayoutConstraint.activate([
            webView.leadingAnchor.constraint(equalTo: leadingAnchor),
            webView.trailingAnchor.constraint(equalTo: trailingAnchor),
            webView.topAnchor.constraint(equalTo: topAnchor),
            webView.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])
    }

    required init?(coder: NSCoder) { fatalError("HTMLReaderView is built in code") }

    func clear(ticket: UInt64) {
        guard ticket == currentTicket() else { return }
        self.ticket = ticket
        bodyURL = nil
        navigation = nil
        initialNavigationPending = false
        webView.isHidden = true
        webView.stopLoading()
        cidHandler.replace(with: nil, ticket: ticket)
    }

    func show(_ rendition: HTMLRendition, ticket: UInt64) async {
        guard ticket == currentTicket() else { return }
        clear(ticket: ticket)
        let rules: WKContentRuleList
        do {
            rules = try await compileRules()
        } catch {
            guard self.ticket == ticket, ticket == currentTicket() else { return }
            ruleListUnavailable?(ticket)
            return
        }
        guard self.ticket == ticket, ticket == currentTicket() else { return }
        do {
            let html = try await Task.detached(priority: .userInitiated) {
                String(decoding: try Data(contentsOf: rendition.htmlFile), as: UTF8.self)
            }.value
            guard self.ticket == ticket, ticket == currentTicket() else { return }
            // Substitute data needs no file: exemption or WebKit file access.
            // The base is unique per selection; the body is supplied directly.
            var base = URLComponents()
            base.scheme = "sweetbriar-cid"
            base.host = rendition.token
            base.path = "/__body_\(ticket)"
            guard let url = base.url else { throw URLError(.badURL) }
            webView.configuration.userContentController.add(rules)
            cidHandler.replace(with: rendition, ticket: ticket)
            bodyURL = url
            initialNavigationPending = true
            navigation = webView.loadHTMLString(html, baseURL: url)
        } catch {
            guard self.ticket == ticket, ticket == currentTicket() else { return }
            loadFailed?(ticket, error)
        }
    }

    /// Shared by the delegate and tests. Admit the exact initial body once;
    /// clicks are reported but never admitted as a WebKit navigation.
    func policy(
        url: URL?, sourceURL: URL?, sourceIsMainFrame: Bool, targetIsMainFrame: Bool?,
        type: WKNavigationType, shouldPerformDownload: Bool
    ) -> WKNavigationActionPolicy {
        guard ticket == currentTicket(), let bodyURL, let url, !shouldPerformDownload else { return .cancel }
        if initialNavigationPending, type == .other, targetIsMainFrame == true, url == bodyURL {
            initialNavigationPending = false
            return .allow
        }
        if type == .linkActivated, sourceIsMainFrame, sourceURL == bodyURL,
           targetIsMainFrame == true || targetIsMainFrame == nil,
           ["http", "https", "mailto"].contains(url.scheme?.lowercased() ?? "") {
            linkActivated?(url)
        }
        return .cancel
    }

    func webView(
        _ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction,
        decisionHandler: @escaping @MainActor @Sendable (WKNavigationActionPolicy) -> Void
    ) {
        decisionHandler(policy(
            url: navigationAction.request.url, sourceURL: navigationAction.sourceFrame.request.url,
            sourceIsMainFrame: navigationAction.sourceFrame.isMainFrame,
            targetIsMainFrame: navigationAction.targetFrame?.isMainFrame,
            type: navigationAction.navigationType, shouldPerformDownload: navigationAction.shouldPerformDownload
        ))
    }

    func webView(
        _ webView: WKWebView, decidePolicyFor navigationResponse: WKNavigationResponse,
        decisionHandler: @escaping @MainActor @Sendable (WKNavigationResponsePolicy) -> Void
    ) {
        let allowed = ticket == currentTicket() && navigationResponse.isForMainFrame &&
            navigationResponse.response.url == bodyURL && navigationResponse.canShowMIMEType &&
            navigationResponse.response.mimeType == "text/html"
        decisionHandler(allowed ? .allow : .cancel)
    }

    func webView(_ webView: WKWebView, didReceiveServerRedirectForProvisionalNavigation navigation: WKNavigation!) {
        guard isCurrent(navigation) else { return }
        webView.stopLoading()
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        guard isCurrent(navigation) else { return }
        webView.isHidden = false
    }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: any Error) {
        report(error, navigation: navigation)
    }

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: any Error) {
        report(error, navigation: navigation)
    }

    private func isCurrent(_ candidate: WKNavigation?) -> Bool {
        ticket == currentTicket() && candidate != nil && candidate === navigation
    }

    private func report(_ error: any Error, navigation: WKNavigation?) {
        guard isCurrent(navigation) else { return }
        webView.isHidden = true
        loadFailed?(ticket, error)
    }

    // target=_blank is reported by the action policy and never creates a view.
    func webView(
        _ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration,
        for navigationAction: WKNavigationAction, windowFeatures: WKWindowFeatures
    ) -> WKWebView? { nil }
}
