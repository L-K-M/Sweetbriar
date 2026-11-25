import AppKit
import Observation
import SwiftUI
import SweetbriarBridge
import UniformTypeIdentifiers

enum SettingsTab: Hashable { case appearance, accounts, reading, sending, backups }

@MainActor
@Observable
private final class SettingsSelection {
    var tab = SettingsTab.appearance
}

@MainActor
final class AppearanceSettingsWindow: NSWindowController {
    private let selection = SettingsSelection()
    private let editor = ArchiveWriteTasks()

    init(
        session: CoreSession, shared: SharedAppearance, cues: SharedComposeCue,
        backups: SharedBackups,
        onSignInAgain: @escaping @MainActor (Account) -> Void
    ) {
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 780, height: 680),
                              styleMask: [.titled, .closable, .miniaturizable, .resizable],
                              backing: .buffered, defer: false)
        window.title = "Settings"
        window.center()
        window.contentView = NSHostingView(rootView: AppearanceSettingsLoader(
            session: session, shared: shared, cues: cues, backups: backups,
            selection: selection, editor: editor, onSignInAgain: onSignInAgain
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
    let cues: SharedComposeCue
    let backups: SharedBackups
    @Bindable var selection: SettingsSelection
    let editor: ArchiveWriteTasks
    let onSignInAgain: @MainActor (Account) -> Void
    @State private var bridge: CoreBridge?
    @State private var model: BackupsModel?
    @State private var accountsModel: AccountsSettingsModel?
    @State private var diagnosticsModel: DiagnosticsModel?
    @State private var sendingModel: SendSafetySettingsModel?
    @State private var allowancesModel: RecipientAllowancesModel?
    @State private var readingModel: ReadingSettingsModel?
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
                        } else if let cue = cues.model(source: bridge) {
                            AccountAppearanceSettings(
                                bridge: bridge, model: AppearanceSettingsModel(appearance: shared.model(source: bridge), logos: bridge),
                                cue: cue, backups: model, editor: editor, editorRevision: editor.revision
                            )
                            .id(editor.revision)
                        } else {
                            ContentUnavailableView("Compose appearance unavailable", systemImage: "exclamationmark.triangle",
                                                   description: Text("Reopen Settings after the archive operation."))
                        }
                    }
                    .tabItem { Label("Appearance", systemImage: "paintpalette") }.tag(SettingsTab.appearance)
                    Group {
                        if model.isClosed {
                            ContentUnavailableView("Archive closed", systemImage: "exclamationmark.triangle",
                                                   description: Text("Quit and reopen Sweetbriar to settle the restore."))
                        } else if backups.isRestoring {
                            ProgressView("Waiting for the restore outcome…")
                        } else if let accountsModel, let diagnosticsModel {
                            AccountsSettingsView(
                                model: accountsModel, diagnostics: diagnosticsModel,
                                editor: editor, editorRevision: editor.revision,
                                appearance: shared.model(source: bridge),
                                onSignInAgain: onSignInAgain
                            )
                            .id(editor.revision)
                        } else {
                            ProgressView("Opening accounts…")
                        }
                    }
                    .disabled(!backups.canWrite)
                    .tabItem { Label("Accounts", systemImage: "person.crop.circle") }.tag(SettingsTab.accounts)
                    Group {
                        if model.isClosed {
                            ContentUnavailableView("Archive closed", systemImage: "exclamationmark.triangle",
                                                   description: Text("Quit and reopen Sweetbriar to settle the restore."))
                        } else if backups.isRestoring {
                            ProgressView("Waiting for the restore outcome…")
                        } else if let readingModel {
                            ReadingSettingsView(model: readingModel).id(editor.revision)
                        } else {
                            ProgressView("Opening reading settings…")
                        }
                    }
                    .disabled(!backups.canWrite)
                    .tabItem { Label("Reading", systemImage: "envelope.open") }.tag(SettingsTab.reading)
                    Group {
                        if model.isClosed {
                            ContentUnavailableView("Archive closed", systemImage: "exclamationmark.triangle",
                                                   description: Text("Quit and reopen Sweetbriar to settle the restore."))
                        } else if backups.isRestoring {
                            ProgressView("Waiting for the restore outcome…")
                        } else if let sendingModel {
                            SendingSettingsView(model: sendingModel, allowances: allowancesModel)
                                .id(editor.revision)
                        } else {
                            ProgressView("Opening sending settings…")
                        }
                    }
                    .disabled(!backups.canWrite)
                    .tabItem { Label("Sending", systemImage: "paperplane") }.tag(SettingsTab.sending)
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
                accountsModel = AccountsSettingsModel(source: bridge)
                diagnosticsModel = DiagnosticsModel(source: bridge)
                sendingModel = SendSafetySettingsModel(source: bridge)
                allowancesModel = RecipientAllowancesModel(
                    source: bridge,
                    accountNames: {
                        let accounts = (try? await bridge.accounts()) ?? []
                        return Dictionary(
                            accounts.map { ($0.accountId, $0.displayName) }, uniquingKeysWith: { first, _ in first })
                    })
                readingModel = ReadingSettingsModel(source: bridge)
                if model == nil { failure = "Settings are unavailable while quitting." }
            } catch { failure = error.localizedDescription }
        }
    }
}

private struct AccountAppearanceSettings: View {
    let bridge: CoreBridge
    @State var model: AppearanceSettingsModel
    let cue: ComposeCueModel
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
    /// The address the custom color sheet edits; nil edits the account.
    @State private var editingIdentity: IdentityAppearance?
    @State private var customColor = Color.black
    @State private var customError: String?
    @State private var letterText = ""
    /// Each identity's address, from the compose catalog.
    @State private var addresses: [String: String] = [:]

    private var current: AccountAppearance? {
        model.appearance.appearance?.accounts.first { $0.accountId == selected }
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                ComposeCueSettingsView(
                    cue: cue, editor: editor, revision: editorRevision, preview: cuePreview,
                    previewLogo: current.flatMap { AccountLogoImages.image(for: $0) }
                ) {
                    backups.canWrite
                }
                Divider()
                Group {
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
                            Picker(
                                "Account color",
                                selection: Binding<UInt8?>(
                                    get: {
                                        if case .paletteV1(let slot) = current.assignment { return slot }
                                        return nil
                                    },
                                    set: { slot in
                                        guard let slot else { return }
                                        save(.paletteV1(slot: slot), account: current)
                                    }
                                )
                            ) {
                                ForEach(model.appearance.palette, id: \.slot) { preset in
                                    HStack {
                                        Circle().fill(color(preset.colorSrgb)).overlay(
                                            Circle().strokeBorder(.secondary)
                                        ).frame(width: 20, height: 20)
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
                                editingIdentity = nil
                                customColor = color(current.colorSrgb)
                                customError = nil
                                customOpen = true
                            }
                            .disabled(model.saving || model.appearanceFailure != nil)
                            if current.assignment == .custom {
                                Text("Custom: \(current.colorSrgb)").foregroundStyle(.secondary)
                            }
                            letterControls(current)
                            logoControls(current)
                            addressControls(current)
                            HStack(spacing: 12) {
                                preview("Light", current.tokens.light, current.monogram, logo: AccountLogoImages.image(for: current))
                                preview("Dark", current.tokens.dark, current.monogram, logo: AccountLogoImages.image(for: current))
                            }
                            Text("Stored on this device; included in export.").font(.footnote)
                                .foregroundStyle(.secondary)
                            if let error = model.error { Text(error).foregroundStyle(.red) }
                            if let admissionError { Text(admissionError).foregroundStyle(.red) }
                            if model.saving { ProgressView("Saving…") }
                        } else if model.appearanceFailure == nil {
                            ProgressView("Loading appearance…")
                        }
                    }
                }
                .disabled(!backups.canWrite)
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
            // Without the catalog an address row shows its identity id.
            if let catalog = try? await bridge.composeCatalog() {
                addresses = Dictionary(
                    catalog.identities.map { ($0.identityId, $0.email) }, uniquingKeysWith: { first, _ in first })
            }
        }
        // Reseed the field when the account or its stored letter changes,
        // not on every revision: another write (a color, an address) must
        // not clear a letter still being typed.
        .task(id: "\(selected ?? "")#\(current?.symbol ?? "")") { letterText = current?.symbol ?? "" }
        .sheet(isPresented: $customOpen) {
            VStack(alignment: .leading, spacing: 16) {
                Text(editingIdentity == nil ? "Custom account color" : "Custom address color").font(.headline)
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
                        customOpen = false
                        if let identity = editingIdentity {
                            run { await model.setAddressColor(value, identity: identity) }
                        } else if let account = editingAccount {
                            save(.custom(colorSrgb: value), account: account)
                        }
                    }.keyboardShortcut(.defaultAction)
                }
            }.padding(24).frame(width: 360)
        }
    }

    private func save(_ color: AccountColor, account: AccountAppearance) {
        run { await model.choose(color, account: account) }
    }

    /// One write through the editor's admission: a read-only archive or a
    /// retired editor writes nothing and says why.
    private func run(_ write: @escaping @MainActor () async -> Void) {
        guard backups.canWrite else {
            admissionError = "The archive is not writable. Check Backups for its status."
            return
        }
        admissionError = nil
        if !editor.run(revision: editorRevision, write) {
            admissionError = "The archive changed. Reopen this editor before saving."
        }
    }

    /// The account's own letter (spec/08 "Letter"): one or two characters,
    /// empty for the automatic one.
    @ViewBuilder
    private func letterControls(_ account: AccountAppearance) -> some View {
        HStack {
            TextField("Letter", text: $letterText, prompt: Text(account.monogram))
                .frame(width: 140)
                .onChange(of: letterText) { _, text in
                    // Typing stops at two characters.
                    let limited = AppearanceSettingsModel.letter(text) ?? ""
                    if text.trimmingCharacters(in: .whitespacesAndNewlines).count > 2 { letterText = limited }
                }
                .onSubmit { run { await model.setLetter(letterText, account: account) } }
            Button("Set Letter") { run { await model.setLetter(letterText, account: account) } }
        }
        .disabled(model.saving || model.appearanceFailure != nil)
        Text("One or two characters. Leave it empty for the automatic letter.").font(.footnote)
            .foregroundStyle(.secondary)
    }

    /// The account's logo (spec/08 "Logo"): Choose Image… and Remove, and
    /// a note when the stored logo can't be shown.
    @ViewBuilder
    private func logoControls(_ account: AccountAppearance) -> some View {
        HStack {
            Text("Logo")
            Button("Choose Image…") { chooseLogo(for: account) }
            if account.logoPng != nil {
                Button("Remove") { run { await model.setLogo(nil, account: account) } }
            }
        }
        .disabled(model.saving || model.appearanceFailure != nil)
        if AccountLogoImages.isUndecodable(account) {
            Text("The stored logo can't be shown, so the letter shows instead. Choose another image.")
                .font(.footnote).foregroundStyle(.secondary)
        }
    }

    private func chooseLogo(for account: AccountAppearance) {
        let panel = NSOpenPanel()
        panel.allowedContentTypes = [.image]
        panel.allowsMultipleSelection = false
        panel.canChooseDirectories = false
        guard panel.runModal() == .OK, let url = panel.url else { return }
        run { await model.chooseLogo(at: url, account: account) }
    }

    /// Each of the account's addresses with the account color or its own
    /// (spec/08 "Addresses").
    @ViewBuilder
    private func addressControls(_ account: AccountAppearance) -> some View {
        let identities = model.appearance.appearance?.identities.filter { $0.accountId == account.accountId } ?? []
        if !identities.isEmpty {
            Text("Addresses").font(.headline)
            ForEach(identities, id: \.identityId) { identity in
                let address = addresses[identity.identityId] ?? identity.identityId
                HStack {
                    Text(address)
                    Spacer()
                    Picker(
                        "Color for \(address)",
                        selection: Binding<String?>(
                            get: { identity.colorOverrideSrgb },
                            set: { value in run { await model.setAddressColor(value, identity: identity) } })
                    ) {
                        Text("Use account color").tag(String?.none)
                        ForEach(model.appearance.palette, id: \.slot) { preset in
                            Text(preset.name).tag(Optional(preset.colorSrgb))
                        }
                        if let custom = identity.colorOverrideSrgb,
                            !model.appearance.palette.contains(where: { $0.colorSrgb == custom })
                        {
                            Text("Custom: \(custom)").tag(Optional(custom))
                        }
                    }
                    .labelsHidden()
                    .frame(width: 200)
                    .accessibilityLabel("Color for \(address)")
                    Button("Custom…") {
                        editingAccount = nil
                        editingIdentity = identity
                        customColor = color(identity.colorSrgb)
                        customError = nil
                        customOpen = true
                    }
                }
                .disabled(model.saving || model.appearanceFailure != nil)
            }
        }
    }

    private var cuePreview: IdentityStripView.Content {
        guard let current, let account = accounts.first(where: { $0.accountId == selected }) else {
            return ComposeCueSettingsView.noAccount
        }
        return .identity(
            label: "From: \(accountLabel(account))", monogram: current.monogram, tokens: current.tokens)
    }

    private func accountLabel(_ account: Account) -> String {
        let sections = accounts.map { SidebarModel.Section(id: $0.accountId, title: $0.displayName, entries: []) }
        let section = SidebarModel.Section(id: account.accountId, title: account.displayName, entries: [])
        return SidebarViewController.accountLabel(section, among: sections)
    }

    private func preview(_ name: String, _ variant: AppearanceVariant, _ mark: String, logo: NSImage?) -> some View {
        VStack(alignment: .leading) {
            Text(name).font(.caption)
            HStack {
                if let logo {
                    AccountLogoMark(image: logo, ring: color(variant.markerSrgb), side: 18)
                        .environment(\.colorScheme, name == "Dark" ? .dark : .light)
                } else {
                    Text(mark).bold().foregroundStyle(color(variant.foregroundSrgb))
                }
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
