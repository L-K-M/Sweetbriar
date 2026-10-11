# 08 — The macOS front end

Synthesis of [raw/macos-ui/codex.md](raw/macos-ui/codex.md) and
[raw/macos-ui/devin.md](raw/macos-ui/devin.md). Both reached the same
verdict independently. Checked 2026-10-11. macOS 27 "Golden Gate" is
current (released 2026-09-14, Apple silicon only); macOS 26 Tahoe is the
previous release.

## Summary

- **AppKit owns the structure; SwiftUI fills bounded islands.** AppKit
  owns windows, menus, toolbars, the three-pane split, the message list,
  the composer and the responder chain. SwiftUI owns settings, inspectors,
  onboarding, account setup and the menu bar extra. Apple mixes the two in
  both directions itself, and Mimestream ships the same mix.
- **Message list:** view-based `NSOutlineView` (threaded) /
  `NSTableView` (flat), one model for both modes.
- **Reader:** one reused `WKWebView` for sanitized HTML, with page
  JavaScript off, deny-all content rules, and parts served from an app
  scheme. Plain text gets a native TextKit 2 view.
- **Composer:** `NSTextView` on TextKit 2. Plain text and Markdown first,
  restricted rich text next, WYSIWYG HTML only if demand proves it.
- **One command layer** feeds menus, shortcuts, AppleScript, Services, App
  Intents (Apple ships a mail schema domain), notifications and Shortcuts.
- **Distribution:** Developer ID, notarization and Sparkle first, with a
  mostly sandboxed design so a Mac App Store build stays possible.
- **Deployment floor:** macOS 26 (scene bridging, rich-text and WebKit
  APIs, AppKit Observation tracking), arm64 first; adopt macOS 27 APIs
  behind `@available`.

## AppKit vs SwiftUI in 2026

- **SwiftUI improved substantially.** WWDC25: Mac lists over 100k items
  load 6× faster and update up to 16× faster. There's a SwiftUI performance
  instrument, an `AttributedString` `TextEditor`, and a SwiftUI
  `WebView`/`WebPage`. macOS 27 adds a Document API, AppKit-interop
  bridges, and SwiftUI rendering some AppKit controls internally.
- **The remaining pain sits exactly in mail's hot spots** (community
  reports):
  - `Table` identity diffing and sort hangs at scale
  - context menus without "clicked but unselected" Mac semantics
  - `NSHostingView` sizing bugs inside table cells
  - no type-select customization
  - `TextEditor` not yet composer-grade (font panel, spellcheck quirks)
- **Interop got cheaper.** Since macOS 26, AppKit tracks `@Observable`
  reads in `draw`/`layout`, so AppKit and SwiftUI views can share one view
  model (devin; back-deploys to 15 behind an Info.plist key; unverified).
- **Decision rule:** build one fair list prototype, AppKit outline vs
  SwiftUI `Table`/`List`, on the same Rust query API. Include variable
  heights, threading, 10k-id selections, drag promises, VoiceOver and live
  sync. Switch to SwiftUI only if it matches interaction quality.
- **Liquid Glass:** use system toolbar, sidebar and control treatments;
  keep message bodies, dense rows and compose text on stable, legible
  surfaces. macOS 27 refined readability after Tahoe's rough reception
  (menu symbols hidden by default again). NetNewsWire's author reported
  real adoption cost (2026-09-22). Build the icon with Icon Composer.

## Message list

- Lightweight ordered id index from the core; display rows paged around the
  viewport (see [01-architecture](01-architecture.md)); a bounded row cache.
- Reused `NSTableCellView`s with simple or drawn content and **no Auto
  Layout or stack views in cells** (NetNewsWire rule). Cheap `heightOfRow`
  from heights cached by revision, width, font and preview mode. Explicit
  invalidation; never `reloadData()` for a flag change.
- Selection, expansion, anchor row, scroll offset and focus are separate,
  ID-based state, and they survive sorts and live updates.
- Type-select through the documented delegates, Mac context-menu semantics,
  drag-out `.eml` through `NSFilePromiseProvider`, and moves as explicit core
  commands with visible results.
- **Acceptance targets:**
  - p95 keyboard selection feedback under 50 ms
  - no frame overruns in warm scrolling (16.7 ms / 8.3 ms)
  - tested at 100k and 1M messages, mixed heights, long expanded threads,
    10k selected, live sync, resizing, VoiceOver

## Reader

- `WKWebView` with
  `defaultWebpagePreferences.allowsContentJavaScript = false`, enforced for
  every navigation. (`WKPreferences.javaScriptEnabled` is deprecated.) Our
  own `evaluateJavaScript` still works with page script off.
- `WKWebsiteDataStore.nonPersistent()`, separate from OAuth (which uses
  `ASWebAuthenticationSession`).
- A `WKContentRuleList` blocking all remote loads (images, stylesheets,
  fonts, media, fetch, redirects). Compile it before displaying any mail;
  fail closed if compilation fails. MailMate's release notes and
  Mimestream's tracker blocking document the same recipe.
- HTML is sanitized in Rust (`ammonia` + `lol_html`): scripts, forms,
  frames, objects, meta refresh and unknown schemes are removed; CSS is
  constrained (see [04-data-safety](04-data-safety.md), EFAIL).
- `cid:` and parts are served through a `WKURLSchemeHandler` on an app
  scheme (`sweetbriar-part://<message>/<part>`), resolved within the
  message's own MIME context, never by global Content-ID. Load the message
  HTML from the same scheme so resolution is uniform.
- **"Load remote content"** is explicit, per message or per sender, through
  an app-controlled fetch-and-rewrite path. Never by enabling JavaScript.
- **Processes:** `WKProcessPool` is deprecated and a no-op. Keep one active
  view per reading pane and reuse it. Devin favors prewarming a hidden view
  at launch (the first WebContent process is costly); codex says only
  after measurement and only after first paint. Rebuild the view from core
  data when the content process terminates.
- **Dark mode:** honor author `color-scheme`/`prefers-color-scheme`; offer
  an "original appearance" toggle; no blanket inversion.
- Native find bar over `find(_:configuration:)`. Printing through
  `printOperation(with:)` with an app-made header template. Distinguish
  Find in Message from mailbox search.
- **Plain text:** a non-editable `NSTextView` on TextKit 2, with quote-depth
  styling, links and format=flowed reflow. MailMate instead renders
  everything through HTML: a valid simplification if one renderer is
  preferred.
- **Open question:** "Reply quoting selection" with page JavaScript off.
  Prototype it with an app-owned script or a Services/pasteboard path.

## Composer

- `NSTextView(usingTextLayoutManager: true)`: spelling, grammar, input
  methods, data detectors, dictation and Writing Tools for free. Watch for
  TextKit 1 compatibility fallbacks. `STTextView` is a maintained TextKit 2
  alternative.
- **Modes:**
  - **Plain text:** format=flowed handled in the shared serializer, not as
    a view setting. Always honor flowed on display. MailMate stopped
    sending flowed by default in 2021 because Outlook and Gmail ignore it,
    so make sending it a preference.
  - **Markdown:** source preserved; generate `multipart/alternative` HTML
    through `pulldown-cmark` plus sanitizing.
  - **Restricted rich text:** later.
  - A web-based editor (ProseMirror, TipTap) only if complex HTML editing
    becomes central; it costs native menus, undo, paste and accessibility.
- **The durable draft model lives in Rust** with revisions; attributed
  strings are presentation only. Attachments are copied into the core's
  immutable store before a draft counts as "saved". Inline images get
  `Content-ID`s and `multipart/related` at send time.
- **Draft states:** Editing → Saved locally → Queued → Sending →
  Sent / Failed / Uncertain. "Saved" means a durable core acknowledgement.
- **Writing Tools:** protect quotes and signatures through ignored ranges;
  keep the pre-operation revision; keep Undo.
- **Classic power** (see [11-classic-clients](11-classic-clients.md)):
  quote-depth rewrap, Paste Quoted, clipboard history, clippings with
  placeholders, an external-editor round trip, a pre-send checks hook.

## System integration

| Surface | Mechanism | Sweetbriar use |
|---|---|---|
| App Intents | Mail schema domain (`archiveMail`, `sendDraft`, `replyMail`, ...); Apple Intelligence routes to the default mail app | Adopt early: Siri, Shortcuts and Spotlight actions |
| AppleScript | `.sdef`, scripting access groups for sandboxed callers, `suspendExecution` for async | Accounts, mailboxes, messages, drafts, selection, commands; Script menu folder; menu-intercept hooks (Mailsmith) |
| Services | `NSServices` | "New message with selection"; expose text to other apps |
| Spotlight | `CSSearchableIndex`, stable ids, deletions, deep links | Opt-in per account; Rust index stays authoritative |
| Quick Look | `QLPreviewPanel`; `.eml` preview extension | Space-bar attachment preview |
| Share extension | `com.apple.share-services` | Hand off into a durable draft before completing |
| Notifications | Categories, actions, text-input reply | Archive, read, reply; private previews by default |
| Focus filters | `SetFocusFilterIntent` | Per-account notification policy |
| Default mail app | `CFBundleURLTypes` + `NSWorkspace.setDefaultApplication` | User-initiated; RFC 6068 parsing; header-injection safe |
| Menu bar extra | `MenuBarExtra` | Optional unread count and quick compose; not the sync owner |
| Shortcuts customization | Menu key equivalents + recorder | Single-letter triage keys only outside text fields |
| Accessibility, localization | Custom-cell accessibility; `.xcstrings`; RTL | Required, not polish |
| Foundation Models | On-device ~3B model, 4,096-token context, availability varies | Optional, user-invoked, no mail-mutating tools ([10-landscape](10-landscape.md)) |

## Distribution

- **Mac App Store** requires the sandbox (mail storage in a container is
  fine). It forbids a self-updater and downloaded code, needs consent for
  helpers that run after quit, and Apple keeps 15–30%.
- **Developer ID + notarization + Sparkle 2.10** (EdDSA-signed, sandbox
  compatible) is the indie standard:
  - Mimestream: direct, sandboxed, $49.99/yr
  - MailMate: direct, left the Store in 2012 over sandboxing
  - Airmail: Store only
  - Spark: both channels (Electron)
- **Recommendation:** direct first, own billing (Paddle/FastSpring), and a
  checklist of Store incompatibilities as features land. Hardened-runtime
  library validation restricts plug-ins to our own Team ID: expose commands
  and scripts before loading third-party native code.

## Prototypes required before product work (codex)

1. The list comparison above.
2. A hostile-HTML reader with network tracing: displaying a blocked
   message emits zero requests.
3. Crash-tested native drafts and attachments.
4. A signed distribution build exercising default-mail registration, a
   Share extension, scripting and an optional helper.

## Apps to study

- **NetNewsWire** (open source): NSTableView timeline, NSOutlineView
  sidebar, WKWebView detail, layered frameworks.
- **Mimestream:** the closest product model.
- **MailMate:** the WKWebView recipe, keyboard customization, plain text
  and Markdown.
- **Things:** keyboard flow and polish.
- **Tower:** native UI per platform over shared logic.
- **Ghostty:** one core under SwiftUI and GTK.
- **Ivory:** the cost of Catalyst's missing native details.

## Key sources

- [WWDC25 What's new in SwiftUI](https://developer.apple.com/videos/play/wwdc2025/256/), [WWDC25 AppKit with the new design](https://developer.apple.com/videos/play/wwdc2025/310/)
- [allowsContentJavaScript](https://developer.apple.com/documentation/webkit/wkwebpagepreferences/allowscontentjavascript), [WKContentRuleListStore](https://developer.apple.com/documentation/webkit/wkcontentruleliststore), [WKProcessPool (deprecated)](https://developer.apple.com/documentation/webkit/wkprocesspool)
- [MailMate r6272 notes](https://updates.mailmate-app.com/release_notes_archive/MailMate_r6272.html)
- [App Intents mail domain](https://developer.apple.com/documentation/appintents/app-schema-domain-mail.md)
- [Writing Tools in AppKit](https://developer.apple.com/documentation/appkit/writing-tools)
- [NetNewsWire coding guidelines](https://github.com/Ranchero-Software/NetNewsWire/blob/main/Technotes/CodingGuidelines.md)
- [Sparkle sandboxing](https://sparkle-project.org/documentation/sandboxing/), [App Review 2.4.5](https://developer.apple.com/app-store/review/guidelines/)
- [Mimestream security model](https://mimestream.com/trust/security-and-privacy)
