import AppKit
import Observation
import SwiftUI
import SweetbriarBridge

enum SettingsTab: Hashable { case appearance, backups }

@MainActor
@Observable
private final class SettingsSelection {
    var tab = SettingsTab.appearance
}

@MainActor
final class AppearanceSettingsWindow: NSWindowController {
    private let selection = SettingsSelection()
    private let editor = ArchiveWriteTasks()

    init(session: CoreSession, shared: SharedAppearance, backups: SharedBackups) {
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 780, height: 680),
                              styleMask: [.titled, .closable, .miniaturizable, .resizable],
                              backing: .buffered, defer: false)
        window.title = "Settings"
        window.center()
        window.contentView = NSHostingView(rootView: AppearanceSettingsLoader(
            session: session, shared: shared, backups: backups, selection: selection, editor: editor
        ))
        super.init(window: window)
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    func select(_ tab: SettingsTab) { selection.tab = tab }

    func retireEditor() { editor.retire() }

    override func close() {
        retireEditor()
        super.close()
    }

    func cancelAndWait() async { await editor.cancelAndWait() }
}

private struct AppearanceSettingsLoader: View {
    let session: CoreSession
    let shared: SharedAppearance
    let backups: SharedBackups
    @Bindable var selection: SettingsSelection
    let editor: ArchiveWriteTasks
    @State private var bridge: CoreBridge?
    @State private var model: BackupsModel?
    @State private var failure: String?

    var body: some View {
        Group {
            if let bridge, let model {
                TabView(selection: $selection.tab) {
                    Group {
                        if model.isClosed {
                            ContentUnavailableView("Archive closed", systemImage: "exclamationmark.triangle",
                                                   description: Text("Quit and reopen Sweetbriar to settle the restore."))
                        } else if backups.isRestoring {
                            ProgressView("Waiting for the restore outcome…")
                        } else {
                            AccountAppearanceSettings(
                                bridge: bridge, model: AppearanceSettingsModel(appearance: shared.model(source: bridge)),
                                backups: model, editor: editor, editorRevision: editor.revision
                            )
                            .id(editor.revision)
                            .disabled(!model.canWrite)
                        }
                    }
                    .tabItem { Label("Appearance", systemImage: "paintpalette") }.tag(SettingsTab.appearance)
                    BackupsSettingsView(model: model, restoreBlockedReason: backups.restoreBlockedReason) {
                        guard backups.confirmRestore() else { return false }
                        // Synchronous, before the newly scheduled restore task can run.
                        editor.retire()
                        return true
                    }
                    .tabItem { Label("Backups", systemImage: "externaldrive") }.tag(SettingsTab.backups)
                }
            } else if let failure {
                ContentUnavailableView("Settings unavailable", systemImage: "exclamationmark.triangle", description: Text(failure))
            } else {
                ProgressView("Opening settings…")
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .task {
            do {
                let bridge = try await session.bridge()
                try Task.checkCancellation()
                self.bridge = bridge
                model = backups.model(source: bridge)
                if model == nil { failure = "Settings are unavailable while quitting." }
            } catch { failure = error.localizedDescription }
        }
    }
}

private struct AccountAppearanceSettings: View {
    let bridge: CoreBridge
    @State var model: AppearanceSettingsModel
    let backups: BackupsModel
    let editor: ArchiveWriteTasks
    let editorRevision: UInt64
    @State private var admissionError: String?
    @State private var accounts: [Account] = []
    @State private var loadingAccounts = true
    @State private var selected: String?
    @State private var loadError: String?
    @State private var customOpen = false
    @State private var editingAccount: AccountAppearance?
    @State private var customColor = Color.black
    @State private var customError: String?

    private var current: AccountAppearance? {
        model.appearance.appearance?.accounts.first { $0.accountId == selected }
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                Text("Accounts").font(.title2.bold())
                if let failure = model.appearanceFailure {
                    Text("Appearance may be out of date: \(failure)").foregroundStyle(.red)
                    Button("Retry appearance") { Task { await model.retryAppearance() } }
                }
                if let loadError {
                    Text(loadError).foregroundStyle(.red)
                } else if loadingAccounts {
                    ProgressView("Loading accounts…")
                } else if accounts.isEmpty {
                    Text("No accounts available.").foregroundStyle(.secondary)
                } else {
                    Picker("Account", selection: $selected) {
                        ForEach(accounts, id: \.accountId) { account in
                            Text(accountLabel(account)).tag(Optional(account.accountId))
                        }
                    }
                    if let current {
                        Text("Appearance").font(.headline)
                        Text("Choose a color for this account.")
                        Picker("Account color", selection: Binding<UInt8?>(
                            get: {
                                if case .paletteV1(let slot) = current.assignment { return slot }
                                return nil
                            },
                            set: { slot in
                                guard let slot else { return }
                                save(.paletteV1(slot: slot), account: current)
                            }
                        )) {
                            ForEach(model.appearance.palette, id: \.slot) { preset in
                                HStack {
                                    Circle().fill(color(preset.colorSrgb)).overlay(Circle().strokeBorder(.secondary)).frame(width: 20, height: 20)
                                    Text(preset.name)
                                }
                                .tag(Optional(preset.slot))
                                .accessibilityLabel(preset.name)
                            }
                        }
                        .pickerStyle(.radioGroup)
                        .disabled(model.saving || model.appearanceFailure != nil)
                        Button("Custom…") {
                            editingAccount = current
                            customColor = color(current.colorSrgb)
                            customError = nil
                            customOpen = true
                        }
                        .disabled(model.saving || model.appearanceFailure != nil)
                        if current.assignment == .custom {
                            Text("Custom: \(current.colorSrgb)").foregroundStyle(.secondary)
                        }
                        HStack(spacing: 12) {
                            preview("Light", current.tokens.light, current.monogram)
                            preview("Dark", current.tokens.dark, current.monogram)
                        }
                        Text("Stored on this device; included in export.").font(.footnote).foregroundStyle(.secondary)
                        if let error = model.error { Text(error).foregroundStyle(.red) }
                        if let admissionError { Text(admissionError).foregroundStyle(.red) }
                        if model.saving { ProgressView("Saving…") }
                    } else if model.appearanceFailure == nil {
                        ProgressView("Loading appearance…")
                    }
                }
                Spacer(minLength: 0)
            }
            .padding(24)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        }
        .task {
            do {
                accounts = try await bridge.accounts()
                selected = accounts.first?.accountId
            } catch { loadError = error.localizedDescription }
            loadingAccounts = false
        }
        .sheet(isPresented: $customOpen) {
            VStack(alignment: .leading, spacing: 16) {
                Text("Custom account color").font(.headline)
                ColorPicker("Color", selection: $customColor, supportsOpacity: false)
                if let customError { Text(customError).foregroundStyle(.red) }
                HStack {
                    Spacer()
                    Button("Cancel") { customOpen = false }.keyboardShortcut(.cancelAction)
                    Button("Save") {
                        guard let value = AppearanceSettingsModel.canonicalColor(NSColor(customColor)) else {
                            customError = "Choose an opaque sRGB color."
                            return
                        }
                        guard let account = editingAccount else { customOpen = false; return }
                        customOpen = false
                        save(.custom(colorSrgb: value), account: account)
                    }.keyboardShortcut(.defaultAction)
                }
            }.padding(24).frame(width: 360)
        }
    }

    private func save(_ color: AccountColor, account: AccountAppearance) {
        guard backups.canWrite else {
            admissionError = "The archive is not writable. Check Backups for its status."
            return
        }
        admissionError = nil
        if !editor.run(revision: editorRevision, { await model.choose(color, account: account) }) {
            admissionError = "The archive changed. Reopen this editor before saving."
        }
    }

    private func accountLabel(_ account: Account) -> String {
        let sections = accounts.map { SidebarModel.Section(id: $0.accountId, title: $0.displayName, entries: []) }
        let section = SidebarModel.Section(id: account.accountId, title: account.displayName, entries: [])
        return SidebarViewController.accountLabel(section, among: sections)
    }

    private func preview(_ name: String, _ variant: AppearanceVariant, _ mark: String) -> some View {
        VStack(alignment: .leading) {
            Text(name).font(.caption)
            HStack {
                Text(mark).bold().foregroundStyle(color(variant.foregroundSrgb))
                Text("From: \(accounts.first { $0.accountId == selected }?.displayName ?? "Account")").foregroundStyle(color(variant.foregroundSrgb))
                Spacer()
            }
            .padding(8)
            .background(color(variant.stripSrgb))
            .overlay(RoundedRectangle(cornerRadius: 4).stroke(color(variant.outlineSrgb)))
            .clipShape(RoundedRectangle(cornerRadius: 4))
        }
        .accessibilityElement(children: .combine)
    }

    private func color(_ hex: String) -> Color {
        guard hex.count == 7, hex.first == "#", let value = UInt32(hex.dropFirst(), radix: 16) else { return .primary }
        return Color(.sRGB, red: Double((value >> 16) & 255) / 255,
                     green: Double((value >> 8) & 255) / 255, blue: Double(value & 255) / 255, opacity: 1)
    }
}
