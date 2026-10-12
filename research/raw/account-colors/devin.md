<!-- provenance: helper=devin model=swe-2-max started=2026-10-12T04:23:53+00:00 exit=0 brief=research/briefs/ -->

# Sweetbriar research: color-coding accounts

Devin (SWE-2 Max), 2026-10-12. Companion to the Sweetbriar research snapshot
in `research/` (esp. [00-summary](../../00-summary.md),
[08-macos-ui](../../08-macos-ui.md), [09-linux](../../09-linux.md),
[07-standards](../../07-standards.md), and the classic-client studies).

Owner's requirement: "One feature I need is to color-code accounts and show
these colors clearly (e.g. as the window color for new mail windows)."
This report treats that as a requirement and researches how to do it well.

## TL;DR

- **Account colors are a proven, loved feature with unusually thin coverage.**
  Thunderbird added built-in account colors in 128 "Nebula" (July 2024) after
  years of Bugzilla demand (bug 1482172 and the 22-year-old bug 355520 for a
  compose indicator), and Thunderbird 153 (July 2026) extended them to
  unified-folder rows. eM Client 11 (2026-10-07) ships account colors "in the
  top bar of new message and reply windows" — literally the requested feature.
  Spark, Airmail, Mimestream, Canary and Mailspring all color rows in the
  unified inbox. Apple Mail, Gmail web, and desktop Outlook don't have it.
- **The safety argument is real.** Postbox's signature "Domain Fencing"
  (2015) exists purely to stop wrong-account sends; Thunderbird bugs and
  support threads document the pain going back to 2011. Color alone isn't a
  mitigation — the compose window must name the account (text + color +
  avatar) — but a whole-window color is the strongest ambient cue available.
- **On macOS 26/27 this is implementable with public APIs.** The clean native
  design is not a flat solid tint: it's a Liquid-Glass identity band —
  an `NSGlassEffectView` with `tintColor` set to the account color (or a
  small colored titlebar-accessory strip via `NSTitlebarAccessoryViewController`),
  tinted toolbar items via `NSToolbarItem.style = .prominent` +
  `backgroundTintColor`, plus the account's name and monogram in the strip.
  `window.tab.accessoryView` is the only sanctioned per-tab hook.
- **On Linux/GTK4, per-window tinting is CSS.** A per-widget `GtkCssProvider`
  or a class like `.account-work` on the window/headerbar overrides the
  libadwaita accent variables (`--accent-bg-color`, `--accent-color`,
  `--accent-fg-color`); libadwaita's own docs bless per-widget accent
  overrides. GNOME notifications can't be colored; neither can macOS banners.
- **One stored color, derived everywhere.** Store one sRGB color per account
  (optional per-identity override), derive light/dark variants in the shared
  Rust core (OKLCH is a good space for this), and keep the pair inside
  contrast guardrails. Color is never the sole carrier: pair it with the
  account name/monogram everywhere, and honor
  `accessibilityDisplayShouldDifferentiateWithoutColor`.
- **The color stays local.** No mail protocol carries an account color.
  IMAP METADATA (RFC 5464) can stash `/private/vendor/...` annotations on
  servers that support it (Dovecot/Cyrus yes; Gmail/Outlook no), so treat it
  as best-effort seeding, not sync — consistent with D23 (per-machine
  metadata in v1, stable IDs reserved for later sync).
- **Counter-argument worth taking seriously:** a saturated window tint reads
  "un-Mac-like," and Apple's own guidance says tint glass to convey meaning
  sparingly. The answer: the account color *is* meaning (that's exactly the
  use case Apple's tint guidance carves out), the default should be a
  restrained strip/chip rather than a soaked window, and a "Subtle / Bold"
  intensity preference plus Reduce Transparency support keeps it native.

## 1. Precedents

### 1a. Mail clients that ship account colors

| Client | What it does | Where the color shows | Evidence |
|---|---|---|---|
| **Thunderbird 128+** | Built-in "account color" per account (added 128.0 ESR, July 2024, "by popular demand"). TB 153 (July 2026) added an account-color indicator to unified-folder rows with an account-name tooltip | Account icon in folder pane; color shown in the compose "From" field; unified-folder row indicator (153) | [TB 128 release post](https://blog.thunderbird.net/2024/07/welcome-to-thunderbird-128-nebula/), [128.0 release notes](https://www.thunderbird.net/en-US/thunderbird/128.0esr/releasenotes), [TB 153 release notes](https://www.thunderbird.net/en-US/thunderbird/153.0/releasenotes/), [bug 1482172](https://bugzilla.mozilla.org/show_bug.cgi?id=1482172), [bug 355520](https://bugzilla.mozilla.org/show_bug.cgi?id=355520) |
| **Betterbird** (TB fork) | Full account colors since **115.6.0** (Jan 2024), **full-row** list colors since 115.8.0 — predates and out-scopes upstream; its feature table calls TB's implementation "Incomplete" (cards view only; a default blue marker when unconfigured) | Row colors in message list, compose identity pulldown | [Betterbird feature table](https://www.betterbird.eu/), [128.x release notes](https://betterbird.eu/releasenotes/index-previous-128.html) |
| **Account Colors (Thunderbird add-on, retired)** | Per-account and per-identity font *and* background colors; folder pane, thread pane, message header, and the compose window header/From menu. Abandoned when TB 74+ removed legacy extension APIs (v13.5, Feb 2020) | Row fonts/backgrounds, header labels, compose From menu | [addons.thunderbird.net page](https://addons.thunderbird.net/en-us/thunderbird/addon/account-colors/) |
| **eM Client 11** (2026-10-07) | Per-account color chosen in account settings | **Top bar of new message and reply windows** — pitched explicitly as "never send an email from the wrong email address" | [eM Client 11 sneak peek](https://www.emclient.com/blog/em-client-11-sneak-peek--account-colors-835) |
| **Mimestream** | Per-account color in Settings → Accounts; per-profile color + icon | Small colored badge on each message in unified inbox; default color for labels in sidebar; Focus-filtered Profiles per account group | [Mimestream unified inbox help](https://mimestream.com/help/user-guide/unified-inbox), [profiles help](https://mimestream.com/help/user-guide/managing-profiles), [release notes](https://mimestream.com/releases) |
| **Spark** | Per-account color ("Color coding for accounts" toggle in Appearance) + account Title | Marks which account each inbox email belongs to; per-account notification settings; **per-account LED notification color on Android** | [Spark help](https://sparkmailapp.com/help/add-manage-accounts/choose-the-account-name-title-and-color) |
| **Airmail** | Per-account color + per-folder/label colors | "A thin coloured line displayed on the left hand inside border of the message panel" in All Inboxes | [Airmail appearance prefs](https://help.airmailapp.com/en-us/article/preferences-appearance-airmail-for-macos-xob04q/) |
| **Canary Mail** | Per-account color, automatic | Vertical color marker on the **right edge** of thread-list rows in Unified View (macOS and Windows) | [Canary help](https://canarymail.io/help/unified-view-macos) |
| **Mailspring** | Per-account "Account Label" + "Account Color" (community PR #2240 closing issues #526, #1738, #2184) | Account tags in sidebar; account-colored tags on unified-inbox conversations | [Mailspring multi-account docs](https://www.getmailspring.com/docs/setting-up-multiple-accounts), [PR 2240](https://github.com/Foundry376/Mailspring/pull/2240) |
| **Postbox** (discontinued 2024) | No built-in account colors — the official workaround was a colored "Topic" per account applied by filters (TO IS your-address → Add Topic). Its distinctive safety feature was **Domain Fencing**: red recipient bubbles when about to send to/from the wrong domain or account | Compose recipient tokens turn red | [Postbox features](https://www.postbox-inc.com/features), [Custom Colors for Accounts](https://support.postbox-inc.com/hc/en-us/articles/360045276134-Custom-Colors-for-Accounts), [Postbox 4 review](https://brettterpstra.com/2015/05/20/postbox-4/) |
| **Outlook for Mac** | "Profiles" (v16.70, April 2023): groups of accounts with per-profile **theme color** — whole-window theme changes when you switch profiles | Whole app theme; profile icon in nav rail; Focus filter integration | [M365 Insider blog](https://techcommunity.microsoft.com/blog/microsoft365insiderblog/profiles-in-outlook-for-mac/4219359), [9to5Mac](https://9to5mac.com/2023/04/17/microsoft-outlook-mac-profiles) |
| **BlueMail** | Per-account color (Settings → Accounts → Account Color) | Colors each message in the Unified Inbox; per-account Default From | [BlueMail help](https://bluemail.me/help/accounts-colors/), [unified inbox](https://bluemail.me/help/unified-inbox/) |
| **FairEmail (Android)** | Per-account colors for rows | Unified-inbox row accents | (well-known; spot-verified via app feature lists — mark medium confidence) |

### 1b. Mail clients that don't

- **Apple Mail**: no account colors. Users have hacked it for a decade with
  Rules → "Set Color of background/text" keyed on the receiving account —
  multiple Apple Community threads (2013, 2018 with 124 "Me too"s). Dark Mode
  recolors those rule colors unpredictably and there's no way to remove a
  message color once set — a cautionary tale about deriving dark variants
  properly. ([Apple Community](https://discussions.apple.com/thread/6024434),
  [MacRumors](https://forums.macrumors.com/threads/is-there-a-way-to-make-emails-from-different-mailboxes-display-in-colors.1532402/))
  Mail *does* have the seven renamable flag colors — now standardized as
  `$MailFlagBit0/1/2` in RFC 9979 (May 2026) — which constrain the palette
  (see §6).
- **Gmail web**: no unified inbox at all, no account colors. Label colors are
  message-scoped, not account-scoped.
- **Outlook web / new Outlook for Windows**: no per-account colors in the
  folder pane or unified inbox; Microsoft Q&A threads explicitly confirm it's
  not possible ([example](https://learn.microsoft.com/en-us/answers/questions/4521914/how-to-distinguish-different-e-mail-accounts-in-th),
  [Outlook mobile request](https://learn.microsoft.com/en-us/answers/questions/4597086/color-code-each-email-account-in-the-unified-inbox)).
  Category colors exist (25 presets via Graph) but are message metadata.
- **MailMate**: no account colors — but it does have **per-mailbox colors**
  (Mailbox → Color submenu) and, more importantly for the safety story, it
  auto-derives the reply From via an "Address Pattern" matching
  `X-Original-To`/`Delivered-To` headers
  ([MailMate blog, 2013](https://blog.freron.com/2013/handling-multiple-identities)),
  and a hidden preference warns when sending outside an "internal" domain list
  ([hidden preferences](https://manual.mailmate-app.com/hidden_preferences.html)).
- **KMail / Evolution / Geary**: no per-account color. KDE Discuss documents a
  10-step tag+filter workaround for KMail ("From selected accounts only" →
  tag with text color); Evolution offers folder colors and filter-time "set
  color," and a Debian feature request for account-name colors is open.
  ([KDE Discuss](https://discuss.kde.org/t/kmail-different-color-per-account/40248))
- **Thunderbird for Android (ex-K-9)**: gap known — issue #5389 "Display
  account colors in all lists" is open.
  ([thunderbird-android#5389](https://github.com/thunderbird/thunderbird-android/issues/5389))
- **Superhuman**: no per-account color; Split Inboxes can be named with an
  emoji "to save space and add color" (that's the closest thing).
  ([Split Inbox help](https://help.superhuman.com/hc/en-us/articles/46005636204941-Custom-Split-Inbox))
- **Edison Mail**: no per-account color found in help docs (unverified).

### 1c. The classic-client lineage

From the snapshot's studies:

- **Eudora**: personalities were first-class (server, SMTP, address,
  signature, stationery) and replies inherited the receiving personality —
  but there was no color per personality. Color went to the 15 named labels.
  ([research/classics/eudora.md](../../classics/eudora.md))
- **Mailsmith**: 32 labels; account was a filter criterion. No account color.
- **Claris Emailer**: 19 named **colored priorities** — color attached to
  message metadata, not accounts. Its "one In Box across every service" is
  the conceptual ancestor of the unified inbox Sweetbriar's colors must serve.
  ([research/classics/claris-emailer.md](../../classics/claris-emailer.md))
- **Musashi**: 7-color message highlight, per-account settings, and crucially
  the Virtual Message Viewer that shows "where each message was filed" — the
  "which context am I in" cue by *name*, which Sweetbriar should render by
  name *and* color. ([research/classics/musashi.md](../../classics/musashi.md))

So the classics colored *messages* (labels, priorities, highlights) but not
*accounts*. The account-color idea is a post-2010 unified-inbox invention —
which makes sense: it only matters once messages from different accounts
share one list.

### 1d. Outside email — this is a well-established pattern

- **Chrome profiles**: each profile can have its own theme color tinting the
  window frame/toolbar (`profile_highlight_color` in Chromium's profile
  attributes). Syncs via the Google account.
  ([chromium source](https://chromium.googlesource.com/chromium/src/+/HEAD/chrome/browser/profiles/profile_attributes_entry.cc))
- **Edge**: same Chromium profile model; Edge-specific per-profile window
  colors (unverified beyond the shared engine).
- **Firefox Multi-Account Containers**: each container gets a color + icon;
  tabs show a colored underline, the address bar shows a colored container
  icon/label. Firefox offers 9 named container colors (blue, turquoise,
  green, yellow, orange, red, pink, purple, toolbar), exposed to extensions
  via the `contextualIdentities` API. Users have filed requests for more
  colors and for non-color indicators (dashed/dotted tab lines) — direct
  accessibility evidence that underline-only cues aren't enough.
  ([support page](https://support.mozilla.org/en-US/kb/containers),
  [contextualIdentities API](https://developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/API/contextualIdentities),
  [MAC PR #2841](https://github.com/mozilla/multi-account-containers/pull/2841),
  [issue 2777](https://github.com/mozilla/multi-account-containers/issues/2777))
- **Slack**: themes are per-workspace, per-person — Slack's own help
  recommends "selecting a different theme for each one to help tell them
  apart"; users cite it as the fix for posting in the wrong workspace —
  the exact "wrong account" failure Sweetbriar cares about.
  ([Slack help](https://slack.com/help/articles/205166337-Change-your-Slack-theme))
- **macOS Terminal / iTerm2**: profiles define the whole window appearance —
  background color, tab color — per profile; iTerm2 supports separate
  light/dark color sets per profile. The original "window color = context"
  precedent. ([iTerm2 colors docs](https://iterm2.com/documentation-preferences-profiles-colors.html))
- **Arc browser**: each Space has its own theme color, icon, and default
  profile — the "which space am I in" cue for browser chrome.
  ([Arc Spaces help](https://resources.arc.net/hc/en-us/articles/19228064149143-Spaces-Distinct-Browsing-Areas))
- **VS Code + Peacock**: tints title/status bar per project — the developer
  version of "which context am I in?"
  ([marketplace](https://marketplace.visualstudio.com/items?itemName=johnpapa.vscode-peacock))
- **Every calendar app**: Apple Calendar and Fantastical color-code
  calendars; "a calendar is identified by its name and color"
  ([Apple Support](https://support.apple.com/guide/calendar/change-a-calendars-name-or-color-icl1030/mac)).
  Nobody questions this in calendars — email accounts are the same idea.

### 1e. What users complain about

- **Demand outruns supply.** Thunderbird's account color shipped in 128
  after a formal request (bug 1482172, 2018) driven by the death of the
  Account Colors add-on; the request for a compose-window identity indicator
  (bug 355520) dated to 2002 and was only fixed by TB 128's From-field
  colors — over two decades from request to ship.
- **Too-subtle indicators.** TB users asked to reuse the account color for
  message-list card borders (TB 128 release-post comments); Betterbird calls
  the built-in indicator a "rather useless additional blue indicator" when
  unconfigured and ships full-row colors itself
  ([blog.betterbird.eu](https://blog.betterbird.eu/)); a Canary feature
  request ("the very thin bar doesn't really help that much") has 39 votes
  ([roadmap](https://roadmap.canarymail.io/b/90p891vj/feature-ideas));
  Firefox container users filed bugs for more-visible tab indicators
  (bug 1325902) and for non-color styles (issue 2777).
- **Half measures get called out.** Betterbird's whole pitch against upstream
  is "Incomplete implementation" — Sweetbriar should not repeat the mistake
  of coloring only the icon.
- **Colors that don't survive theme changes.** Apple Mail rule colors break
  under Dark Mode with no fix or undo — the exact failure a
  light/dark-variant pipeline prevents.
- **CVD users hit limits first.** An eM Client forum user: "I'm color-blind…
  it limits how many [colors] I can use and still be able to distinguish."
  ([forum.emclient.com](https://forum.emclient.com/t/indicate-email-account-in-inbox-preview-pane/48864))

## 2. Where the color should appear

Exhaustive list, then priority. "Cost" means implementation complexity and
design risk, not just code.

| Surface | Shows | Feasible? | Priority |
|---|---|---|---|
| Sidebar / account rows | Colored account icon/dot + name | Yes, both platforms | **P0** |
| Unified inbox rows | Small colored edge or badge on each row (Mimestream, Canary, Airmail all do a variant) | Yes | **P0** |
| **Compose window** | Title-bar/toolbar identity band + tinted From control (§3, §4) | Yes on both | **P0** |
| Reader header / message view | Account chip next to recipients ("to Work <me@work>") | Yes | **P0** |
| Mixed-account threads | Per-message account chip/marker in thread view | Yes | P1 |
| Out Box rows | Account color + name on each queued item | Yes | **P0** (safety: last place to see "about to send as") |
| Notifications | macOS/GNOME banners: **system draws them — no color API** (app icon only; even changing the icon isn't supported). You can only put the account name in title/body, or draw your own banner window (private/hacky path — NotificationNanny does it on macOS with accessibility APIs) | No via public API | P2 (name in text only) |
| Dock icon / badge | `NSDockTile.badgeLabel` is text-only on the system red pill; can't tint per account | No per-account | P3 |
| Menu bar extra | Can show a colored (non-template) icon or per-account counts in menu items | Partial | P2 |
| Search results | Same row treatment as unified inbox | Yes | P1 |
| Window tabs | macOS: `window.tab.accessoryView` = per-tab accessory (a colored dot) is the only public API; per-tab fill needs private `NSTabBar` internals (Tabberwocky does it, not App Store-safe). Linux: notebook tab styling possible | Partial | P1 |
| Stage Manager / Mission Control | These render the real window, so any visible title-bar tint shows in thumbnails automatically — an argument for putting the cue in the chrome, not deep in content | Automatic | — |
| Account color in "From" menus/pickers | Everywhere an identity is chosen: From popup, move/copy account pickers, rules UI | Yes | **P0** |
| Focus filters / profiles | If Sweetbriar adds profiles later (Mimestream/Outlook precedent), profile color = account-group color | Later | P3 |

**Design rule from the precedents:** the two surfaces that prevent actual
harm are (a) the unified list/reader (identifying *received* mail) and
(b) the compose window (identifying the *sending* identity). Everything
else is garnish. Note the asymmetry: on lists you want a *small* marker that
doesn't pollute 50 rows; on compose you want the *largest* cue that's still
legible — because that's where the mistake leaves the building.

## 3. The compose window — the safety case

**How real is wrong-account sending?**

- Postbox built "Domain Fencing" as a headline feature in 2015: "Ever
  accidentally sent a work email from your personal account – or vice-versa?"
  Recipients outside the account's configured domains get red address
  bubbles. ([postbox-inc.com](https://www.postbox-inc.com/features),
  [BetaNews](https://betanews.com/article/postbox-unveils-major-new-features-and-improvements-to-simplify-email-management/))
- Thunderbird's bug history is a 20-year catalog: bug 327713 (opened ~2006,
  still NEW with ~15 duplicates — X-Account-Key forces the wrong From on
  reply), bug 704613 (with 8 accounts, replies picked a wrong-account
  identity in ~98% of cases, tagged "privacy"), bug 688147 (feature request
  to *learn* correspondent↔account mappings and warn — still unconfirmed),
  bug 760784 (wrong account after moving messages). A support thread titled
  "Accidentally sending emails from the wrong account" asks for "a favicon
  or some visual aid to let you know that you are in a particular email
  account within an email compose window" — users independently invented
  this feature. Add-ons filled the gap meanwhile: Identity Chooser and
  Flexible Identity exist purely for From selection.
  ([bug 327713](https://bugzilla.mozilla.org/show_bug.cgi?id=327713),
  [bug 704613](https://bugzilla.mozilla.org/show_bug.cgi?id=704613),
  [bug 688147](https://bugzilla.mozilla.org/show_bug.cgi?id=688147),
  [support thread](https://support.mozilla.org/ca/questions/1191400))
- Outlook has the same scar tissue: a Slipstick forum thread "Replies being
  sent from wrong account" (2018), a Microsoft KB on mail delivered to the
  wrong account (updated 2025), an Ask Leo thread (2005, 77 comments) asking
  how to "configure Outlook to ALWAYS ask which account," and a Super User
  answer that ships a VBA `Application_ItemSend` guard as the workaround.
  ([Slipstick](https://forums.slipstick.com/threads/96149-replies-being-sent-from-wrong-account),
  [MS KB](https://learn.microsoft.com/en-us/troubleshoot/exchange/outlook-issues/emails-sent-to-wrong-account-if-has-many-accounts),
  [Super User](https://superuser.com/questions/366306))
- eM Client 11's own marketing: account colors let you "make sure you never
  send an email from the wrong email address."
- Slack's per-workspace themes are used for the same reason (wrong-workspace
  posts).
- Existing mitigations show the bar: Gmail defaults to "reply from the same
  address the message was sent to" plus "treat as alias"; Airmail
  auto-selects the receiving account on reply; MailMate derives the reply
  identity from `X-Original-To`/`Delivered-To` and can warn on external
  sends. ([Gmail help](https://support.google.com/mail/answer/22370),
  [Airmail](https://help.airmailapp.com/en-us/article/how-to-manage-multiple-email-identities-in-airmail-11rrpbj/),
  [MailMate](https://blog.freron.com/2013/handling-multiple-identities))

**Why color + not just the From address.** In every surveyed client the From
row is a narrow control; users don't re-read their own address. Peripheral,
ambient cues (window chrome color, like a Terminal profile or a Slack
workspace theme) are how people *feel* context before they look at details.
Postbox's red-bubble warning is a good second layer — a *rule-based* alert,
not just a look.

**Sweetbriar behavior spec (compose):**

1. The account/identity color + name follow the **selected From identity**,
   not the account that happens to own a mailbox folder. Switching From
   re-tints immediately (a short crossfade, ~150 ms, honors Reduce Motion).
2. New message: color = default identity of the account associated with the
   *currently viewed mailbox*, falling back to the default identity.
3. Reply/forward: color = the identity the original was delivered to
   (Eudora's rule: replies come from the personality that received the
   message — the bug history above shows what happens when clients get this
   wrong). If the user overrides From, the tint follows.
4. Reply-time **cross-account warning** (Postbox-style, optional but on by
   default): if recipients include addresses that pattern-match a *different*
   account's domain history, show a gentle inline banner — "Sending as Work;
   3 recipients usually correspond with Personal" — dismissible, learnable.
   This is a rules feature, not just color, but it's what converts the color
   system into a real mitigation.
5. Undo Send remains the last line of defense.

## 4. macOS implementation (macOS 26/27, Liquid Glass)

Verified API surface (all confirmed present on macOS 26+; checked
2026-10-12):

| API | What it does here | Availability |
|---|---|---|
| `NSWindow.titlebarAppearsTransparent = true` + `NSWindow.backgroundColor` | Classic trick: titlebar stops drawing its own background so the window color shows through — still works, but under Liquid Glass the toolbar floats as glass over content, so this alone produces a subtle bleed, not a crisp tint | 10.10+ |
| `NSGlassEffectView` | Embeds a content view in glass; **`tintColor`** tints "the background and glass effect"; `style` is `.regular` or `.clear`; `cornerRadius`, `effectIsInteractive` configurable. This is the sanctioned way to make a *tinted glass* strip/chip | macOS 26.0+ ([docs](https://developer.apple.com/documentation/appkit/nsglasseffectview)) |
| `NSGlassEffectContainerView` | Groups multiple glass elements so they share adaptive appearance | macOS 26.0+ |
| `NSToolbarItem.style = .prominent` + `backgroundTintColor` | Tints a toolbar item's glass with accent or a custom color — e.g., tint the Send button or a dedicated account chip with the account color | macOS 26.0+ ([docs](https://developer.apple.com/documentation/appkit/nstoolbaritem/style-swift.property)) |
| `NSTitlebarAccessoryViewController` (`window.addTitlebarAccessoryViewController(_:)`) | A custom view bar directly under the titlebar — the classic place for a persistent identity strip (that's how apps show "beta"/context banners); can host an `NSHostingView` for a SwiftUI strip | long-standing |
| `window.tab.accessoryView` / `tab.attributedTitle` | The **only** public per-tab customization: a small accessory view next to the tab title — enough for a colored dot + monogram per tab | long-standing ([docs](https://developer.apple.com/documentation/appkit/nswindowtab)) |
| `NSBackgroundExtensionView` | Extends content under sidebars/titlebar/inspector, replicating content in the margin — the macOS 26 way to run content chrome-to-chrome | macOS 26.0+ ([docs](https://developer.apple.com/documentation/appkit/nsbackgroundextensionview)) |
| SwiftUI `containerBackground(_:for: .window)` + `toolbarBackgroundVisibility(_:for:)` | Replaces the window background with any `ShapeStyle` (color, gradient, material) and hides the toolbar background — the SwiftUI route to a tinted window (macOS 15+) | macOS 15+ ([WWDC24](https://developer.apple.com/videos/play/wwdc2024/10148/)) |
| SwiftUI `.glassEffect(_:in:)` + `Glass.regular.tint(Color)` | Per-view tinted glass chips — e.g., a "From: Work" pill in a SwiftUI island | macOS 26.0+ ([WWDC25/323](https://developer.apple.com/videos/play/wwdc2025/323/)) |
| `NSWorkspace.accessibilityDisplayShouldReduceTransparency` / `…ShouldIncreaseContrast` / `…ShouldDifferentiateWithoutColor` + `NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification` | Appearance overrides the design must honor | 10.10+ |
| `NSDockTile.badgeLabel` | Text-only badge; cannot be colored per account | — |
| `UNUserNotificationCenter` | Banner appearance is system-owned: app icon, no tint support; per-account color not possible without replacing banners with custom windows (hacky, accessibility-API-based; not recommended) | — |

**System-appearance interactions (macOS 26/27):**

- **Liquid Glass "Tinted" vs "Clear"** (System Settings → Appearance, added
  in macOS 26.1, Nov 2025): Tinted raises glass opacity system-wide; either
  way our tint still applies — the strip must be tested under both.
  ([MacRumors 26.1](https://www.macrumors.com/2025/11/03/apple-releases-macos-tahoe-26-1/))
- **Dark Mode**: derive a dark variant (deeper, lower-luminance) of the
  account color — never reuse the light-mode hex. (Apple Mail rule colors
  breaking in Dark Mode is the cautionary tale.)
- **Reduce Transparency**: glass renders more opaque; keep the strip's
  *information* (color + label) identical.
- **Increase Contrast**: raise text/strip contrast; consider drawing a border.
- **Differentiate Without Color**: when `accessibilityDisplayShouldDifferentiateWithoutColor`
  is true, add a shape/glyph cue (striped/hatched edge, icon) and always
  pair color with the account name — the account name is always present
  anyway, which satisfies the *spirit* even without the flag.
- **Accent color**: don't fight `controlAccentColor` for controls; the
  account color lives in the identity band and tinted chips, not in
  selection/focus colors.
- **macOS 27 note**: WWDC26 kept refining Liquid Glass (sidebar refraction,
  interactive glass, `NSViewCornerConfiguration` concentricity). Sidebar
  icons "retain their color" in 27 — good for colored account icons. The
  toolbar's look is still in flux (a translucent bar behind buttons shipped
  in 27 betas, per [mjtsai.com](https://mjtsai.com/blog/2026/06/09/golden-gate-sidebars-and-toolbars/)),
  so prefer the accessory-strip approach, which doesn't depend on toolbar
  internals.

**Recommended macOS design — "identity band," not "painted window":**

```
┌─────────────────────────────────────────────┐
│ ●●●   Compose   [ Work ● account chip ]  ✉  │ ← titlebar/toolbar (untinted glass)
├─────────────────────────────────────────────┤ ← 3–4 pt NSTitlebarAccessoryViewController
│ ▓▓▓▓ Work — me@work.example ▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓ │    strip in account color + label
├─────────────────────────────────────────────┤
│ From: [me@work.example ▾] (tinted glass)    │
│ To:   …                                     │
└─────────────────────────────────────────────┘
```

- The strip is an `NSGlassEffectView` (`.regular`, `tintColor = accountColor`)
  inside an `NSTitlebarAccessoryViewController` (`layoutAttribute = .bottom`,
  `fullScreenMinHeight` set so it survives fullscreen), containing the
  account monogram + name text. Glass keeps it native; tint keeps it
  colorful; text keeps it accessible.
- The From popup/chip gets the same tint (`backgroundTintColor` on a
  prominent `NSToolbarItem`, or a SwiftUI `.glassEffect(.regular.tint(c))`).
- Reading panes get a compact chip, not a strip (list density wins).
- Why not soak the whole window: Liquid Glass already colors chrome subtly
  via content bleed; a fully tinted window fights the adaptive material and
  looks un-Mac-like. The band is legible in Mission Control/Stage Manager
  thumbnails (it renders as the window's top edge) and survives window
  tabbing (the accessory view can reflect the *selected* tab's account).
- Optional "Bold" intensity preference can additionally set
  `titlebarAppearsTransparent` + `backgroundColor` for a full-chrome tint
  for users who want the Terminal-profile look.

## 5. Linux implementation (GTK4/libadwaita)

**Verified mechanisms (checked 2026-10-12):**

- **Per-widget CSS providers still exist in GTK4**:
  `gtk_style_context_add_provider(context, provider, priority)` affects "the
  style of the widget to which context belongs"; for app-wide rules use
  `gtk_style_context_add_provider_for_display` with
  `GTK_STYLE_PROVIDER_PRIORITY_APPLICATION`. (Users' own `gtk-4.0/gtk.css`
  can still override at `PRIORITY_USER`.) ([GTK4 StyleContext docs](https://docs.gtk.org/gtk4/method.StyleContext.add_provider.html))
- **Practical shape**: define N classes once (`.sb-acct-0 … .sb-acct-7`, or
  arbitrary-color via inline CSS variables) and set them on the window's
  header bar + a strip widget:
  ```css
  .sb-acct-2 headerbar, headerbar.sb-acct-2 {
    background: linear-gradient(to bottom, var(--sb-acct-bg), shade(var(--sb-acct-bg), 0.9));
    color: var(--sb-acct-fg);
  }
  /* or override libadwaita accents on the subtree: */
  .sb-acct-2 { --accent-bg-color: var(--orange-3);
               --accent-color: oklab(from var(--accent-bg-color) var(--standalone-color-oklab)); }
  ```
  CSS custom properties inherit to descendant widgets, so setting
  `--accent-*` on the window recolors accent-sensitive children.
- **libadwaita 1.6+ accent system**: `AdwStyleManager:accent-color` /
  `accent-color-rgba` report the *system* accent (9 named colors: blue,
  teal, green, yellow, orange, red, pink, purple, slate, exposed as
  `--accent-blue` … `--accent-slate` and derived `--accent-bg-color`,
  `--accent-fg-color`, `--accent-color`). Libadwaita's own docs bless
  overriding them **per widget** — "Accent can also be overridden for an
  individual widget," with the documented recipe of setting
  `--accent-bg-color` and recomputing `--accent-color` via `oklab(...)`.
  Note `AdwStyleManager` still reports the system color — the override is a
  style-sheet fact, so the app tracks its own state.
  ([libadwaita Styles & Appearance](https://gnome.pages.gitlab.gnome.org/libadwaita/doc/main/styles-and-appearance.html),
  [CSS variables](https://gnome.pages.gitlab.gnome.org/libadwaita/doc/1.6/css-variables.html))
- **GtkHeaderBar** exposes a `headerbar` CSS node; styling it per window is
  ordinary GTK theming. The compose window should mirror the macOS design: a
  colored header-bar-adjacent strip (a `GtkBox` with the account class) plus
  the account name in the title widget.
- **GNOME HIG / a11y**: honor high-contrast (`AdwStyleManager` has
  `system-supports-color-schemes`/HC plumbing; Adwaita ships a HighContrast
  stylesheet — test with it). There's no exact GNOME equivalent of
  "Differentiate without color," so the paired name+color rule carries the
  load; announce the account through `accessible-name`/ATK for Orca.
- **Notifications on GNOME**: freedesktop notifications carry no color —
  server draws them. Account name in the summary/body is all you get.
- **Qt6/KDE fallback** (if the Qt route is chosen per research/09):
  per-widget stylesheets or `QPalette` on the window's toolbar/header area
  are straightforward (`setStyleSheet` scoped to the widget, or Kirigami
  `Theme` inline color overrides for QML). KDE has no system-wide per-window
  accent to override — arguably easier than libadwaita, though you lose the
  "follows the user theme" freebie for the tinted area. (Per the snapshot,
  GTK4 remains the default; this stays a contingency note.)

## 6. Color design

### Palette

- **How many:** categorical-color work converges on roughly 6–12 reliably
  distinguishable hues (ColorBrewer's qualitative schemes stop at 8–12
  classes; Paul Tol's largest "muted" qualitative scheme is 10). CVD shrinks
  the safe set — Okabe-Ito offers 8 colorblind-safe colors (plus black).
  **Recommend 8 curated defaults + a custom picker**, auto-assigning
  distinct hues to new accounts (maximize minimum pairwise distance from
  already-used colors — the iwanthue approach, computable in the core).
  ([Okabe-Ito](https://jfly.uni-koeln.de/color/),
  [Paul Tol schemes PDF](https://personal.sron.nl/~pault/data/colourschemes.pdf),
  [ColorBrewer](https://colorbrewer2.org/))
- **Candidate default set** (Okabe-Ito based, sRGB hex; dark variants
  derived): blue `#0072B2`, vermillion `#D55E00`, bluish green `#009E73`,
  reddish purple `#CC79A7`, orange `#E69F00`, sky blue `#56B4E9`, yellow
  `#F0E442`, gray `#999999`. Paul Tol's *bright* (7) or *high-contrast* (3
  primaries + pairs) schemes are drop-in alternates; both are explicitly
  CVD-checked against the two main color-blind vision types.
- **Derived light/dark variants:** store one sRGB color; convert to OKLCH,
  clamp chroma/luminance to a "chrome band" gamut per mode (light mode:
  mid-luminance saturated; dark mode: lighter, desaturated ~20%), convert
  back, and persist nothing extra. OKLCH interpolation keeps perceived hue
  stable — better than HSL lightness math. (This also fixes the Apple Mail
  dark-mode failure mode where rule colors become unrecognizable.)
- **Custom picker:** `NSColorSampler`/`NSColorPanel` on macOS, `ColorPicker`
  in SwiftUI settings; `GtkColorDialog` on GTK4 — with a "may be hard to
  distinguish from X" hint when within ΔE threshold of a sibling account or
  of flag colors.

### Contrast

- Text on tinted surfaces follows WCAG 2.2: ≥4.5:1 for body text, ≥3:1 for
  large text and non-text UI (SC 1.4.3, 1.4.11); color may never be the
  *only* differentiator (SC 1.4.1). Compute contrast in the core; if a
  custom color can't hit 4.5:1 with black or white text, derive the
  foreground (or nudge the surface) automatically. APCA (WCAG 3 draft) is
  the better model for saturated colored surfaces — use it internally as a
  second check, but don't cite it as a conformance target (non-normative).

### Never color alone

- Always pair the color with the **account name** (strip text, chip label,
  sidebar row, VoiceOver/Orca label). Add a **monogram/avatar or symbol**
  (first letter, or a user-picked emoji/SF Symbol) — that's what survives
  grayscale, CVD, and "Differentiate without color" mode. Okabe-Ito's own
  CUD principle #2 is "redundant coding": color + shape/position/label.

### Avoiding collisions with other color systems

| Existing color system | Risk | Mitigation |
|---|---|---|
| Apple/RFC 9979 flag colors (red orange yellow green blue purple gray) | A red account strip next to a red flag badge looks related | Account indicators use a *different shape* (edge band/chip, not a flag glyph) and the flag glyph stays SF-rendered; offer an "avoid flag hues" palette option |
| Gmail label colors (restricted allowed-palette hexes) & Graph category colors (preset0–24) | Label/category chips share the row with account markers | Keep account color off row backgrounds; use edge marker or icon — visually distinct channel from labels (Mimestream's badge approach) |
| Provider brand colors (Gmail red, Outlook blue, Proton purple) | Auto-assigning brand colors can collide (two Gmail accounts both red) | Use brand color only as a *suggestion* for the first account of that provider; subsequent accounts get distance-maximized colors |
| System accent color | Tinted controls looking "accented" | Keep account tint on dedicated chrome (strip/chip), never on default-action buttons |

## 7. Data model and sync

- **Where it lives:** the account object (joining D14's first-class
  identities): `account.color` (sRGB, e.g. `#RRGGBB`) +
  `identity.color_override NULL`. The compose tint follows the *identity*,
  inheriting its account's color unless overridden — that matches the
  Account Colors add-on model (colors per account, overridable per
  identity).
- **Storage:** SQLite settings table in the core (the D1 "sole authority for
  mutable state"), included in export/backup generations like every other
  setting. Per D23, v1 is local-only, labeled per-machine.
- **Cross-device:** no mail protocol carries account color. Survey:
  - IMAP METADATA (RFC 5464) supports server-level and per-mailbox
    annotations with `/private/vendor/<vendor-token>` namespaces — Sweetbriar
    *could* write `/private/vendor/sweetbriar/account-color` on capable
    servers (Dovecot supports the extension; Gmail and Exchange Online do
    not advertise it). ([RFC 5464](https://www.rfc-editor.org/rfc/rfc5464.html))
  - JMAP: no account-color property in RFC 8621 objects; client-specific
    extension at best.
  - Graph `outlookCategory` colors (preset0–24) are message categories, not
    account settings.
  - Gmail API `users.labels[].color` accepts only a fixed ~100-color palette,
    and is label-scoped.
  - **Verdict:** keep it per-machine in v1 (consistent with D23); optionally
    *seed* new machines from IMAP METADATA where available, last-write-wins
    with a revision timestamp. Don't block the feature on sync.
- **Interesting interplay:** Mimestream already learned that users want the
  account color to also be the *default label color* — worth a "use account
  color for its label/folder icons" toggle.

## 8. Accessibility

- **VoiceOver/Orca:** the identity band is an accessible element with
  `accessibilityLabel = "Account: Work (me@work.example)"` (macOS) /
  ATK accessible-name (GTK). Sidebar rows and message chips get the account
  name appended to their label. When the compose From changes, post
  `NSAccessibilityAnnouncementRequestedNotification` / AT-SPI announcement:
  "Sending account changed to Work."
- **Differentiate Without Color** (macOS 10.10+):
  `NSWorkspace.accessibilityDisplayShouldDifferentiateWithoutColor` +
  `NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification`; when set,
  the strip also renders a per-account glyph/pattern. The account name in
  the strip is the baseline regardless.
- **Increase Contrast**: bump strip-to-titlebar contrast, add a 1-pt
  separator; **Reduce Transparency**: the glass strip becomes more opaque —
  keep label legible at both.
- **Keyboard access:** the From identity picker must be a first-class
  keyboard-navigable control (tab-stop, arrow-key popup) so identity
  selection never requires a mouse; the identity band itself is decorative
  chrome and should not steal tab order, but must be reachable via
  VoiceOver/Orca navigation.
- **Testing:** Sim Daltonism for CVD simulation on macOS screenshots; GNOME
  `gtk Inspector` + HighContrast style; run the palette through a
  protanopia/deuteranopia/tritanopia check at design time (Okabe-Ito &
  Tol already target deuteranopia/protanopia).
- CVD affects ~8% of males in the data Okabe-Ito cites — account count will
  also exceed any safe palette for power users (the eM Client user with 12
  accounts is real), which is why the name/monogram is non-negotiable.

## Options compared

**Where the compose cue lives (macOS):**

| Option | Native? | Persistence | Cost | Verdict |
|---|---|---|---|---|
| Whole-window tint (`titlebarAppearsTransparent` + `backgroundColor`) | Risky under Liquid Glass — fights adaptive material | Strongest ambient cue | Low | Optional "Bold" mode only |
| Tinted-glass identity strip (`NSTitlebarAccessoryViewController` + `NSGlassEffectView.tintColor`) | Yes — reads as a context banner | High; survives tab switching and fullscreen | Medium | **Recommended default** |
| Tinted toolbar items only (`NSToolbarItem` `.prominent` + `backgroundTintColor`) | Yes | Weaker — small colored controls | Low | Complement, not the primary cue |
| Private tab-bar tinting | No — private AppKit internals, not App Store-safe | Per-tab | Fragile | Avoid |
| Custom banner windows replacing notifications | No — suppresses system notifications, needs accessibility APIs | — | Fragile | Avoid |

**Where color data lives:**

| Option | Pro | Con | Verdict |
|---|---|---|---|
| Per machine (local SQLite + export) | Simple; matches D23; can't corrupt remote state | Colors differ across machines | **v1** |
| IMAP METADATA `/private/vendor/` | Server-carried on Dovecot/Cyrus | Gmail/Exchange don't support; conflict resolution needed | Optional seeding later |
| Encrypted cross-device settings sync | Consistent UX | Requires stable IDs/revisions Sweetbriar hasn't built | Post-v1 |

**Palette strategy:**

| Option | Pro | Con | Verdict |
|---|---|---|---|
| Okabe-Ito 8 | CVD-tested, familiar | Slightly dull for chrome | **Default set** |
| Paul Tol bright/high-contrast | Designed for categorical CVD-safe use | Fewer hues | Alternate set / fallback for CVD users |
| Provider brand colors | Instant recognition | Collides across same-provider accounts; implies false affiliation | Suggestion only, never auto-assign twice |
| Unlimited custom | User freedom | Can pick indistinguishable colors | Allowed, with a ΔE "too similar to account X" warning |

## 9. Recommendation for Sweetbriar

**Scope in v1 (ship it):**

1. Account color field (8 CVD-safe presets + custom picker + auto-assign),
   optional identity-level override.
2. macOS: colored account icon + name in sidebar; edge/chip marker on
   unified-inbox rows; account chip in reader header; **compose identity
   band** = `NSGlassEffectView` tint inside `NSTitlebarAccessoryViewController`
   with name + monogram, plus tinted From control; `window.tab.accessoryView`
   dot for tabbed compose windows.
3. Linux: same surfaces via `.sb-acct-N` CSS class on window/headerbar +
   libadwaita accent-variable override on the compose window subtree.
4. Out Box rows tagged with account color + name.
5. Reply From = receiving identity; From-picker shows colors in the popup.
6. Color in export/backup; per-machine for now.
7. Settings UI: Accounts → color swatch row (grid of presets + "Custom…"),
   "Avoid colors close to flag colors" toggle, intensity preference
   (Subtle strip / Bold full-chrome tint).

**Later:**

- Domain-fencing-style pre-send warnings (learned correspondent↔account
  patterns, TB bug 688147's idea, Postbox's precedent).
- Profile-level colors (Mimestream/Outlook profiles) if profiles ship.
- Per-tab tints via private NSTabBar only if a direct (non-App-Store) build
  ever justifies it — otherwise keep the accessory dot.
- IMAP METADATA seeding where the server supports it.
- Focus-filter-aware "work hours" dimming, custom banner notifications if a
  dedicated in-app notification surface is ever built (don't fight UNUserNotification).

**Defaults:** on for ≥2 accounts, off/irrelevant for a single account; first
account gets a neutral blue unless it's a Gmail/Outlook/Proton brand
suggestion.

**Strongest counter-argument:** "A tinted window looks un-Mac-like; Liquid
Glass is supposed to be adaptive and calm, and Apple's own guidance says
tint to convey meaning, sparingly." The answer is threefold: (1) Apple
tints glass precisely *to convey meaning* — the sending account is exactly
the kind of state tint exists for; (2) we're not soaking content — the
recommended design is a restrained glass identity band + chip, the same
shape as eM Client 11's "top bar" which shipped this month to mainstream
users; (3) the intensity preference and accessibility settings hand control
to the user — and the alternative (reading the From line) is demonstrably
what fails people today. If a future macOS makes full-window tinting gauche,
the strip survives the change.

## Risks and open questions

1. **Liquid Glass churn.** The toolbar/titlebar rendering changed between
   26.0 and 26.1 ("Tinted" mode) and again in 27 betas — design must not
   depend on toolbar internals. Mitigation: accessory strip + custom views
   only; verify each macOS beta.
2. **Per-identity vs per-account granularity** — is account-level enough for
   v1? (Owner requirement says accounts; identities inherit. Aliases within
   an account sharing a color is probably *correct* — same blast radius.)
3. **Color naming in a11y**: monogram vs emoji vs SF Symbol — pick in the
   prototype pass; emoji in chrome can look non-native.
4. **A tinted strip's interaction with window tabbing**: verify the accessory
   strip updates when the active tab changes accounts.
5. **Light-glass readability**: very pale account colors on clear glass over
   bright content — the chroma/luminance clamps need a real corpus test.
6. **Does eM Client 11's "top bar" look good?** Worth a hands-on review —
   it's the closest shipping implementation of the requirement.
7. **Sync**: if cross-device color parity matters later, an
   `/private/vendor/` IMAP METADATA convention plus a public spec is an
   option only for capable servers; otherwise export/import parity.

## Sources

- Thunderbird: [128 Nebula release](https://blog.thunderbird.net/2024/07/welcome-to-thunderbird-128-nebula/),
  [128.0esr release notes](https://www.thunderbird.net/en-US/thunderbird/128.0esr/releasenotes),
  [153 release notes](https://www.thunderbird.net/en-US/thunderbird/153.0/releasenotes/),
  [bug 327713](https://bugzilla.mozilla.org/show_bug.cgi?id=327713),
  [bug 355520](https://bugzilla.mozilla.org/show_bug.cgi?id=355520),
  [bug 1482172](https://bugzilla.mozilla.org/show_bug.cgi?id=1482172),
  [bug 704613](https://bugzilla.mozilla.org/show_bug.cgi?id=704613),
  [bug 688147](https://bugzilla.mozilla.org/show_bug.cgi?id=688147),
  [bug 760784](https://bugzilla.mozilla.org/show_bug.cgi?id=760784),
  [bug 1052075](https://bugzilla.mozilla.org/show_bug.cgi?id=1052075),
  [support thread](https://support.mozilla.org/ca/questions/1191400),
  [Account Colors add-on](https://addons.thunderbird.net/en-us/thunderbird/addon/account-colors/),
  [thunderbird-android#5389](https://github.com/thunderbird/thunderbird-android/issues/5389)
- Betterbird: [feature table](https://www.betterbird.eu/),
  [blog](https://blog.betterbird.eu/),
  [128.x release notes](https://betterbird.eu/releasenotes/index-previous-128.html)
- BlueMail: [account colors](https://bluemail.me/help/accounts-colors/),
  [unified inbox](https://bluemail.me/help/unified-inbox/)
- MailMate: [multiple identities](https://blog.freron.com/2013/handling-multiple-identities),
  [hidden preferences](https://manual.mailmate-app.com/hidden_preferences.html)
- Superhuman: [Split Inbox help](https://help.superhuman.com/hc/en-us/articles/46005636204941-Custom-Split-Inbox)
- KMail/Evolution: [KDE Discuss workaround](https://discuss.kde.org/t/kmail-different-color-per-account/40248)
- Canary roadmap: [account-color feature request](https://roadmap.canarymail.io/b/90p891vj/feature-ideas)
- Gmail: [send from a different address](https://support.google.com/mail/answer/22370)
- Wrong-account evidence: [Slipstick forum](https://forums.slipstick.com/threads/96149-replies-being-sent-from-wrong-account),
  [MS KB wrong-account delivery](https://learn.microsoft.com/en-us/troubleshoot/exchange/outlook-issues/emails-sent-to-wrong-account-if-has-many-accounts),
  [Super User VBA guard](https://superuser.com/questions/366306)
- eM Client: [11 sneak peek — account colors](https://www.emclient.com/blog/em-client-11-sneak-peek--account-colors-835),
  [forum: account colors](https://forum.emclient.com/t/account-colors/36017),
  [forum: preview pane account indicator](https://forum.emclient.com/t/indicate-email-account-in-inbox-preview-pane/48864)
- Mimestream: [unified inbox](https://mimestream.com/help/user-guide/unified-inbox),
  [profiles](https://mimestream.com/help/user-guide/managing-profiles),
  [account settings](https://mimestream.com/help/user-guide/account-settings),
  [release notes](https://mimestream.com/releases)
- Spark: [account name/title/color](https://sparkmailapp.com/help/add-manage-accounts/choose-the-account-name-title-and-color),
  [notifications](https://sparkmailapp.com/help/add-manage-accounts/customize-notifications)
- Airmail: [appearance prefs](https://help.airmailapp.com/en-us/article/preferences-appearance-airmail-for-macos-xob04q/),
  [identities](https://help.airmailapp.com/en-us/article/how-to-manage-multiple-email-identities-in-airmail-11rrpbj/)
- Canary: [unified view macOS](https://canarymail.io/help/unified-view-macos),
  [unified view Windows](https://canarymail.io/help/how-are-accounts-distinguished-in-unified-view-windows)
- Mailspring: [multi-account docs](https://www.getmailspring.com/docs/setting-up-multiple-accounts),
  [PR 2240](https://github.com/Foundry376/Mailspring/pull/2240)
- Postbox: [features](https://www.postbox-inc.com/features),
  [Custom Colors for Accounts](https://support.postbox-inc.com/hc/en-us/articles/360045276134-Custom-Colors-for-Accounts),
  [Postbox 4 review](https://brettterpstra.com/2015/05/20/postbox-4/),
  [BetaNews](https://betanews.com/article/postbox-unveils-major-new-features-and-improvements-to-simplify-email-management/)
- Outlook: [Profiles in Outlook for Mac](https://techcommunity.microsoft.com/blog/microsoft365insiderblog/profiles-in-outlook-for-mac/4219359),
  [9to5Mac profiles coverage](https://9to5mac.com/2023/04/17/microsoft-outlook-mac-profiles),
  [MS Q&A folder-pane color](https://learn.microsoft.com/en-us/answers/questions/4521914/how-to-distinguish-different-e-mail-accounts-in-th),
  [MS Q&A unified-inbox color request](https://learn.microsoft.com/en-us/answers/questions/4597086/color-code-each-email-account-in-the-unified-inbox),
  [category pastel complaint](https://learn.microsoft.com/en-us/answers/questions/5652035/how-to-get-categories-colors-back-to-bright-shades)
- Apple Mail: [flags guide](https://support.apple.com/guide/mail/mark-emails-to-revisit-later-mlhlp1052/mac),
  [rule-color workaround thread](https://discussions.apple.com/thread/6024434),
  [dark-mode complaint](https://discussions.apple.com/thread/254642583),
  [iPhone request](https://discussions.apple.com/thread/8331332)
- Non-mail: [Firefox containers](https://support.mozilla.org/en-US/kb/containers),
  [contextualIdentities API](https://developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/API/contextualIdentities),
  [MAC issue 2777](https://github.com/mozilla/multi-account-containers/issues/2777),
  [bug 1325902](https://bugzilla.mozilla.org/show_bug.cgi?id=1325902),
  [MAC PR 2841](https://github.com/mozilla/multi-account-containers/pull/2841),
  [Chromium profile colors](https://chromium.googlesource.com/chromium/src/+/HEAD/chrome/browser/profiles/profile_attributes_entry.cc),
  [Slack themes](https://slack.com/help/articles/205166337-Change-your-Slack-theme),
  [iTerm2 profile colors](https://iterm2.com/documentation-preferences-profiles-colors.html),
  [Arc Spaces](https://resources.arc.net/hc/en-us/articles/19228064149143-Spaces-Distinct-Browsing-Areas),
  [VS Code Peacock](https://marketplace.visualstudio.com/items?itemName=johnpapa.vscode-peacock),
  [Apple Calendar color](https://support.apple.com/guide/calendar/change-a-calendars-name-or-color-icl1030/mac)
- macOS APIs: [NSGlassEffectView](https://developer.apple.com/documentation/appkit/nsglasseffectview),
  [Style enum](https://developer.apple.com/documentation/appkit/nsglasseffectview/style-swift.enum),
  [tintColor](https://developer.apple.com/documentation/appkit/nsglasseffectview/tintcolor),
  [NSToolbarItem.style](https://developer.apple.com/documentation/appkit/nstoolbaritem/style-swift.property),
  [backgroundTintColor](https://developer.apple.com/documentation/appkit/nstoolbaritem/backgroundtintcolor),
  [NSTitlebarAccessoryViewController](https://developer.apple.com/documentation/appkit/nstitlebaraccessoryviewcontroller),
  [NSWindowTab](https://developer.apple.com/documentation/appkit/nswindowtab),
  [NSBackgroundExtensionView](https://developer.apple.com/documentation/appkit/nsbackgroundextensionview),
  [differentiateWithoutColor](https://developer.apple.com/documentation/appkit/nsworkspace/accessibilitydisplayshoulddifferentiatewithoutcolor),
  [titlebarAppearsTransparent](https://developer.apple.com/documentation/appkit/nswindow/titlebarappearstransparent),
  [WWDC25/310 AppKit new design](https://developer.apple.com/videos/play/wwdc2025/310/),
  [WWDC25/323 SwiftUI new design](https://developer.apple.com/videos/play/wwdc2025/323/),
  [WWDC24/10148 windows](https://developer.apple.com/videos/play/wwdc2024/10148/),
  [macOS 26.1 tinted toggle](https://www.macrumors.com/2025/11/03/apple-releases-macos-tahoe-26-1/),
  [macOS 27 WWDC26/289](https://developer.apple.com/videos/play/wwdc2026/289/),
  [mjtsai 27 sidebars/toolbars](https://mjtsai.com/blog/2026/06/09/golden-gate-sidebars-and-toolbars/),
  [private NSTabBar styling](https://github.com/uncSoft/Tabberwocky)
- Linux: [GTK4 StyleContext.add_provider](https://docs.gtk.org/gtk4/method.StyleContext.add_provider.html),
  [Gtk.CssProvider](https://docs.gtk.org/gtk4/class.CssProvider.html),
  [HeaderBar CSS nodes](https://docs.gtk.org/gtk4/class.HeaderBar.html),
  [gtk-rs CSS book](https://gtk-rs.org/gtk4-rs/git/book/css.html),
  [libadwaita styles & appearance](https://gnome.pages.gitlab.gnome.org/libadwaita/doc/main/styles-and-appearance.html),
  [libadwaita CSS variables](https://gnome.pages.gitlab.gnome.org/libadwaita/doc/1.6/css-variables.html),
  [AdwStyleManager:accent-color](https://gnome.pages.gitlab.gnome.org/libadwaita/doc/main/property.StyleManager.accent-color.html)
- Standards/color: [RFC 5464](https://www.rfc-editor.org/rfc/rfc5464.html),
  [RFC 9979 draft flag-color table](https://www.ietf.org/archive/id/draft-ietf-mailmaint-messageflag-mailboxattribute-02.html),
  [Graph outlookCategory](https://learn.microsoft.com/en-us/graph/api/resources/outlookcategory?view=graph-rest-1.0),
  [Gmail users.labels color](https://developers.google.com/gmail/api/reference/rest/v1/users.labels),
  [Okabe-Ito CUD](https://jfly.uni-koeln.de/color/),
  [Paul Tol schemes](https://personal.sron.nl/~pault/),
  [tol-colors docs](https://tol-colors.readthedocs.io/en/latest/),
  [WCAG 2.2](https://www.w3.org/TR/WCAG22/)
- Notifications: [UNNotification icon limitation](https://stackoverflow.com/questions/72380347/unmutablenotificationcontent-change-the-default-app-icon),
  [Notifier macOS 26.4 issue](https://github.com/jamf/Notifier/issues/34),
  [NotificationNanny](https://github.com/chessper53/NotificationNanny)
