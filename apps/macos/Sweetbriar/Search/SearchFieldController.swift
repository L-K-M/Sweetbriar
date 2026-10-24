// Search entry and AST chips (spec/08). The native editor keeps live text
// and composition; the bridge owns parsing and the parent owns windows.

import AppKit
import Observation
import SweetbriarBridge

@MainActor
final class SearchFieldController: NSViewController {
    private let model: SearchFieldModel
    /// Resolved late (the sidebar may still be loading): re-read on
    /// every model change and on menu open.
    private let scopes: @MainActor () -> [(label: String, scope: SearchScope)]

    private let field = NSSearchField()
    private let scopePopup = NSPopUpButton()
    private let sortPopup = NSPopUpButton()
    private let chipScroll = NSScrollView()
    private let chipRow = NSStackView()
    private let statusLabel = NSTextField(labelWithString: "")

    private var chipTopSpace: NSLayoutConstraint!
    private var chipHeight: NSLayoutConstraint!
    private var statusTopSpace: NSLayoutConstraint!
    private var statusHeight: NSLayoutConstraint!
    /// While model text is pushed into the field, its textDidChange
    /// is display sync, not user input.
    private var applyingModelText = false
    /// The last field editor and the span it underlined, for safe
    /// removal of the temporary error marking.
    private weak var lastEditor: NSTextView?
    private var underlinedRange: NSRange?

    init(
        model: SearchFieldModel,
        scopes: @escaping @MainActor () -> [(label: String, scope: SearchScope)]
    ) {
        self.model = model
        self.scopes = scopes
        super.init(nibName: nil, bundle: nil)
        // The model gates before the debounce and after every await;
        // it reads composition state off the live field editor.
        model.hasMarkedText = { [weak self] in
            (self?.field.currentEditor() as? NSTextView)?.hasMarkedText() ?? false
        }
    }

    required init?(coder: NSCoder) {
        fatalError("SearchFieldController is built in code")
    }

    override func loadView() {
        let strip = NSView()
        field.placeholderString = "Search mail"
        field.delegate = self
        field.target = self
        field.action = #selector(submitField(_:))
        field.sendsWholeSearchString = true // The model owns incremental debounce.
        field.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        field.setAccessibilityIdentifier("search.field")
        field.setAccessibilityLabel("Search mail")

        let scopeMenu = NSMenu()
        scopeMenu.delegate = self
        scopePopup.menu = scopeMenu
        scopePopup.setAccessibilityIdentifier("search.scope")
        scopePopup.setAccessibilityLabel("Search scope")
        scopePopup.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        scopePopup.cell?.lineBreakMode = .byTruncatingTail

        sortPopup.addItems(withTitles: SortItem.allCases.map(\.title))
        sortPopup.target = self
        sortPopup.action = #selector(sortChanged(_:))
        sortPopup.setAccessibilityIdentifier("search.sort")
        sortPopup.setAccessibilityLabel("Sort search results")

        chipRow.orientation = .horizontal
        chipRow.spacing = 4
        chipRow.alignment = .centerY
        chipRow.setAccessibilityIdentifier("search.chips")
        chipScroll.documentView = chipRow
        chipScroll.hasHorizontalScroller = true
        chipScroll.hasVerticalScroller = false
        chipScroll.autohidesScrollers = true
        chipScroll.borderType = .noBorder
        chipScroll.isHidden = true

        statusLabel.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        statusLabel.textColor = .secondaryLabelColor
        statusLabel.lineBreakMode = .byTruncatingTail
        statusLabel.setAccessibilityIdentifier("search.status")
        statusLabel.isHidden = true

        for subview in [field, scopePopup, sortPopup, chipScroll, statusLabel] {
            subview.translatesAutoresizingMaskIntoConstraints = false
            strip.addSubview(subview)
        }
        chipRow.translatesAutoresizingMaskIntoConstraints = false
        chipTopSpace = chipScroll.topAnchor.constraint(equalTo: field.bottomAnchor)
        chipHeight = chipScroll.heightAnchor.constraint(equalToConstant: 0)
        statusTopSpace = statusLabel.topAnchor.constraint(equalTo: chipScroll.bottomAnchor)
        statusHeight = statusLabel.heightAnchor.constraint(equalToConstant: 0)
        NSLayoutConstraint.activate([
            field.topAnchor.constraint(equalTo: strip.topAnchor, constant: 8),
            field.leadingAnchor.constraint(equalTo: strip.leadingAnchor, constant: 8),
            field.widthAnchor.constraint(greaterThanOrEqualToConstant: 64),
            scopePopup.widthAnchor.constraint(greaterThanOrEqualToConstant: 80),
            scopePopup.widthAnchor.constraint(lessThanOrEqualToConstant: 120),
            scopePopup.leadingAnchor.constraint(equalTo: field.trailingAnchor, constant: 8),
            scopePopup.centerYAnchor.constraint(equalTo: field.centerYAnchor),
            sortPopup.leadingAnchor.constraint(equalTo: scopePopup.trailingAnchor, constant: 8),
            sortPopup.trailingAnchor.constraint(equalTo: strip.trailingAnchor, constant: -8),
            sortPopup.centerYAnchor.constraint(equalTo: field.centerYAnchor),
            chipTopSpace, chipHeight, statusTopSpace, statusHeight,
            chipScroll.leadingAnchor.constraint(equalTo: strip.leadingAnchor, constant: 8),
            chipScroll.trailingAnchor.constraint(equalTo: strip.trailingAnchor, constant: -8),
            chipRow.leadingAnchor.constraint(equalTo: chipScroll.contentView.leadingAnchor),
            chipRow.topAnchor.constraint(equalTo: chipScroll.contentView.topAnchor),
            chipRow.bottomAnchor.constraint(equalTo: chipScroll.contentView.bottomAnchor),
            chipRow.trailingAnchor.constraint(
                greaterThanOrEqualTo: chipScroll.contentView.trailingAnchor
            ),
            statusLabel.leadingAnchor.constraint(equalTo: strip.leadingAnchor, constant: 8),
            statusLabel.trailingAnchor.constraint(
                lessThanOrEqualTo: strip.trailingAnchor, constant: -8
            ),
            statusLabel.bottomAnchor.constraint(equalTo: strip.bottomAnchor, constant: -8),
        ])
        view = strip
        applyModel()
        observeModel()
    }

    // MARK: - Model to view

    /// Retire the editor buffer as well as its parser ticket on list navigation.
    func navigated(to spec: WindowSpec) {
        if case .search = spec { return }
        applyingModelText = true
        _ = field.abortEditing()
        field.stringValue = ""
        model.navigated(to: spec)
        applyingModelText = false
        if isViewLoaded { applyModel() }
    }

    private func observeModel() {
        withObservationTracking {
            _ = model.text
            _ = model.clauses
            _ = model.scope
            _ = model.sort
            _ = model.failure
            _ = model.errorRange
            _ = model.lastValidText
            _ = model.queryJSON
            _ = model.isParsing
            _ = model.canEditClauses
            _ = scopes()
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in
                self?.applyModel()
                self?.observeModel()
            }
        }
    }

    private func applyModel() {
        updateChipEnablement()
        // While a composition is live: never rebuild chips, rewrite
        // text, alter the editor's selection or mark its text.
        guard !model.isComposing else { return }
        syncText()
        refreshScopeMenu()
        sortPopup.selectItem(at: (model.sort == .date ? SortItem.date : .relevance).rawValue)
        rebuildChips()
        refreshStatus()
        refreshErrorMarking()
    }

    /// Compare live bytes before writing; rewriting equal text drops the caret.
    private func syncText() {
        let editor = field.currentEditor() as? NSTextView
        lastEditor = editor ?? lastEditor
        let live = editor?.string ?? field.stringValue
        guard editor?.hasMarkedText() != true,
              !live.utf8.elementsEqual(model.text.utf8) else { return }
        setFieldText(model.text, editor: editor)
    }

    private func setFieldText(_ text: String, editor: NSTextView?) {
        applyingModelText = true
        defer { applyingModelText = false }
        if let editor {
            editor.string = text
        } else {
            field.stringValue = text
        }
    }

    private func updateChipEnablement() {
        for subview in chipRow.arrangedSubviews {
            (subview as? NSButton)?.isEnabled = model.canEditClauses
        }
    }

    // MARK: - Scope and sort

    /// Missing display names must not silently broaden a chosen scope.
    private func refreshScopeMenu() {
        guard let menu = scopePopup.menu else { return }
        let entries = scopes()
        menu.removeAllItems()
        var options: [(String, SearchScope)] = [("All accounts", .all)]
        for entry in entries where entry.scope != .all
            && !options.contains(where: { $0.1 == entry.scope }) {
            options.append((entry.label, entry.scope))
        }
        if !options.contains(where: { $0.1 == model.scope }) {
            switch model.scope {
            case .all: break
            case .account(let id): options.append(("Account \(id)", model.scope))
            case .mailbox(let id): options.append(("Mailbox \(id)", model.scope))
            }
        }
        for (title, scope) in options {
            let item = NSMenuItem(
                title: title, action: #selector(scopeChosen(_:)), keyEquivalent: ""
            )
            item.target = self
            item.representedObject = scope
            item.state = scope == model.scope ? .on : .off
            menu.addItem(item)
        }
        if let current = menu.items.first(where: {
            ($0.representedObject as? SearchScope) == model.scope
        }) {
            scopePopup.select(current)
        } else {
            scopePopup.selectItem(at: 0)
        }
    }

    @objc
    private func scopeChosen(_ sender: NSMenuItem) {
        model.chooseScope((sender.representedObject as? SearchScope) ?? .all)
    }

    @objc
    private func sortChanged(_ sender: NSPopUpButton) {
        model.chooseSort(SortItem(rawValue: sender.indexOfSelectedItem)?.sort ?? .relevance)
    }

    // MARK: - Clause chips

    private func rebuildChips() {
        chipRow.arrangedSubviews.forEach { $0.removeFromSuperview() }
        for clause in model.clauses where clause.isChip {
            let button = ClauseChipButton(
                title: clause.label, target: self, action: #selector(editClause(_:))
            )
            button.bezelStyle = .rounded
            button.controlSize = .small
            button.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
            button.clauseID = clause.id
            button.menu = clauseMenu(for: clause)
            button.isEnabled = model.canEditClauses
            button.setAccessibilityIdentifier("search.clause.\(clause.id.uuidString)")
            button.setAccessibilityLabel("Search clause \(clause.label)")
            chipRow.addArrangedSubview(button)
        }
        let hasChips = !chipRow.arrangedSubviews.isEmpty
        chipScroll.isHidden = !hasChips
        chipTopSpace.constant = hasChips ? 6 : 0
        chipHeight.constant = hasChips ? 24 : 0
    }

    /// Actions carry the clause's expiring UUID, never a stored
    /// numeric position.
    private func clauseMenu(for clause: SearchClause) -> NSMenu {
        let menu = NSMenu(title: clause.label)
        for (title, action) in [
            ("Edit", #selector(editClauseItem(_:))),
            ("Remove", #selector(removeClauseItem(_:))),
            ("Move earlier", #selector(moveClauseItem(_:))),
            ("Move later", #selector(moveClauseItem(_:))),
        ] as [(String, Selector)] {
            let item = NSMenuItem(title: title, action: action, keyEquivalent: "")
            item.target = self
            item.representedObject = clause.id
            item.isEnabled = model.canEditClauses
            menu.addItem(item)
        }
        return menu
    }

    @objc
    private func editClause(_ sender: ClauseChipButton) {
        if let id = sender.clauseID { editClause(id: id) }
    }

    @objc
    private func editClauseItem(_ sender: NSMenuItem) {
        if let id = sender.representedObject as? UUID { editClause(id: id) }
    }

    /// Select the clause within core-canonical text, retaining groups and ids.
    private func editClause(id: UUID) {
        guard let selection = model.selection(for: id) else { return }
        model.updateText(selection.text, composition: .committed)
        setFieldText(selection.text, editor: field.currentEditor() as? NSTextView)
        guard let window = view.window,
              window.makeFirstResponder(field),
              let editor = field.currentEditor() as? NSTextView else { return }
        editor.setSelectedRange(selection.range)
    }

    @objc
    private func removeClauseItem(_ sender: NSMenuItem) {
        if let id = sender.representedObject as? UUID { model.removeClause(id) }
    }

    @objc
    private func moveClauseItem(_ sender: NSMenuItem) {
        guard let id = sender.representedObject as? UUID else { return }
        model.moveClause(id, toward: sender.title == "Move earlier" ? .earlier : .later)
    }

    // MARK: - Status and error marking

    private func refreshStatus() {
        var text = ""
        if let failure = model.failure {
            text = failure.localizedDescription
            if model.queryJSON != nil, !model.lastValidText.isEmpty {
                text += " Showing results for: \(model.lastValidText)"
            }
        }
        statusLabel.stringValue = text
        statusLabel.isHidden = text.isEmpty
        statusTopSpace.constant = text.isEmpty ? 0 : 4
        statusHeight.isActive = text.isEmpty
    }

    /// The model's UTF-16 span becomes a temporary red underline on
    /// the live field editor; a zero-length (EOF) span has nothing to
    /// mark; the status line keeps the error visible. No caret moves.
    private func refreshErrorMarking() {
        let editor = field.currentEditor() as? NSTextView
        if let applied = underlinedRange,
           let old = lastEditor ?? editor, !old.hasMarkedText(),
           let manager = old.layoutManager {
            let end = min(NSMaxRange(applied), old.string.utf16.count)
            if end > applied.location {
                let clear = NSRange(location: applied.location, length: end - applied.location)
                manager.removeTemporaryAttribute(.underlineColor, forCharacterRange: clear)
                manager.removeTemporaryAttribute(.underlineStyle, forCharacterRange: clear)
            }
        }
        underlinedRange = nil
        lastEditor = editor
        guard let editor, let range = model.errorRange,
              range.length > 0,
              !editor.hasMarkedText(),
              NSMaxRange(range) <= editor.string.utf16.count
        else { return }
        editor.layoutManager?.addTemporaryAttributes([
            .underlineStyle: NSUnderlineStyle.single.rawValue,
            .underlineColor: NSColor.systemRed,
        ], forCharacterRange: range)
        underlinedRange = range
    }

    @objc private func submitField(_ sender: NSSearchField) {
        guard !model.isComposing else { return }
        model.updateText(sender.stringValue)
    }
}

extension SearchFieldController: NSSearchFieldDelegate {
    /// The live field editor carries marked text, unlike the cell's stored value.
    func controlTextDidChange(_ notification: Notification) {
        guard !applyingModelText else { return }
        let editor = field.currentEditor() as? NSTextView
        let value = editor?.string ?? field.stringValue
        model.updateText(
            value,
            composition: editor?.hasMarkedText() == true ? .marked : .committed
        )
    }

    func controlTextDidEndEditing(_ notification: Notification) {
        guard !applyingModelText else { return }
        if model.isComposing || !field.stringValue.utf8.elementsEqual(model.text.utf8) {
            model.updateText(field.stringValue, composition: .committed)
        }
    }

    /// Return submits the actual current value, cancellation clears;
    /// while a composition is live, both belong to the input method.
    func control(
        _ control: NSControl,
        textView: NSTextView,
        doCommandBy commandSelector: Selector
    ) -> Bool {
        guard !textView.hasMarkedText() else { return false }
        switch commandSelector {
        case #selector(NSResponder.insertNewline(_:)):
            model.updateText(textView.string, composition: .committed)
            return true
        case #selector(NSResponder.cancelOperation(_:)):
            model.clear()
            return true
        default:
            break
        }
        return false
    }
}

extension SearchFieldController: NSMenuDelegate {
    /// Sidebar scopes can arrive after the strip: rebuild on open.
    func menuNeedsUpdate(_ menu: NSMenu) {
        if menu === scopePopup.menu { refreshScopeMenu() }
    }
}

/// A clause chip: clicking edits; a right-click offers the clause
/// menu (Edit, Remove, Move earlier/later) via the standard context
/// machinery, keeping the actions keyboard- and VoiceOver-visible.
@MainActor
private final class ClauseChipButton: NSButton {
    var clauseID: UUID?
}

/// The sort popup defaults to Relevance.
private enum SortItem: Int, CaseIterable {
    case relevance
    case date

    var title: String {
        self == .relevance ? "Relevance" : "Date"
    }

    var sort: SearchSort {
        self == .relevance ? .relevance : .date
    }
}
