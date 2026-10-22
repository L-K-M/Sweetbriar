import SweetbriarCore

/// Raw query JSON and outer constraints; the core validates them when opening.
public struct SearchSpec: Sendable, Equatable, Hashable {
    public var queryJSON: String
    public var scope: SearchScope
    public var sort: SearchSort

    public init(queryJSON: String, scope: SearchScope, sort: SearchSort) {
        self.queryJSON = queryJSON
        self.scope = scope
        self.sort = sort
    }

    // Swift String equality merges Unicode spellings that exact local parts keep distinct.
    public static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.scope == rhs.scope && lhs.sort == rhs.sort &&
            lhs.queryJSON.utf8.elementsEqual(rhs.queryJSON.utf8)
    }

    public func hash(into hasher: inout Hasher) {
        hasher.combine(scope)
        hasher.combine(sort)
        for byte in queryJSON.utf8 { hasher.combine(byte) }
    }

    var generated: SweetbriarCore.SearchSpec {
        .init(queryJson: queryJSON, scope: scope.generated, sort: sort.generated)
    }
}

/// Date descending or native relevance ascending, with stable occurrence ties.
public enum SearchSort: Sendable, Equatable, Hashable {
    case date, relevance

    var generated: SweetbriarCore.SearchSort {
        switch self {
        case .date: return .date
        case .relevance: return .relevance
        }
    }
}

/// Facts from the same native read snapshot as the Search page's rows.
public struct SearchStatus: Sendable, Equatable {
    public var coverage: SearchCoverage
    public var excludedUnknown: UInt64

    public init(coverage: SearchCoverage, excludedUnknown: UInt64) {
        self.coverage = coverage
        self.excludedUnknown = excludedUnknown
    }

    init(_ value: SweetbriarCore.SearchStatus) {
        self.init(coverage: SearchCoverage(value.coverage), excludedUnknown: value.excludedUnknown)
    }
}
