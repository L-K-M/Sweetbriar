import Foundation
import Observation

public protocol SearchParsing: Sendable {
    func parseSearch(text: String, context: ParseContext) async throws -> String
    func formatSearch(queryJSON: String) async throws -> String
}

extension CoreBridge: SearchParsing {}

public struct SearchClause: Identifiable, Sendable, Equatable {
    public let id: UUID
    public let text: String
    public let label: String
    public let isChip: Bool
    public let range: NSRange?
}

public enum SearchComposition: Sendable, Equatable { case committed, marked }
public enum SearchClauseMove: Sendable, Equatable { case earlier, later }

/// Committed input owns one cancellable parser task; accepted windows have their
/// own lifetime. Invalid edits keep the preceding valid query and its results.
@MainActor @Observable
public final class SearchFieldModel {
    public private(set) var text = ""
    public private(set) var scope: SearchScope = .all
    public private(set) var sort: SearchSort = .relevance
    public private(set) var clauses: [SearchClause] = []
    public private(set) var queryJSON: String?
    public private(set) var lastValidText = ""
    public private(set) var failure: CoreFailure?
    public private(set) var errorRange: NSRange?
    public private(set) var isParsing = false
    private var marked = false
    private var acceptedText = ""
    private var canonicalText = ""
    private var parts: SearchQuery?
    private var previous: WindowSpec = .unified(.inbox)
    private var serial: UInt64 = 0
    private var closed = false

    @ObservationIgnored public var onPresent: (@MainActor (WindowSpec, UInt64) -> Void)?
    @ObservationIgnored public var hasMarkedText: @MainActor () -> Bool = { false }
    // Lifecycle tests join superseded work instead of guessing when it finished.
    @ObservationIgnored private(set) var pending: Task<Void, Never>?
    @ObservationIgnored private var operation: Input?
    private let parser: any SearchParsing
    private let context: @MainActor () -> ParseContext
    private let label: @MainActor (SearchClauseReference) -> String?
    private let debounce: Duration

    private enum Input {
        case text(String, ParseContext)
        case json(String)
    }

    public init(
        parser: any SearchParsing, debounce: Duration = .milliseconds(30),
        context: @escaping @MainActor () -> ParseContext = {
            ParseContext(tz: TimeZone.autoupdatingCurrent.identifier,
                         clock: Int64(Date().timeIntervalSince1970 * 1_000))
        },
        label: @escaping @MainActor (SearchClauseReference) -> String? = { _ in nil }
    ) {
        self.parser = parser
        self.debounce = debounce
        self.context = context
        self.label = label
    }

    deinit { pending?.cancel() }

    public var isComposing: Bool { marked || hasMarkedText() }
    public var canEditClauses: Bool {
        !closed && !isParsing && !isComposing && failure == nil && queryJSON != nil &&
            text.utf8.elementsEqual(acceptedText.utf8)
    }

    public func isCurrent(_ ticket: UInt64) -> Bool { !closed && serial == ticket }

    public func updateText(_ value: String, composition: SearchComposition = .committed) {
        guard !closed else { return }
        let ticket = invalidate()
        text = value
        marked = composition == .marked
        failure = nil
        errorRange = nil
        guard !isComposing else { return }
        if value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            let restore = queryJSON != nil
            reset()
            if restore { onPresent?(previous, ticket) }
            return
        }
        launch(.text(value, context()), ticket: ticket, delay: debounce)
    }

    public func clear() { updateText("") }

    public func chooseScope(_ value: SearchScope) {
        guard scope != value, !closed else { return }
        scope = value
        constraintsChanged()
    }

    public func chooseSort(_ value: SearchSort) {
        guard sort != value, !closed else { return }
        sort = value
        constraintsChanged()
    }

    /// External list navigation retires field work before a late parse can show.
    public func navigated(to spec: WindowSpec) {
        if case .search = spec { return }
        previous = spec
        _ = invalidate()
        reset()
    }

    public func close() {
        _ = invalidate()
        closed = true
        onPresent = nil
    }

    /// Editing selects the clause in core-formatted text, retaining group/context.
    public func selection(for id: UUID) -> (text: String, range: NSRange)? {
        guard canEditClauses, let clause = clauses.first(where: { $0.id == id }),
              let range = clause.range else { return nil }
        return (canonicalText, range)
    }

    public func removeClause(_ id: UUID) {
        guard canEditClauses, var parts, let index = clauses.firstIndex(where: { $0.id == id }) else { return }
        parts.nodes.remove(at: index)
        if parts.nodes.isEmpty { clear(); return }
        change(parts)
    }

    public func moveClause(_ id: UUID, toward direction: SearchClauseMove) {
        guard canEditClauses, var parts, let index = clauses.firstIndex(where: { $0.id == id }) else { return }
        let destination = index + (direction == .earlier ? -1 : 1)
        guard parts.nodes.indices.contains(destination) else { return }
        parts.nodes.swapAt(index, destination)
        change(parts)
    }

    private func invalidate() -> UInt64 {
        serial += 1
        pending?.cancel()
        pending = nil
        operation = nil
        isParsing = false
        return serial
    }

    private func reset() {
        text = ""
        acceptedText = ""
        canonicalText = ""
        queryJSON = nil
        parts = nil
        clauses = []
        failure = nil
        errorRange = nil
        marked = false
    }

    private func constraintsChanged() {
        let interrupted = operation
        let ticket = invalidate()
        if let queryJSON { present(queryJSON, ticket: ticket) }
        if let interrupted, !isComposing { launch(interrupted, ticket: ticket, delay: .zero) }
    }

    private func change(_ parts: SearchQuery) {
        do {
            let json = try SearchQuery.encode(parts.nodes)
            launch(.json(json), ticket: invalidate(), delay: .zero)
        } catch {
            failure = .internalError(detail: error.localizedDescription)
        }
    }

    private func present(_ json: String, ticket: UInt64) {
        onPresent?(.search(SearchSpec(queryJSON: json, scope: scope, sort: sort)), ticket)
    }

    private func check(_ ticket: UInt64) throws {
        try Task.checkCancellation()
        guard isCurrent(ticket), !isComposing else { throw CancellationError() }
    }

    private func launch(_ input: Input, ticket: UInt64, delay: Duration) {
        operation = input
        isParsing = true
        pending = Task { @MainActor [weak self] in
            guard let self else { return }
            defer {
                if self.isCurrent(ticket) {
                    self.pending = nil
                    self.operation = nil
                    self.isParsing = false
                }
            }
            var spanSource: String?
            do {
                try await Task.sleep(for: delay)
                try self.check(ticket)
                let source: String
                let json: String
                switch input {
                case .text(let value, let context):
                    source = value
                    spanSource = value
                    json = try await self.parser.parseSearch(text: value, context: context)
                    spanSource = nil
                case .json(let value):
                    source = try await self.parser.formatSearch(queryJSON: value)
                    try self.check(ticket)
                    json = try await self.parser.parseSearch(text: source, context: self.context())
                }
                try self.check(ticket)
                let whole = try await self.parser.formatSearch(queryJSON: json)
                try self.check(ticket)
                let parts = try SearchQuery(json)
                var clauses: [SearchClause] = []
                var remaining = whole.startIndex..<whole.endIndex
                for node in parts.nodes {
                    let rendered = try await self.parser.formatSearch(queryJSON: SearchQuery.encode([node]))
                    try self.check(ticket)
                    let range = whole.range(of: rendered, options: .literal, range: remaining)
                    if let range { remaining = range.upperBound..<whole.endIndex }
                    let resolved = SearchQuery.reference(node).flatMap(self.label)
                    clauses.append(SearchClause(
                        id: UUID(), text: rendered, label: resolved ?? rendered,
                        isChip: SearchQuery.isChip(node), range: range.map { NSRange($0, in: whole) }
                    ))
                }
                try self.check(ticket)
                // Do not rewrite live input after a successful text parse.
                self.text = source
                self.acceptedText = source
                self.lastValidText = source
                self.canonicalText = whole
                self.queryJSON = json
                self.parts = parts
                self.clauses = clauses
                self.failure = nil
                self.errorRange = nil
                self.present(json, ticket: ticket)
            } catch is CancellationError {
                // A newer field/list choice owns presentation.
            } catch {
                guard self.isCurrent(ticket), !self.isComposing else { return }
                let failure = error as? CoreFailure ?? .internalError(detail: error.localizedDescription)
                self.failure = failure
                if let spanSource, case .searchParse(let start, let end, _) = failure {
                    self.errorRange = SearchParseSpan.utf16Range(start: start, end: end, in: spanSource)
                } else {
                    self.errorRange = nil
                }
            }
        }
    }
}
