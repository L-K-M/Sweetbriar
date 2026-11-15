// AppKit window restoration for compose windows (spec/08 "Windows"). A
// window encodes only its draft id. At launch AppKit asks this class
// for each saved window before the app has built its compose owner, so
// requests wait in `ComposeRestorer` until `attach(_:)`. A refusal
// (a draft that is gone, an archive that can't open, quit or restore)
// restores nothing and reports nothing.

import AppKit

final class ComposeRestoration: NSObject, NSWindowRestoration {
    static let windowIdentifier = NSUserInterfaceItemIdentifier("compose")
    private static let draftIdKey = "draftId"

    static func encode(draftId: String, into state: NSCoder) {
        state.encode(draftId as NSString, forKey: draftIdKey)
    }

    static func draftId(from state: NSCoder) -> String? {
        state.decodeObject(of: NSString.self, forKey: draftIdKey) as String?
    }

    static func restoreWindow(
        withIdentifier identifier: NSUserInterfaceItemIdentifier, state: NSCoder,
        completionHandler: @escaping (NSWindow?, (any Error)?) -> Void
    ) {
        let draftId = identifier == windowIdentifier ? draftId(from: state) : nil
        MainActor.assumeIsolated {
            ComposeRestorer.shared.restore(draftId: draftId, completion: completionHandler)
        }
    }
}

/// Why a compose window wasn't restored. Never shown.
struct ComposeRestorationRefused: Error {}

@MainActor
final class ComposeRestorer {
    typealias Completion = (NSWindow?, (any Error)?) -> Void

    static let shared = ComposeRestorer()

    private var owner: ComposeWindows?
    private var waiting: [(draftId: String, completion: Completion)] = []

    func restore(draftId: String?, completion: @escaping Completion) {
        guard let draftId, !draftId.isEmpty else {
            completion(nil, ComposeRestorationRefused())
            return
        }
        guard let owner else {
            waiting.append((draftId, completion))
            return
        }
        Task {
            let window = await owner.restore(draftId: draftId)
            completion(window, window == nil ? ComposeRestorationRefused() : nil)
        }
    }

    /// The compose owner exists: serve the requests that waited for it.
    func attach(_ owner: ComposeWindows) {
        self.owner = owner
        let waiting = self.waiting
        self.waiting = []
        for request in waiting { restore(draftId: request.draftId, completion: request.completion) }
    }
}
