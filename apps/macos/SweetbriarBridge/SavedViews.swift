// Saved views over the bridge (spec/07 Views, spec/08 Sidebar): the
// rows, the upsert parameters and their mapping to the generated types.

import Foundation
import SweetbriarCore

/// A saved view's stored message order (spec/02 `saved_views.sort`).
public enum SavedViewSort: Sendable, Equatable {
    case dateDesc
    /// Reserved: saving it is `InvalidInput` on `sort` (spec/07 Views).
    case dateAsc
    case relevance

    /// Map the generated enum case by case.
    init(_ value: SweetbriarCore.SavedViewSort) {
        switch value {
        case .dateDesc: self = .dateDesc
        case .dateAsc: self = .dateAsc
        case .relevance: self = .relevance
        }
    }

    var generated: SweetbriarCore.SavedViewSort {
        switch self {
        case .dateDesc: return .dateDesc
        case .dateAsc: return .dateAsc
        case .relevance: return .relevance
        }
    }
}

/// One saved view as `saved_views()` reports it, in sidebar order.
public struct SavedView: Sendable, Equatable {
    public var viewId: String
    public var name: String
    /// The canonical v1 query AST JSON (spec/06).
    public var queryJson: String
    public var sort: SavedViewSort
    public var revision: Int64

    public init(viewId: String, name: String, queryJson: String, sort: SavedViewSort, revision: Int64) {
        self.viewId = viewId
        self.name = name
        self.queryJson = queryJson
        self.sort = sort
        self.revision = revision
    }

    /// Map the generated record field by field.
    init(_ value: SweetbriarCore.SavedView) {
        self.init(
            viewId: value.viewId,
            name: value.name,
            queryJson: value.queryJson,
            sort: SavedViewSort(value.sort),
            revision: value.revision
        )
    }
}

/// `upsert_saved_view`'s parameters (spec/07 Views): the view to update,
/// or none to create one.
public struct SavedViewParams: Sendable, Equatable {
    public var viewId: String?
    public var name: String
    public var queryJson: String
    public var sort: SavedViewSort

    public init(viewId: String?, name: String, queryJson: String, sort: SavedViewSort) {
        self.viewId = viewId
        self.name = name
        self.queryJson = queryJson
        self.sort = sort
    }

    var generated: SweetbriarCore.SavedViewParams {
        SweetbriarCore.SavedViewParams(viewId: viewId, name: name, queryJson: queryJson, sort: sort.generated)
    }
}

extension Receipt {
    /// The stored id a saved-view receipt names (spec/07 Views).
    public var savedViewID: String? {
        guard let data = resultJson.data(using: .utf8),
            let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
            let id = object["view_id"] as? String
        else { return nil }
        return id
    }
}
