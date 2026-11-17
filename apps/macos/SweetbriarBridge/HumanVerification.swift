// Human verification's web view rules (plan M2.14b; spec/10 "The
// Proton API", spec/08 "Proton sign-in"), free of WebKit so the bridge
// tests exercise them. The app's web view asks `allows` for every
// navigation and hands each page message to `parse`.
//
// Proton's verify page (WebClients `applications/verify`) reports
// through `broadcast`: outside Proton's own apps it posts a JSON string
// to `window.parent`, which for a top-level page is the page itself.
// The CAPTCHA it frames lives on `verify-api.proton.me`
// (`components/.../humanVerification/Captcha.tsx`) and posts its answer
// to the verify page, never to us.

import Foundation

public enum HumanVerificationPolicy {
    /// The verification page's host, the only main-frame host.
    public static let pageHost = "verify.proton.me"

    /// Hosts a frame inside the page may load: the page itself and
    /// Proton's CAPTCHA. The owner's qualification (M2.Q) confirms the
    /// list against a live challenge.
    public static let frameHosts: Set<String> = [pageHost, "verify-api.proton.me"]

    /// The name the page script posts to (`window.webkit.messageHandlers`).
    public static let messageHandlerName = "sweetbriarVerification"

    /// Forwards the page's own `postMessage`s, and only those, to the
    /// handler; injected into the main frame at document start.
    public static let pageScript = """
        window.addEventListener('message', function (event) {
          if (event.source !== window || event.origin !== 'https://\(pageHost)') { return; }
          var data = typeof event.data === 'string' ? event.data : JSON.stringify(event.data);
          window.webkit.messageHandlers.\(messageHandlerName).postMessage(data);
        });
        """

    /// The web view's content rules: every load is blocked, then the
    /// allowed hosts and the page's own inline documents and data pass,
    /// so scripts, images and requests reach no other host either.
    /// WebKit's rule regexes have no alternation: one rule per prefix.
    public static var contentRules: String {
        let passes = frameHosts.sorted().map { "^https://" + $0.replacingOccurrences(of: ".", with: "\\.") + "/" }
            + ["^about:", "^data:", "^blob:"]
        let rules: [[String: Any]] = [["trigger": ["url-filter": ".*"], "action": ["type": "block"]]]
            + passes.map { ["trigger": ["url-filter": $0], "action": ["type": "ignore-previous-rules"]] }
        let data = try! JSONSerialization.data(withJSONObject: rules, options: [.sortedKeys])
        return String(decoding: data, as: UTF8.self)
    }

    /// The page Proton named, or `nil` when it isn't an https URL on
    /// the verification host: the sheet then refuses to open it.
    public static func startURL(_ string: String) -> URL? {
        guard let url = URL(string: string), allows(url, mainFrame: true) else { return nil }
        return url
    }

    /// Whether a navigation may load. The main frame stays on the
    /// verification page; frames may load Proton's CAPTCHA, or be
    /// empty documents the page builds itself.
    public static func allows(_ url: URL, mainFrame: Bool) -> Bool {
        if !mainFrame, ["about:blank", "about:srcdoc"].contains(url.absoluteString) {
            return true
        }
        guard url.scheme == "https", url.user == nil, url.password == nil, url.port == nil,
              let host = url.host?.lowercased()
        else { return false }
        return mainFrame ? host == pageHost : frameHosts.contains(host)
    }
}

/// One message from the verification page.
public enum HumanVerificationMessage: Sendable, Equatable {
    /// Proton handed back the token: the sheet closes and the
    /// conversation continues with it.
    case verified(token: String, tokenType: String)
    /// The page asked to close.
    case closed
    /// The page reported an error it shows itself.
    case failed(String)

    /// A page message (a JSON string), or `nil` for one the sheet
    /// ignores: resizes, notifications, load events and anything
    /// malformed.
    public static func parse(_ body: Any) -> HumanVerificationMessage? {
        guard let text = body as? String, let data = text.data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let type = object["type"] as? String
        else { return nil }
        let payload = object["payload"] as? [String: Any]
        switch type {
        case "HUMAN_VERIFICATION_SUCCESS":
            guard let token = payload?["token"] as? String, !token.isEmpty,
                  let tokenType = payload?["type"] as? String, !tokenType.isEmpty
            else { return nil }
            return .verified(token: token, tokenType: tokenType)
        case "CLOSE":
            return .closed
        case "ERROR":
            return .failed(payload?["message"] as? String ?? "The verification page reported an error.")
        default:
            return nil
        }
    }
}

extension HumanVerificationMessage: CustomStringConvertible, CustomDebugStringConvertible {
    /// Never the token (spec/10).
    public var description: String {
        switch self {
        case .verified(_, let tokenType):
            return "verified([redacted], \(tokenType))"
        case .closed:
            return "closed"
        case .failed(let message):
            return "failed(\(message))"
        }
    }

    public var debugDescription: String { description }
}
