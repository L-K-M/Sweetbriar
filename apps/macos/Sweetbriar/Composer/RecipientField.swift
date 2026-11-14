import AppKit
import SweetbriarBridge

/// AppKit may hold an unfinished mailbox list beside its native token
/// attachments. Keep that text as a represented object until native parsing
/// accepts the whole list; no synchronous delegate invents mailbox syntax.
private final class RecipientToken: NSObject {
    enum Value: Sendable {
        case address(DraftRecipient)
        case input(String)
    }
    let value: Value
    init(_ value: Value) { self.value = value }
    @MainActor var text: String {
        switch value {
        case .address(let recipient): return RecipientField.text(recipient)
        case .input(let text): return text
        }
    }

    // AppKit repeatedly validates represented objects while measuring its
    // intrinsic size. Identity-only equality creates new values every pass
    // and can make native constraint layout loop indefinitely.
    override func isEqual(_ object: Any?) -> Bool {
        guard let other = object as? RecipientToken else { return false }
        switch (value, other.value) {
        case (.address(let a), .address(let b)): return RecipientField.sameBytes([a], [b])
        case (.input(let a), .input(let b)): return a.utf8.elementsEqual(b.utf8)
        default: return false
        }
    }

    override var hash: Int {
        var hash = Hasher()
        switch value {
        case .input(let text):
            hash.combine(0)
            for byte in text.utf8 { hash.combine(byte) }
        case .address(let recipient):
            hash.combine(1)
            for byte in recipient.address.utf8 { hash.combine(byte) }
            hash.combine(recipient.name != nil)
            if let name = recipient.name { for byte in name.utf8 { hash.combine(byte) } }
        }
        return hash.finalize()
    }
}

@MainActor
final class RecipientField: NSObject, NSTokenFieldDelegate {
    private static let maximumCompletionPrefixBytes = 1_024
    let control = NSTokenField()
    var inputDidChange: (() -> Void)?
    var editingDidEnd: (() -> Void)?
    var presentationDidChange: (() -> Void)?
    private(set) var isPending = false
    private(set) var problem: String?
    private var values: [DraftRecipient] = []
    private let source: any RecipientSource
    private var epoch: UInt64 = 0
    private var publishing = false
    private var closed = false
    private var completion: Task<Void, Never>?
    private var validatedInputs: [Data: [DraftRecipient]] = [:]
    private var completionId = UUID()
    private var completionKey: Data?
    private var requestedCompletionKey: Data?
    private var completionStrings: [String] = []

    init(label: String, source: any RecipientSource) {
        self.source = source
        super.init()
        control.delegate = self
        // A comma inside a quoted name is not a token delimiter. Native
        // validation expands complete comma/semicolon lists asynchronously.
        control.tokenizingCharacterSet = CharacterSet()
        control.tokenStyle = .rounded
        control.completionDelay = 0.15
        publishing = true
        control.objectValue = [RecipientToken]()
        publishing = false
        control.setAccessibilityLabel(label)
        control.setAccessibilityIdentifier("compose\(label)")
    }

    var inputVersion: UInt64 { epoch }
    var hasMarkedText: Bool { (control.currentEditor() as? NSTextView)?.hasMarkedText() == true }

    func load(_ recipients: [DraftRecipient]) {
        guard !isPending, control.currentEditor() == nil, !Self.sameBytes(values, recipients) else { return }
        values = recipients
        publish(recipients)
    }

    /// The scheduler owns this preparation and freezes the save snapshot only
    /// after it succeeds. A stale answer never replaces newer native input.
    func prepare() async -> [DraftRecipient]? {
        guard !closed else { return nil }
        guard !hasMarkedText else {
            problem = "Finish composing the recipient before saving."
            return nil
        }
        guard isPending else { return values }
        publishing = true
        control.validateEditing()
        let rawObjects = control.objectValue
        publishing = false
        let tokens: [RecipientToken]
        if rawObjects == nil && control.stringValue.isEmpty {
            tokens = []
        } else if let objects = rawObjects as? [RecipientToken] {
            tokens = objects
        } else {
            problem = "Recipient input could not be read. Keep editing or explicitly close without saving."
            return nil
        }
        let ticket = epoch
        do {
            var recipients: [DraftRecipient] = []
            var parsed: [Data: [DraftRecipient]] = [:]
            for token in tokens {
                switch token.value {
                case .address(let recipient): recipients.append(recipient)
                case .input(let text):
                    let answer = try await source.parseDraftRecipients(text: text)
                    parsed[Data(text.utf8)] = answer
                    recipients += answer
                }
            }
            guard !closed, ticket == epoch, !hasMarkedText else { return nil }
            values = recipients
            validatedInputs = parsed
            isPending = false
            problem = nil
            if !parsed.isEmpty {
                if let editor = control.currentEditor() as? NSTextView, control.isEditable {
                    // Let AppKit perform its own undoable token insertion.
                    // Assigning objectValue here erases native typing history.
                    publishing = true
                    editor.doCommand(by: #selector(NSResponder.insertNewline(_:)))
                    publishing = false
                } else {
                    publish(recipients)
                }
            }
            return recipients
        } catch {
            guard !closed, ticket == epoch else { return nil }
            problem = String(describing: error)
            return nil
        }
    }

    func stopAndWait() async {
        closed = true
        epoch &+= 1
        completion?.cancel()
        await completion?.value
        completion = nil
    }

    private func publish(_ recipients: [DraftRecipient]) {
        publishing = true
        control.objectValue = recipients.map { RecipientToken(.address($0)) }
        publishing = false
    }

    private func changed() {
        guard !publishing, !closed else { return }
        epoch &+= 1
        isPending = true
        problem = nil
        validatedInputs = [:]
        completionKey = nil
        requestedCompletionKey = nil
        completion?.cancel()
        inputDidChange?()
    }

    func controlTextDidChange(_ notification: Notification) { changed() }
    func controlTextDidEndEditing(_ notification: Notification) { editingDidEnd?() }

    func tokenField(_ tokenField: NSTokenField, representedObjectForEditing editingString: String) -> Any? {
        RecipientToken(.input(editingString))
    }

    func tokenField(_ tokenField: NSTokenField, shouldAdd tokens: [Any], at index: Int) -> [Any] {
        let accepted = tokens.flatMap { object -> [Any] in
            guard let token = object as? RecipientToken, case .input(let text) = token.value,
                let recipients = validatedInputs[Data(text.utf8)]
            else { return [object] }
            return recipients.map { RecipientToken(.address($0)) }
        }
        changed()
        return accepted
    }

    func tokenField(_ tokenField: NSTokenField, displayStringForRepresentedObject representedObject: Any)
        -> String?
    {
        (representedObject as? RecipientToken)?.text
    }

    func tokenField(_ tokenField: NSTokenField, editingStringForRepresentedObject representedObject: Any)
        -> String?
    {
        (representedObject as? RecipientToken)?.text
    }

    func tokenField(
        _ tokenField: NSTokenField, writeRepresentedObjects objects: [Any], to pasteboard: NSPasteboard
    ) -> Bool {
        guard let tokens = objects as? [RecipientToken] else { return false }
        pasteboard.clearContents()
        return pasteboard.setString(tokens.map(\.text).joined(separator: "; "), forType: .string)
    }

    func tokenField(_ tokenField: NSTokenField, readFrom pasteboard: NSPasteboard) -> [Any]? {
        guard let text = pasteboard.string(forType: .string) else { return nil }
        return [RecipientToken(.input(text))]
    }

    func tokenField(
        _ tokenField: NSTokenField, completionsForSubstring substring: String,
        indexOfToken tokenIndex: Int, indexOfSelectedItem selectedIndex: UnsafeMutablePointer<Int>?
    ) -> [Any]? {
        selectedIndex?.pointee = -1
        guard !closed else { return [] }
        guard substring.utf8.count <= Self.maximumCompletionPrefixBytes else {
            problem = "Address suggestions require at most 1,024 UTF-8 bytes."
            presentationDidChange?()
            return []
        }
        let key = Data(substring.utf8)
        if completionKey == key { return completionStrings }
        if requestedCompletionKey == key { return [] }
        requestedCompletionKey = key
        let ticket = epoch
        let id = UUID()
        let previous = completion
        previous?.cancel()
        completionId = id
        let source = source
        completion = Task { [weak self] in
            // Cancelling an old query does not abandon its native waiter.
            await previous?.value
            guard !Task.isCancelled else { return }
            do {
                let recipients = try await source.recipientCompletions(prefix: substring, limit: 20)
                guard let self, !self.closed, !Task.isCancelled,
                    self.epoch == ticket, self.completionId == id
                else { return }
                self.completionKey = key
                self.completionStrings = recipients.map(Self.text)
                self.presentationDidChange?()
                (self.control.currentEditor() as? NSTextView)?.complete(nil)
            } catch {
                guard let self, !self.closed, self.epoch == ticket, self.completionId == id,
                    !Task.isCancelled
                else { return }
                self.problem = "Address suggestions unavailable: \(error)"
                self.completionKey = key
                self.completionStrings = []
                self.presentationDidChange?()
            }
        }
        return []
    }

    static func text(_ recipient: DraftRecipient) -> String {
        guard let name = recipient.name else { return recipient.address }
        let quoted = name.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(
            of: "\"", with: "\\\"")
        return "\"\(quoted)\" <\(recipient.address)>"
    }

    nonisolated static func sameBytes(_ lhs: [DraftRecipient], _ rhs: [DraftRecipient]) -> Bool {
        guard lhs.count == rhs.count else { return false }
        return zip(lhs, rhs).allSatisfy { a, b in
            guard a.address.utf8.elementsEqual(b.address.utf8) else { return false }
            switch (a.name, b.name) {
            case (.none, .none): return true
            case (.some(let x), .some(let y)): return x.utf8.elementsEqual(y.utf8)
            default: return false
            }
        }
    }
}
