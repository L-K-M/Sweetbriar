<!-- provenance: helper=codex model=gpt-6.1-sol started=2026-10-12T04:23:54+00:00 exit=0 brief=research/briefs/ -->

# Sweetbriar: color-coding accounts and sending identities

Research date: **2026-10-12**. Account color is a requirement. The recommendation is a persistent account-colored compose strip, explicit From identity, and consistent account markers throughout mixed-account views.

**Evidence convention:** product features and API status below were checked on 2026-10-12 unless an earlier publication or release date is stated. “(Unverified)” means the consulted material did not establish the claim; it does not mean the feature is absent. Recommendations, dimensions, defaults, and acceptance criteria are proposed Sweetbriar specifications, not measured product behavior.

The starting constraints are the project's [summary](../../00-summary.md), [macOS study](../../08-macos-ui.md), [Linux study](../../09-linux.md), [standards study](../../07-standards.md), and [decision agenda](../../notes/decision-agenda.md): first-class identities, an AppKit shell with SwiftUI islands, a macOS 26 floor, later GTK4/libadwaita, durable SQLite settings, and accessibility.

## 1. TL;DR

- **Make the compose window unmistakable.** Show an account-colored band containing a monogram, account name, and complete sending address. Its color follows the selected From identity immediately.
- **Account color and identity color are related but distinct.** An account supplies the default; an identity may override it. Incoming mail uses its receiving/storage account, while drafts and queued mail use their selected sending identity.
- **There are strong shipping precedents.** Thunderbird 128 added native account colors and a colored compose From field; eM Client 11, released **2026-10-07**, colors compose/reply headers. [Thunderbird 128](https://www.thunderbird.net/en-US/thunderbird/128.0esr/releasenotes/), [eM Client 11](https://www.emclient.com/blogpost.aspx?id=842).
- **Use native chrome plus a reliable strip.** macOS 26 supports `NSGlassEffectView.tintColor` and `NSToolbarItem.backgroundTintColor`, but Apple's HIG discourages extensive custom toolbar backgrounds. Stronger window tint should be an option. [Glass API](https://developer.apple.com/documentation/appkit/nsglasseffectview/tintcolor), [toolbar tint API](https://developer.apple.com/documentation/appkit/nstoolbaritem/backgroundtintcolor), [Toolbar HIG](https://developer.apple.com/design/human-interface-guidelines/toolbars).
- **GNOME can style each compose window independently.** Scope CSS to its window class; use header-bar variables or an account strip. Preserve the system accent for ordinary controls. [libadwaita appearance](https://gnome.pages.gitlab.gnome.org/libadwaita/doc/1.10/styles-and-appearance.html).
- **Color supplements sending safeguards.** Resolve replies from the original occurrence, preserve a restored draft's identity, warn on cross-account replies, and freeze identity when queuing. Wrong-From population frequency is **(unverified)**; documented failures establish the problem, not its prevalence. [Thunderbird identity-cue report](https://bugzilla.mozilla.org/show_bug.cgi?id=355520).
- **Start with seven chromatic Okabe–Ito colors and a custom picker.** Never promise unlimited distinguishability or use color alone. Keep names and monograms visible. [Okabe–Ito guidance](https://jfly.uni-koeln.de/color/).
- **Store appearance locally and export it.** IMAP METADATA can carry a private application convention, but no reviewed protocol supplies a portable account-color field. Flags, Gmail labels, and Graph categories have other meanings. [RFC 5464](https://www.rfc-editor.org/rfc/rfc5464.html), [RFC 8621](https://www.rfc-editor.org/rfc/rfc8621.html).
- **Ship accessibility with v1.** Honor appearance and contrast preferences, expose account/identity names to VoiceOver and Orca, and qualify actual rendered contrast. [Apple accessibility testing](https://developer.apple.com/documentation/accessibility/testing-system-accessibility-features-in-your-app), [GNOME accessibility](https://developer.gnome.org/hig/guidelines/accessibility.html).

## 2. Findings

### 2.1. Precedents: what the color actually identifies

#### Mail clients

All current-feature observations in this table are dated **2026-10-12**. Introduction dates are included where verified.

| Client | Verified mechanism and surfaces | Implication for Sweetbriar |
|---|---|---|
| **Thunderbird desktop** | Native account-color customization landed for **128**; **128.0 ESR, 2024-07-11**, explicitly added account colors in the compose From field. Account/folder-pane cues are separate from later mixed-message-list support. [Account-color implementation](https://bugzilla.mozilla.org/show_bug.cgi?id=1482172), [128 release notes](https://www.thunderbird.net/en-US/thunderbird/128.0esr/releasenotes/). | Treat sidebar, message list, and compose as separate required surfaces. |
| **Spark** | Account settings choose a color; its help explains identifying each email's account in the inbox. Desktop also has an Appearance toggle enabling account color coding. Full compose-window tint was not established **(unverified)**. [Spark account settings](https://sparkmailapp.com/help/add-manage-accounts/choose-the-account-name-title-and-color). | A configured color is ineffective if a second switch unexpectedly hides it. |
| **Airmail** | Documents a thin account-colored line inside the message panel's left edge in All Inboxes, plus account icons. Its identity guide describes a per-account Color Indicator and selectable account/alias From addresses; separate alias-color overrides remain **(unverified)**. [Appearance](https://help.airmailapp.com/en-us/article/preferences-appearance-airmail-for-macos-xob04q/), [identities](https://help.airmailapp.com/en-us/article/how-to-manage-multiple-email-identities-in-airmail-11rrpbj/). | Distinguish account-color support from identity-level overrides. |
| **Outlook, new Mac** | Profiles can have different theme colors. A profile is a work/personal context, potentially containing multiple accounts; this does not establish a compose tint that follows From. [Microsoft's Profiles announcement](https://techcommunity.microsoft.com/discussions/microsoft365insider/profiles-in-outlook-for-mac/3795584). | A profile theme cannot identify every sending identity in a multi-account profile. |
| **Outlook, new Windows** | Reviewed documentation establishes colored categories, while a user-selectable account/From window-color feature remains **(unverified)**. Categories appear in the inbox and open items. [Microsoft category documentation](https://support.microsoft.com/en-gb/outlook/mail/create-and-assign-color-categories-in-outlook). | Keep category colors separate from account appearance. |
| **eM Client** | **Version 11, released 2026-10-07**, assigns account colors in Accounts → General and displays them in the top bar/header of new-message and reply windows. Its preview explicitly positions this as wrong-account-send prevention. [Release announcement](https://www.emclient.com/blogpost.aspx?id=842), [account-color preview](https://www.emclient.com/blog/em-client-11-sneak-peek--account-colors-835). | The closest direct precedent for the owner's compose-window requirement. |
| **Postbox** | Its “Custom Colors for Accounts” recipe creates Topics per account, applies them through filters, and enables Topic coloring. This is a tagging workaround, not a distinct account-color setting. [Official recipe](https://support.postbox-inc.com/hc/en-us/articles/360045276134-Custom-Colors-for-Accounts). | Implement account appearance directly; avoid taxonomy side effects. |
| **Canary** | Unified View assigns account colors to vertical markers on the **right edge** of thread-list rows. The Mac article was updated **2025-09-25**. [Canary Mac help](https://canarymail.io/help/unified-view-macos). | Consistent row geometry can make a modest cue recognizable. |
| **Mailspring** | Account Label and Account Color distinguish conversations in its unified inbox. The account-color PR also covers sidebar, compose From, and contacts surfaces; it merged **2021-01-11**. [Current documentation](https://www.getmailspring.com/docs/setting-up-multiple-accounts), [implementation PR](https://github.com/Foundry376/Mailspring/pull/2240). | Appearance changes must reach existing windows and recycled rows. |
| **MailMate** | Supports mailbox Color and “Use Color in Message List”; its manual says the old `Styles.plist`/`MmMessageColorsEnabled` mechanism no longer works. Dedicated identity-following window tint remains **(unverified)**. [MailMate manual](https://manual.mailmate-app.com/hidden_preferences.html). | Mailbox coloring is useful, but its scope differs from a sending identity. |
| **Mimestream** | **1.0, 2023-05-22**, introduced account swatches on unified-inbox messages and account color as the default for sidebar labels. Profiles separately have icons/colors. Current help also offers account-name text in unified folders. [1.0 announcement](https://mimestream.com/blog/whats-new-in-1.0), [unified inbox](https://mimestream.com/help/user-guide/unified-inbox). | Especially relevant to a native Mac mail client; retain the textual option. |
| **Apple Mail** | Flags are per-message markers. A **2014-03-23** community recipe uses Account-based rules to color message backgrounds. A native account-color/compose-window-color setting in macOS 27 remains **(unverified)**. [Current flag guide](https://support.apple.com/en-ae/guide/mail/mlhlp1052/mac), [rule workaround](https://discussions.apple.com/thread/6024434). | “Flags only” is too narrow: rule coloring exists, but is not account chrome. |
| **Gmail web** | The API documents per-label foreground/background colors; Send mail as supplies selectable addresses. A built-in account-specific window/From color remains **(unverified)**. [Label schema](https://developers.google.com/workspace/gmail/api/reference/rest/v1/users.labels), [send-as help](https://support.google.com/mail/answer/22370?hl=en). | An alias and a label should not be mistaken for an account-color setting. |
| **Outlook web** | Colored categories are documented for the web client. Account-specific compose/window coloring remains **(unverified)** in reviewed material. [Microsoft documentation](https://support.microsoft.com/en-gb/outlook/mail/create-and-assign-color-categories-in-outlook). | Global themes and item categories do not establish identity-following color. |
| **Superhuman** | Its Account Colors announcement offers **12** choices through Cmd+K → Account Colors to distinguish the current account context. The consulted entry does not state its shipment date **(unverified)** or enumerate all compose surfaces. [Product announcement](https://new.superhuman.com/account-colors-136673). | Context color can be prominent without coloring message content. |

**Thunderbird's timeline needs precision.** The older **Account Colors extension** supported account/identity foreground and background customization across folder/thread panes, message headers, and compose headers. It is a distinct implementation from Thunderbird's native feature. The archive lists version 13.5 for Thunderbird 68–73. [Extension archive](https://services.addons.thunderbird.net/En-US/thunderbird/addon/account-colors/).

Native list support subsequently expanded: **153.0, released 2026-07-21**, documents account indicators and account-name tooltips in unified folders. The Cards-view work is tracked in bug 1882368. A table-view omission reported against Thunderbird 154 is now **RESOLVED FIXED**, targeting the **156 branch**, while its 153 ESR tracking flag is `wontfix`. It should be described as a historical/version-specific gap, not an open current defect. [153 release notes](https://www.thunderbird.net/en-US/thunderbird/153.0/releasenotes/), [Cards implementation](https://bugzilla.mozilla.org/show_bug.cgi?id=1882368), [table-view fix](https://bugzilla.mozilla.org/show_bug.cgi?id=2059950).

#### Classic clients: separate identity semantics from decorative color

The supplied classics studies were read first. They establish valuable identity behavior, but do not establish account-following compose-window color for all four clients.

| Client | What the snapshot establishes | Account/window-color conclusion |
|---|---|---|
| **Eudora** | Personalities bind identity and sending configuration; replies retain the receiving personality. Colored labels and text/background preferences are separate features. [Study](../../classics/eudora.md), [published settings resources](https://raw.githubusercontent.com/pmachapman/eudora-mac/master/SettingsTwo.rsrc.r). | Native personality-following window tint **(unverified)**. Preserve the personality semantics. |
| **Mailsmith** | Account-aware filters and clippings; colored labels and quote-depth colors. A contemporary firsthand review describes labeled-message coloring and urgent-mailbox coloring. [Study](../../classics/mailsmith.md), [January 1999 review](https://atpm.com/5.01/page8.shtml). | Account-specific compose/window tint **(unverified)**. |
| **Claris Emailer** | Explicit Send via account selection, multiple services, and named colored priorities in 2.0. [Study](../../classics/claris-emailer.md), [archived user guide](https://archive.org/details/claris-emailer-lite-manual). | Priorities are message semantics; account-following window tint **(unverified)**. |
| **Musashi** | The artifact study distinguishes window-type foreground/background preferences, seven message highlights, quotation colors, and classic appearance modules. Color scope was generally a kind of window, not a proven account identity. [Study](../../classics/musashi.md), [artifact findings](../../raw/musashi-artifacts/codex.md), [distribution archive](http://macintoshgarden.org/apps/musashi). | User/account-following tint **(unverified)**. Do not infer it from colorful screenshots. |

**Design inference:** Sweetbriar can combine classic account-aware composition with modern account cues without claiming to reproduce a specific historical window-color feature.

#### Outside mail

| Product | Verified color treatment | Useful lesson |
|---|---|---|
| **Chrome profiles** | A profile has a name, photo, and color theme; Chrome themes decorate the browser border. [Profile help](https://support.google.com/chrome/answer/2364824?hl=en), [theme help](https://support.google.com/chrome_webstore/answer/148695?hl=en). | Frame-scale color helps distinguish simultaneous windows. |
| **Edge profiles** | Microsoft confirmed **2021-03-24** that theme colors can differ per profile while light/dark is global. Its announcement explicitly frames colors as identifying personal/work/school contexts. [Microsoft discussion](https://techcommunity.microsoft.com/discussions/edgeinsiderannouncements/personalize-microsoft-edge-with-built-in-theme-colors/2230250/replies/2231786). | Theme mode and identity hue should be independent settings. |
| **Firefox Multi-Account Containers** | Color-coded tabs have editable container names, colors, and icons. [Mozilla help](https://support.mozilla.org/en-US/kb/containers). | Put the cue on the unit that owns the context: a tab can differ from its neighboring tab. |
| **Arc Spaces** | Space themes/colors and icons distinguish browsing areas; Spaces can be associated with Profiles. [Arc help](https://resources.arc.net/hc/en-us/articles/19228064149143-Spaces-Distinct-Browsing-Areas). | A workspace and an authenticated identity remain separate concepts. |
| **Slack workspaces** | Current help recommends different themes per workspace; themes follow the user across devices, and custom colors are personal. It also documents importing legacy themes. [Slack help](https://slack.com/help/articles/205166337-Change-your-Slack-theme). | Preserve established colors across redesigns and explain sync scope. |
| **macOS Terminal profiles** | Profiles customize a window's background, text, cursor, and other appearance; settings apply to the selected profile. This is content-background coloring, not proof of account-aware titlebar tint. [Profile guide](https://support.apple.com/en-lamr/guide/terminal/trml107/mac), [Text settings](https://support.apple.com/en-mt/guide/terminal/trmltxt/mac). | Strong window color has a native Mac precedent when it communicates context. |

**What users complain about, and what follows from it:**

- **Cues becoming too subtle:** K-9's message-list redesign drew complaints that a thinner account bar made accounts hard to distinguish. This is historical user evidence, not a claim about today's Android build. Sweetbriar should test cue recognition, not merely whether pixels are colored. [Issue 4110](https://github.com/thunderbird/thunderbird-android/issues/4110).
- **Inconsistent coverage:** Thunderbird's Cards/table split above shows that users notice when account settings fail to appear in their chosen list mode.
- **Stale colors after editing:** Mailspring's PR discussion identifies saved settings that did not rerender downstream components. Include live-update tests for open drafts and visible list rows. [PR discussion](https://github.com/Foundry376/Mailspring/pull/2240).
- **Palette limitations and accessibility:** Mozilla received requests for more container colors/icons and a specific colorblind-friendliness report. A fixed default palette should coexist with custom choice and textual identity. [Request 1566](https://github.com/mozilla/multi-account-containers/issues/1566), [Bug 1586960](https://bugzilla.mozilla.org/show_bug.cgi?id=1586960).
- **Redesigns altering familiar colors:** Slack's legacy-import feature acknowledges migration; the Catppuccin theme maintainers document limitations in reproducing their intended palette. Preserve canonical user choices separately from rendering changes. [Slack import guidance](https://slack.com/help/articles/205166337-Change-your-Slack-theme), [theme-maintainer account](https://github.com/catppuccin/slack).

These reports establish failure modes. They are not controlled evidence that any particular color design reduces errors by a known percentage.

### 2.2. Where color should appear: inventory and priorities

**Proposed semantic rule:** incoming color identifies the account holding the selected **occurrence**; outgoing color identifies the selected **sending identity**, inheriting its account color unless overridden. A conversation spanning accounts has several account cues. Its subject, sender, or latest message does not authorize assigning one account to the whole conversation. This follows the project's occurrence-based model. [Project decisions](../../00-summary.md).

The following inventory covers the main workflow, configuration, and desktop surfaces. **P0** is sending safety, **P1** is v1 account recognition, and **P2** is later expansion.

| Surface | Proposed treatment | Priority |
|---|---|---|
| **Compose window**, including reply/forward | Full-width tinted identity strip; monogram, account name, complete From address; optional stronger upper-window tint | P0 |
| From selector and identity menu | Matching swatch/monogram on every choice; selected address always visible | P0 |
| Drafts, **Out Box**, scheduled-send and retry details | Sending identity chip plus actual address; show frozen route and queue state separately | P0 |
| Send confirmation and cross-account warning | Spell out selected identity and original receiving account; use warning symbol/text distinct from account hue | P0 |
| Sidebar account roots and account-specific favorites | Color-bearing monogram beside account name; retain ordinary folder geometry | P1 |
| **Unified inbox rows** and other mixed-account lists | Persistent account marker plus short account label/monogram; same column in flat and threaded layouts | P1 |
| **Search results**, saved searches, unread/starred views | Reuse exactly the same row treatment; show account in results and scope chips | P1 |
| **Mixed-account threads** | Each message/occurrence gets its own cue; collapsed summary lists account chips/names | P1 |
| Reader header and standalone message window | Native “Received in Work” chip; separate from sender avatar, labels, and flags | P1 |
| Move/copy/archive destination chooser | Mark destination account and folder; explain a cross-account operation | P1 |
| Account settings, onboarding, account chooser | Consistent name/color/symbol preview and explicit identity inheritance | P1 |
| In-app alerts, sync errors, account-health view | Account cue identifies the affected account; status icon/text identifies the problem | P1 |
| **Notifications** | Account name in system-provided text; color only in supported image/icon content | P1 text; P2 imagery |
| Main account-specific browsing windows | Optional account strip/tint; unified multi-account windows remain neutral | P2 |
| **Window tabs** | Color/monogram accessory and account name; selected compose strip remains authoritative | P2 |
| Window menu and keyboard window switcher | Account-prefixed title; color accessory only where public UI allows | P1 title; P2 accessory |
| **Dock**, minimized windows, **menu bar** | Aggregate app count remains neutral; per-account submenu entries carry chips; custom account-specific tile optional | P2 |
| **Stage Manager / Mission Control** previews | Let the window's strip/tint and account-prefixed title survive scaling; test actual thumbnails | P1 consequence; P2 tuning |
| Templates, stationery, signatures, automation previews | Show the identity a template/action will choose before creating or sending a draft | P2 |
| Export/print/share previews and restored archives | Account name as provenance; color supplementary and never added to transmitted MIME | P2 |

**Selection is a separate meaning.** Keep native selection and focus treatments. On a selected row, retain an account label and outlined marker rather than replacing the selection background with the account color. In single-account views, the row label may be reduced after usability testing, but the visible account header and compose cue remain.

#### What the application controls

On **macOS**, the reviewed `UNMutableNotificationContent` interface exposes text, identifiers, sound, badge, and attachments, but no arbitrary banner-background or banner-text-color setting. Use the account name in `subtitle`/`body`, respect notification privacy preferences, and open the correct occurrence when activated. An attachment can contain color if displayed; it is not a guaranteed collapsed-banner cue. [Notification content API](https://developer.apple.com/documentation/usernotifications/unmutablenotificationcontent).

`NSDockTile` allows a custom content view for app/window tiles. Its normal window tile uses a miniaturized window image. `NSStatusItem.button` can host an image; a template image is intended for system treatment. These capabilities permit owned artwork, not recoloring the entire Dock or menu bar. [Dock API](https://developer.apple.com/documentation/appkit/nsdocktile), [status-item API](https://developer.apple.com/documentation/appkit/nsstatusitem), [template images](https://developer.apple.com/documentation/appkit/nsimage/istemplate).

No account-color customization hook for the **surrounding Stage Manager/Mission Control interface** was identified in the reviewed window APIs. The dependable application-owned surfaces are window content and titles; preview readability is a runtime acceptance question **(unverified)**.

On **GNOME**, GTK CSS controls application widgets, including client-side header bars. The notification portal supplies title, body, icon, actions, and presentation hints; it does not offer arbitrary CSS/background color. Backend presentation policy can affect icon visibility. Treat account text as the reliable cue and Activities previews as a consequence of the actual window content. [GNOME styling](https://developer.gnome.org/hig/guidelines/ui-styling.html), [notification portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Notification.html).

### 2.3. Compose behavior and protection against wrong-account sends

#### Evidence and its limits

Thunderbird's identity-cue bug, filed in **2006**, contains reports of sending from the wrong identity despite the From dropdown. A separate support report describes lasting confusion when correspondents remember the accidentally used address. These are direct reports of wrong-From behavior. **No defensible population frequency was found (unverified).** Wrong-recipient/data-leak statistics measure a different failure mode and are not substituted here. [Bug 355520](https://bugzilla.mozilla.org/show_bug.cgi?id=355520), [user report](https://support.mozilla.org/bm/questions/1191400).

Products mitigate different parts of the problem:

| Mechanism | Verified precedent | Limitation |
|---|---|---|
| Choose the receiving identity for replies/forwards | Thunderbird scans original recipients; Mimestream defaults Reply From to Received, including aliases. [Thunderbird](https://support.mozilla.org/en-US/kb/using-identities), [Mimestream](https://mimestream.com/help/user-guide/account-settings). | Delivery information may be incomplete or ambiguous. |
| Make From visually conspicuous | Thunderbird's colored From field; eM Client's colored compose header; BorderColors D's identity-colored compose border. [Thunderbird 128](https://www.thunderbird.net/en-US/thunderbird/128.0esr/releasenotes/), [eM Client](https://www.emclient.com/blogpost.aspx?id=842), [BorderColors D](https://github.com/dreadnaut/bordercolors-d). | Visibility still depends on attention and perception. |
| Require an explicit choice | Identity Chooser presents identities when composing. [Extension](https://services.addons.thunderbird.net/en-us/thunderbird/addon/identity-chooser/). | Repeated choices add friction. |
| Explain an automatic change | Mimestream's best-account selection uses recipients/history and displays an Updated label when From changes. [Composing settings](https://mimestream.com/help/user-guide/composing-settings). | Automatic selection can surprise someone already writing. |
| Warn for account/recipient-domain mismatch | Postbox Domain Fencing marks addresses when sending from an account to configured inappropriate domains, or outside allowed domains. [Official help](https://support.postbox-inc.com/hc/en-us/articles/204344204-Domain-Fencing). | A domain rule cannot infer every personal/work relationship. |

A specifically claimed **Mimestream cross-account reply warning remains (unverified)** after checking its help and release notes. Its documented receiving-address defaults and change indicator are verified. Gmail's send-as documentation also reports a forthcoming restriction: **starting January 2027**, third-party send-as addresses are no longer supported; Workspace aliases and other owned Gmail addresses are unaffected. Sweetbriar must qualify available identities against provider capabilities. [Mimestream settings](https://mimestream.com/help/user-guide/account-settings), [Gmail notice, checked 2026-10-12](https://support.google.com/mail/answer/22370?hl=en).

#### Proposed Sweetbriar behavior

1. **New message:** choose the account of an explicitly selected account/mailbox, otherwise a configured default. A new message invoked through `mailto:`, Share, Services, a menu-bar command, or automation uses the same resolver. If no valid choice exists, show “Choose a sending identity” and disable Send.
2. **Reply/Reply All:** resolve from the exact occurrence being replied to. Prefer a valid configured identity matching the original recipient/delivery information; otherwise use that occurrence's account default. Explain ambiguity. Replying to an already-sent message retains its valid recorded sending identity.
3. **Forward:** initially use the originating occurrence's account/identity context, while allowing deliberate changes. Show divergence clearly; do not assume a forward should trigger the same confirmation policy as a reply.
4. **Resume draft:** restore its durable identity. Changing the currently browsed mailbox or another window must not change it.
5. **Switch From:** update the address, account label, symbol, color strip, optional tint, tab/title, and selected send route in one view-model transition. Update signature, Reply-To, signing/encryption context, and Sent destination coherently; preserve edited body text and Undo.
6. **Send/queue:** validate authorization and show any applicable mismatch warning. Freeze account, identity, envelope sender, and message headers in the durable send attempt. Later UI selection cannot reroute queued mail. Missing accounts/credentials cause an explicit blocked state, never silent substitution.

**Default warning policy:** when replying through a different account from the original receiving account, show “Received in Work; sending from Personal — me@example.com” with **Use Work** and **Keep Personal** actions. Confirm an unacknowledged mismatch at Send. Remember acknowledgement for that draft's identity/recipient revision; invalidate it after relevant edits. Within one account, show alias differences as text and allow per-identity confirmation rules.

**Optional stricter policy:** ask for identity on every new compose, and allow per-account recipient-domain rules. Implement warnings once in the Rust send-validation path so keyboard shortcuts and automation receive the same result.

**Recommendation:** keep identity stable after the initial choice. Recipient history may suggest another identity, but should not silently replace a manually chosen identity. Color changes immediately when the user accepts a change. This is a Sweetbriar safety policy, not a claim that existing clients work this way.

### 2.4. macOS 26/27: implementation and native appearance

#### Current platform facts

Apple's current product page identifies **macOS 27 Golden Gate**, lists Apple-silicon-compatible Macs, and describes a Liquid Glass slider from ultraclear to fully tinted. Its **macOS 26** Appearance guide documents clear/tinted choices instead. The current macOS 27 update notes were published **2026-09-28**. Sweetbriar's macOS 26 floor remains viable for the APIs below. [macOS product page](https://www.apple.com/os/macos/), [26 Appearance settings](https://support.apple.com/en-gb/guide/mac-help/mchlp1225/26/mac/26), [27 update notes](https://support.apple.com/en-us/127257).

**Verification method:** Apple documentation pages and their public JSON metadata were fetched; availability records are retained in `evidence/apple/`. Appearance descriptions below come from documented behavior and Apple's design material. **No native macOS prototype was run**, so exact compositing, inactive-window rendering, and thumbnail legibility remain unverified.

| API/technique | Availability verified 2026-10-12 | What it does; suitability |
|---|---|---|
| `NSWindow.backgroundColor` with `titlebarAppearsTransparent` and `.fullSizeContentView` | Background property predates the floor; titlebar transparency introduced **10.10** | Transparent titlebar stops drawing its own background and reveals underlying content. This enables an owned tinted underlay; it does not prescribe the color of every Liquid Glass toolbar element. [Background](https://developer.apple.com/documentation/appkit/nswindow/backgroundcolor), [transparency](https://developer.apple.com/documentation/appkit/nswindow/titlebarappearstransparent). |
| `NSToolbarItem.style = .prominent` and `backgroundTintColor` | New toolbar treatment/tint in **26.0** | Tints an individual prominent toolbar item's glass. Use for an actual interactive From/account control, not every toolbar button or the whole bar. [Apple AppKit session](https://developer.apple.com/videos/play/wwdc2025/310/), [property](https://developer.apple.com/documentation/appkit/nstoolbaritem/backgroundtintcolor). |
| `NSGlassEffectView.tintColor` | **26.0** | Tints that view's background/glass toward a color. The result remains adaptive, rather than an exact flat sRGB swatch. Suitable for a bounded custom control; it is not a whole-window tint switch. [Class](https://developer.apple.com/documentation/appkit/nsglasseffectview), [tint](https://developer.apple.com/documentation/appkit/nsglasseffectview/tintcolor). |
| SwiftUI `toolbarBackground(_:for:)` | Shape-style overload **13.0** | Requests a preferred bar background, including `.windowToolbar`; the request can depend on container/bar support. Prefer `toolbarBackgroundVisibility(_:for:)`, available **15.0**, for visibility. [Background modifier](https://developer.apple.com/documentation/swiftui/view/toolbarbackground(_:for:)-5ybst), [visibility](https://developer.apple.com/documentation/swiftui/view/toolbarbackgroundvisibility(_:for:)). |
| SwiftUI `containerBackground(_:for: .window)` | Modifier **14.0**; **.window placement 15.0** | Sets the enclosing window background. Useful in a SwiftUI-owned window; an opaque editor can cover the tinted content area. It is distinct from toolbar-item glass tint. [Modifier](https://developer.apple.com/documentation/swiftui/view/containerbackground(_:for:)), [placement](https://developer.apple.com/documentation/swiftui/containerbackgroundplacement/window). |
| SwiftUI `glassEffect(.regular.tint(color))` | `glassEffect`/`Glass.tint` **26.0** | Adaptive colored glass around the modified view. Use a regular variant for legibility; do not layer custom glass over a toolbar control already backed by glass. [Effect](https://developer.apple.com/documentation/swiftui/view/glasseffect(_:in:)), [tint](https://developer.apple.com/documentation/swiftui/glass/tint(_:)). |
| `NSTitlebarAccessoryViewController` | **10.10** | Public way to add an account strip beneath/beside the titlebar. `layoutAttribute = .bottom` uses the horizontal accessory layout; AppKit handles placement/full-screen changes. [Accessory API](https://developer.apple.com/documentation/appkit/nstitlebaraccessoryviewcontroller). |
| `NSWindow.tab.accessoryView` | **10.13** | Adds a custom view beside a window-tab title. A monogram/swatch can identify an inactive compose tab without replacing native tab chrome. [Tab accessory](https://developer.apple.com/documentation/appkit/nswindowtab/accessoryview). |

Apple's AppKit presentation explains that toolbar controls are grouped on adaptive glass, which may switch appearance according to underlying content. Informational toolbar items should have `isBordered = false` so they do not look like buttons. Therefore the color of the From control and the reliable identity strip should not depend solely on the toolbar's compositing. [WWDC25 AppKit session](https://developer.apple.com/videos/play/wwdc2025/310/).

#### HIG guidance and system settings

The current Toolbar HIG recommends reducing custom backgrounds/tinted controls because they can interfere with system effects. Color guidance recommends applying color sparingly to glass, especially where emphasis has meaning. Materials guidance places Liquid Glass chiefly in the controls/navigation layer and reserves clear glass for suitable rich backgrounds. These are design constraints, not a prohibition on account identification. [Toolbars](https://developer.apple.com/design/human-interface-guidelines/toolbars), [Color](https://developer.apple.com/design/human-interface-guidelines/color), [Materials](https://developer.apple.com/design/human-interface-guidelines/materials).

| Setting | Required Sweetbriar response |
|---|---|
| **Dark Mode / Auto** | Recompute surface, marker, and foreground tokens; retain the account's canonical hue. |
| **Increase Contrast** | Strengthen outlines, names, and separators; qualify custom pairs instead of assuming system controls repair them. |
| **Reduce Transparency** | Use an opaque identity strip and avoid requiring glass translucency to identify the account. |
| **Clear/tinted Liquid Glass preference** | Let standard materials adapt; retain a solid marker so changing system glass does not erase account identity. |
| **Accent color / text highlight color** | Keep system selection and ordinary control accent semantics independent from account hue. |
| **Wallpaper window tinting** | Keep the explicit account swatch/band distinct from incidental wallpaper-derived color. |
| **Differentiate Without Color** | Keep names always; add stronger monograms/symbols and outlines automatically. |

These settings and their contrast effects are documented by Apple. The accessibility flags are available through `NSWorkspace`, with `accessibilityDisplayOptionsDidChangeNotification` for changes. SwiftUI islands can use the corresponding accessibility environment values. [Appearance settings](https://support.apple.com/en-gb/guide/mac-help/mchlp1225/26/mac/26), [accessibility testing](https://developer.apple.com/documentation/accessibility/testing-system-accessibility-features-in-your-app), [Differentiate Without Color API](https://developer.apple.com/documentation/appkit/nsworkspace/accessibilitydisplayshoulddifferentiatewithoutcolor).

**Recommended native design:** standard traffic lights, title, toolbar, and neutral editor; immediately below the toolbar, a full-width **32-point minimum** identity strip, allowed to grow with text size. Give it a restrained derived background and a saturated monogram/marker, with semantic text reading **Work · Alex <alex@company.example>**. The From control remains visible and keyboard-accessible. A custom background-drawing `NSView` inside a bottom titlebar accessory supplies the stable surface; each `NSWindowController` binds it to that draft's identity.

An optional **Strong window tint** extends the derived hue behind the upper window using `backgroundColor`/transparent titlebar/full-size content, while keeping the message editor neutral. Do not traverse private titlebar subviews or recolor the traffic lights. This satisfies prominent window identification while retaining native structure. The strip is the guaranteed account cue; the stronger glass appearance must be qualified on both supported OS generations.

The AppKit installation point is straightforward; this is illustrative research code, not a compiled prototype:

```swift
let accessory = NSTitlebarAccessoryViewController()
accessory.layoutAttribute = .bottom
accessory.view = identityStripView
window.addTitlebarAccessoryViewController(accessory)

// Update identityStripView from this draft's selected identity.
// A future tab cue can use window.tab.accessoryView.
```

Use an `NSPopUpButton` for the From selector and account swatches in `NSMenuItem.image`, with full addresses in item titles. These are proposed native controls; the strip and selector observe the same identity model. [Pop-up button](https://developer.apple.com/documentation/appkit/nspopupbutton), [menu-item image](https://developer.apple.com/documentation/appkit/nsmenuitem/image).

For SwiftUI islands, keep the window's identity in the owning AppKit view model. Applying `.tint(accountColor)` throughout an entire view hierarchy would also affect ordinary controls; use bounded surfaces/control tint explicitly instead.

### 2.5. Linux: GTK4/libadwaita and the Qt/KDE fallback

**Verified 2026-10-12:** libadwaita's documentation served version **1.10.0**. CSS custom-property support and widget-specific accent overrides were introduced in the **1.6 generation**, documented by its maintainer on **2024-09-13**. The recommendation uses these capabilities, rather than globally redefining old named colors. [Current appearance documentation](https://gnome.pages.gitlab.gnome.org/libadwaita/doc/1.10/styles-and-appearance.html), [1.6 announcement](https://blogs.gnome.org/alicem/2024/09/13/libadwaita-1-6/).

Use an `AdwApplicationWindow` with `AdwToolbarView`/`AdwHeaderBar` and a separate full-width identity strip immediately below the header, **32 logical pixels minimum**, expanding with text size. Bind it to the draft's selected identity through the native Rust view model. The `gtk4` and `libadwaita` crates remain the project's selected bindings. [Linux architecture](../../09-linux.md).

A `GtkCssProvider` registered through `gtk_style_context_add_provider_for_display()` can contain selectors scoped by `gtk_widget_add_css_class()` on each window. **Per-widget `gtk_style_context_add_provider()` is deprecated since GTK 4.10**; it is not the recommended solution for inherited window styling. [GTK provider documentation](https://docs.gtk.org/gtk4/method.StyleContext.add_provider.html), [GtkCssProvider](https://docs.gtk.org/gtk4/class.CssProvider.html).

Use `GtkDropDown` for From selection, with custom `GtkListItemFactory` representations for the chosen identity and popup rows. Show monogram/color, account name, and full address in both; bind `selected-item` to the same draft identity as the strip. GTK documents separate factories and optional popup search. [GtkDropDown](https://docs.gtk.org/gtk4/class.DropDown.html).

For optional upper-window tint, scope `--headerbar-bg-color`, `--headerbar-fg-color`, and **`--headerbar-backdrop-color`** together. The latter controls inactive appearance, so leaving it untouched can erase the cue when another window gains focus. Keep the editor's `--view-bg-color`/foreground semantic and neutral. [Header-bar variables](https://gnome.pages.gitlab.gnome.org/libadwaita/doc/1-latest/css-variables.html).

Illustrative generated CSS for a light appearance:

```css
window.compose.account-blue {
  --headerbar-bg-color: #eaf2f7;
  --headerbar-backdrop-color: #eaf2f7;
  --headerbar-fg-color: #1d1d20;
}
window.compose.account-blue .account-strip {
  background-color: #eaf2f7;
  color: #1d1d20;
}
```

These example colors are proposed tokens, not a qualified palette. Generate dark/high-contrast counterparts from the same canonical account color. Resource styles such as `style-dark.css`, `style-hc.css`, and `style-hc-dark.css`, or supported appearance media queries, can apply variants; monitor `AdwStyleManager:dark` and `:high-contrast`. Libadwaita's high-contrast preference cannot be disabled by the app. [Styles and Appearance](https://gnome.pages.gitlab.gnome.org/libadwaita/doc/1.10/styles-and-appearance.html).

**Can the accent be overridden per window?** Yes, by a window-scoped CSS subtree. Widget-specific overrides must maintain `--accent-bg-color`, `--accent-color`, and a suitable `--accent-fg-color`; `AdwStyleManager` still reports the **system** accent, not the CSS override. **Recommendation:** define Sweetbriar account tokens and leave ordinary controls on the system accent. Account identity, suggested action, and destructive action should remain distinguishable. [Accent contract](https://gnome.pages.gitlab.gnome.org/libadwaita/doc/1.10/styles-and-appearance.html).

GNOME recommends minimal custom styling, semantic variables, and explicit high-contrast testing. A conspicuous account strip is a contained exception with a clear purpose. Preserve header dragging space, native controls, keyboard navigation, and responsive layout. [Styling HIG](https://developer.gnome.org/hig/guidelines/ui-styling.html), [header-bar HIG](https://developer.gnome.org/hig/patterns/containers/header-bars.html).

**If Qt/KDE is selected:** draw the same strip inside the application. Qt Widgets offers `QWidget::setPalette`/`QPalette` roles and active/inactive/disabled groups, though styles can treat palette roles differently. Kirigami offers `Theme.colorSet`, `Theme.inherit`, background/text colors, and a Header color set. Scope appearance to the identity strip, follow system colors elsewhere, and test Breeze light/dark/high contrast. Server-side decorations remain a window-manager concern; do not make their tint a dependency. [QPalette](https://doc.qt.io/qt-6/qpalette.html), [Qt style behavior](https://doc.qt.io/qt-6/style-drawing.html), [Kirigami Theme](https://api.kde.org/qml-org-kde-kirigami-platform-theme.html).

### 2.6. Color design, contrast, and semantic separation

#### Palette and automatic assignment

There is no universal number of account colors distinguishable in every size, display, lighting condition, and form of color-vision deficiency. **Seven is a proposed default palette size, not a human-perception limit.**

Okabe–Ito's original palette contains black plus seven chromatic colors; its guidance emphasizes redundant coding and substantial markers. Tol offers a seven-color bright scheme, nine-color muted scheme, and three-color high-contrast scheme, and distinguishes strong line/marker colors from background palettes. ColorBrewer provides qualitative palettes and a colorblind-safe filter. [Okabe–Ito](https://jfly.uni-koeln.de/color/), [Tol's current notes](https://sronpersonalpages.nl/~pault/), [ColorBrewer](https://colorbrewer2.org/).

**Proposed default assignment order:**

| Order | Picker name | Canonical opaque sRGB |
|---|---|---|
| 1 | Blue | `#0072B2` |
| 2 | Orange | `#E69F00` |
| 3 | Bluish green | `#009E73` |
| 4 | Reddish purple | `#CC79A7` |
| 5 | Sky blue | `#56B4E9` |
| 6 | Vermilion | `#D55E00` |
| 7 | Yellow | `#F0E442` |

Coordinates were checked against the [authors' original palette image](https://jfly.uni-koeln.de/color/image/pallete.jpg). Offer a neutral choice separately.

Assign the next unused default when an account is created, and persist it immediately. Never recolor existing accounts when adding/removing/reordering accounts or updating the palette. Beyond seven, reuse a least-used slot with a distinct account label/symbol and invite customization. Do not hash provider names into colors: two Gmail accounts must be distinguishable.

**Strongest palette counter-argument:** scientific-chart palettes are not evidence that tiny mail badges or pale glass surfaces will be distinguishable. Answer with a substantial saturated marker, textual identity, custom choice, and tests of the *rendered* design. Tol's palettes are credible alternatives if those tests favor them.

#### One stored color, several rendering roles

Store one opaque sRGB value. Derive separate tokens for **marker**, **light strip**, **dark strip**, **foreground**, and **high-contrast outline**. Use perceptual lightness/chroma adjustment with gamut mapping; do not persist a new account preference when Dark Mode changes. The Rust `palette` crate provides `Srgb`, `Oklab`, and `Oklch` conversions; the consulted documentation identifies **0.7.6**. Color-space clamping alone does not guarantee a valid sRGB gamut result. [Crate documentation](https://docs.rs/palette/latest/palette/), [Oklab/gamut notes](https://docs.rs/palette/latest/palette/struct.Oklab.html).

Prefer opaque strips for reliable contrast. Keep text semantic; if a monogram is drawn directly on a saturated swatch, calculate its foreground against that actual fill. Re-resolve layer-backed colors when appearance changes. Keep the stored choice intact even when its displayed lightness needs adjustment.

#### Contrast targets

Use **WCAG 2.2 as an engineering baseline**, without claiming that adopting its numerical thresholds alone certifies a native application:

- Ordinary text: **4.5:1** minimum.
- Large text: **3:1** minimum under WCAG's large-text definition.
- Essential non-text indicators/control boundaries: **3:1** against adjacent colors.
- Never convey identity or warning solely through hue.

These are SC 1.4.3, 1.4.11, and 1.4.1. [WCAG 2.2](https://www.w3.org/TR/WCAG22/), [non-text explanation](https://www.w3.org/WAI/WCAG22/Understanding/non-text-contrast).

A local calculation using WCAG's relative-luminance formula gives white text on unmodified Orange **2.25:1**, Yellow **1.32:1**, and Blue **5.19:1**. Black on Blue is only **4.05:1**. Thus neither “white on every account color” nor “black on every account color” works. Results are saved in `evidence/palette-contrast.csv`. [Contrast definition](https://www.w3.org/TR/WCAG22/#dfn-contrast-ratio).

APCA is useful as a supplementary readability diagnostic because its guidance considers font size/weight and polarity. Its author recommends **Lc 75** as a minimum for specified body-text sizes, with stronger contrast preferred; apply its font guidance rather than one universal Lc cutoff. It does **not** replace WCAG 2.2 checks. As of **2026-10-12**, WCAG 3 remains a **Working Draft dated 2026-09-10**. [APCA guidance](https://git.apcacontrast.com/documentation/APCA_in_a_Nutshell.html), [current WCAG 3 draft](https://www.w3.org/TR/wcag-3.0/).

Glass tint is not a fixed foreground/background pair. Test actual pixels over bright, dark, and colorful backgrounds, or put identity text on an opaque strip.

#### Avoid confusing account color with other meanings

**RFC 9979**, published **May 2026**, is **Informational**. It registers seven flag colors—red, orange, yellow, green, blue, purple, gray—using `$MailFlagBit0`, `$MailFlagBit1`, and `$MailFlagBit2` in conjunction with `\Flagged`. It describes message flags, not account appearance. [RFC 9979](https://www.rfc-editor.org/rfc/rfc9979.html).

Gmail labels have foreground/background colors; Graph categories have up to **25** preset color values. These classify items. [Gmail schema](https://developers.google.com/workspace/gmail/api/reference/rest/v1/users.labels), [Graph categories](https://learn.microsoft.com/en-us/graph/api/resources/outlookcategory?view=graph-rest-1.0).

**Proposed visual grammar:** account = monogram/chip or fixed account rail; flag = flag glyph; label/category = named tag chip; warning/error = status symbol plus explanatory text. Keep provider logos separate. Hue overlap is inevitable; distinct geometry and text prevent semantic ambiguity. Allow a symbol or emoji, but retain an accessible account name and disambiguate identical monograms.

### 2.7. Data model, durability, and sync

**Recommendation:** account appearance belongs in Sweetbriar's durable SQLite settings, with an optional identity override. Keep rendering native and sending semantics in Rust. This extends D14/D23/D29 without making color a provider-owned message property. [Project decisions](../../00-summary.md), [metadata agenda](../../notes/decision-agenda.md).

Proposed logical records:

```text
AccountAppearance:
  account_id              stable UUID
  color_srgb              opaque #RRGGBB
  symbol                  optional portable identifier/text
  assignment              palette version + slot, or custom
  revision                local monotonic revision

IdentityAppearance:
  identity_id             stable UUID
  color_override_srgb     nullable; null = inherit account
  symbol_override         optional
  revision                local monotonic revision
```

Resolve **incoming** appearance through occurrence → account. Resolve **outgoing** appearance through draft/send attempt → identity → account. Do not infer receiving account from the message's From address or collapse provenance because Message-ID matches.

Persist settings transactionally and acknowledge saving only after the core's durable receipt. Include account/identity appearance in coordinated backups and a versioned JSON settings export with an explicit `colorSpace: "srgb"`. Import maps stable/provider account references with user review where ambiguous. Retain enough account metadata for archived occurrences after an account is disconnected; do not recycle IDs. Platform color archives and GTK CSS strings are presentation artifacts, not the cross-platform format.

| Protocol/provider | Can it carry Sweetbriar account color across devices? |
|---|---|
| **IMAP METADATA, RFC 5464** | Potentially: private vendor annotations can attach to a mailbox or the server using the empty mailbox name. This requires server capability/access and an application convention. Vendor tokens must follow the RFC's IANA registration rules. There is no standard account-color entry. [RFC](https://www.rfc-editor.org/rfc/rfc5464.html). |
| **JMAP Mail, RFC 8621** | Standard Identity fields include name, email, Reply-To/Bcc, signatures, and permissions; no account/identity color is defined there. A server-supported extension could add a convention, but is not portable baseline behavior. [Identity schema](https://www.rfc-editor.org/rfc/rfc8621.html). |
| **Microsoft Graph** | Category color is item taxonomy. Graph's open/schema extensions can hold application data on supported resources, potentially enabling a Sweetbriar convention, but add resource/permission and synchronization dependencies. [Category schema](https://learn.microsoft.com/en-us/graph/api/resources/outlookcategory?view=graph-rest-1.0), [extension overview](https://learn.microsoft.com/en-us/graph/extensibility-overview). |
| **Gmail API** | Reviewed label/sendAs schemas provide label colors and sending-identity settings, not an account-window-color field. Repurposing label color would change mailbox taxonomy. [Labels](https://developers.google.com/workspace/gmail/api/reference/rest/v1/users.labels), [sendAs](https://developers.google.com/workspace/gmail/api/reference/rest/v1/users.settings.sendAs). |

**v1 sync policy:** colors are **per machine**, included in backup/export, and clearly labeled “This device.” Later optional Sweetbriar settings sync can share stable account/identity appearance across Mac/Linux while allowing a device override. Do not make provider metadata availability necessary to preserve color or drafts.

**Strongest counter-argument:** learned colors are more useful when identical on every device. Manual export is a compromise, not the ideal endpoint. Reserve stable IDs and revisions now so later settings sync can address it without changing account semantics.

### 2.8. Accessibility and qualification

**VoiceOver:** use native controls and accessible names such as “From, Alex at company.example, Work account.” Mark purely decorative swatches as redundant. On an intentional From change, expose the updated value and, when necessary, post one concise `NSAccessibility` announcement through `post(element:notification:userInfo:)`. Do not announce every repaint or move keyboard focus. [AppKit accessibility notification API](https://developer.apple.com/documentation/appkit/nsaccessibility-swift.struct/post(element:notification:userinfo:)).

**Orca:** provide GTK accessible labels/descriptions for account chips, row provenance, and From selection. `gtk_accessible_update_property()` updates semantics; `gtk_accessible_announce()`, available since **4.14**, supports announcements without interrupting current speech when appropriate priority is used. [GTK announcement API](https://docs.gtk.org/gtk4/method.Accessible.announce.html), [GNOME naming/testing guidance](https://developer.gnome.org/hig/guidelines/accessibility.html).

**Differentiate Without Color:** text is always present; the setting strengthens symbols/outlines. **Increase Contrast:** strengthen essential boundaries and qualify foreground pairs. **Reduce Transparency:** retain an opaque account strip. On GNOME, support high contrast independently from light/dark. [Apple preferences API](https://developer.apple.com/documentation/appkit/nsworkspace/accessibilitydisplayshoulddifferentiatewithoutcolor), [libadwaita appearance](https://gnome.pages.gitlab.gnome.org/libadwaita/doc/1.10/styles-and-appearance.html).

Proposed release checks:

1. Keyboard-only and screen-reader completion of new/reply/forward, From switching, warning acknowledgement, queue inspection, and color settings.
2. VoiceOver/Orca testing with the display hidden; long/localized/RTL names and duplicate monograms.
3. Light/dark, active/inactive windows, increased contrast, reduced transparency, and system accent variants.
4. Protanopia, deuteranopia, tritanopia, grayscale, and low-contrast simulation of *rendered screenshots*, including selected rows and thumbnails; then evaluation with people who have color-vision differences.
5. Two simultaneous compose windows; rapid identity changes; reopened drafts; palette edits; offline saving; restart/restore; queued sends remaining on the frozen route.

Apple's Accessibility Inspector supplies inspection/auditing and contrast calculation. Firefox's developer accessibility tools can simulate vision deficiencies for a screenshot review page. macOS corrective color filters should not be assumed equivalent to simulating another person's vision. [Apple Inspector](https://developer.apple.com/documentation/accessibility/accessibility-inspector), [Mozilla simulation guidance](https://developer.mozilla.org/en-US/docs/Web/Accessibility/Guides/Colors_and_Luminance), [Okabe–Ito simulation cautions](https://jfly.uni-koeln.de/color/).

## 3. Options compared

The table is a design assessment, not measured comparative usability evidence.

| Option | Visibility | Native integration / cost | Decision |
|---|---|---|---|
| Sidebar/list dots only | Good while browsing; weak during composition | Small implementation | Insufficient for the owner's compose requirement |
| Color the From control only | Directly tied to identity; easy to overlook | Native, compact | Supporting cue |
| **Full-width identity strip + native chrome** | Strong, stable, includes exact address | Public APIs; contained custom styling | **Default** |
| Full upper-window/titlebar tint | Strong peripheral/window recognition | More appearance and contrast qualification | **Optional Strong mode** |
| Fully tinted editor/message rows | Large colored area | Competes with selection, flags, and reading contrast | Do not make it the default |
| Ask identity on every compose/send | Explicit choice | More interruptions | Optional strict mode |

## 4. Concrete specification for Sweetbriar

**v1 scope:** P0/P1 surfaces in §2.2. In particular, compose and From menus, sidebar roots, mixed-account list/search rows, reader provenance, mixed-thread headers, drafts/Out Box, account settings, warnings, and notification **text**. Keep the account-prefixed window title from v1. Later add native tab accessories, richer Dock/menu-bar presentation, account-specific browsing-window tint, and synchronized settings.

**macOS compose:** AppKit `NSWindowController` owns a draft-specific identity model; `NSTitlebarAccessoryViewController` with bottom layout hosts the **32-point minimum** tinted strip. The standard toolbar retains native Liquid Glass. A bounded interactive From item can use `NSToolbarItem.backgroundTintColor`; the strip remains visible without it. Strong mode uses a tinted upper-window underlay through `NSWindow.backgroundColor`, `titlebarAppearsTransparent`, and `.fullSizeContentView`, with a neutral editor.

**Linux compose:** `AdwApplicationWindow`/`AdwToolbarView`/`AdwHeaderBar` plus the same named identity strip. `GtkCssProvider` and window-scoped classes bind generated account tokens independently per window. Strong mode sets the header background/foreground/**backdrop** variables. High contrast switches to a qualified opaque strip and outlined marker; ordinary action colors remain semantic.

**Defaults:** color coding enabled; seven-color assignment order in §2.6; identities inherit their account; explicit selected/default identity for new mail; receiving identity for replies; stable identity after manual selection; cross-account reply confirmation enabled. Standard strip is default; Strong window tint is available from v1. No telemetry is required to implement or assess the feature.

**Settings UI:** Accounts → Appearance shows account name, complete address, palette radio choices with names/checkmarks, Custom…, symbol/monogram, and light/dark compose previews. Identity settings default to “Use account color” and expose an override. General Appearance offers **Standard strip / Strong window tint / Minimal marker**, with the full From address retained in every mode. Sending Safety contains cross-account confirmation, optional always-choose-identity, and domain rules. State that color is stored on this device and included in export.

Use `NSColorWell` or SwiftUI `ColorPicker` on Mac and `GtkColorDialog`/`GtkColorDialogButton` on Linux, with opacity disabled and sRGB/hex validation. GTK's modern chooser APIs are available since **4.10**. Preview custom colors with generated contrast-safe text/outline, and explain any rendering adjustment rather than rejecting a user's preferred hue. [Apple color well](https://developer.apple.com/documentation/appkit/nscolorwell), [SwiftUI picker](https://developer.apple.com/documentation/swiftui/colorpicker), [GTK chooser](https://docs.gtk.org/gtk4/class.ColorDialog.html), [GTK chooser button](https://docs.gtk.org/gtk4/class.ColorDialogButton.html).

**Strongest overall counter-argument:** a tinted mail window can look less native, weaken toolbar contrast, and compete with system selection or warning colors; Apple's current toolbar guidance supports that concern. The answer is a **contained, meaningful account surface**, native window/control behavior, neutral reading/editing areas, and an optional stronger treatment. The requirement is prominent account identification; implementation should survive changing system materials. [Toolbar HIG](https://developer.apple.com/design/human-interface-guidelines/toolbars).

**Second counter-argument:** color may create false confidence while the wrong identity is still selected. The answer is exact address text, predictable resolution, mismatch checks, and a frozen send route. Treat color as recognition assistance rather than a correctness guarantee.

## 5. Risks and open questions

- **Native rendering remains unverified.** Qualify macOS 26 and 27, GNOME/Ubuntu/Flatpak, inactive windows, accessibility settings, and real Stage Manager/Mission Control/Activities thumbnails.
- **Recognition is not yet measured.** Test common 2–4-account use and users with many accounts; compare Standard and Strong modes. Measure identity recognition and observed task mistakes without claiming a known reduction today.
- **Alias/delegation semantics:** define authorized aliases, catch-all domains, shared-mailbox Send As/On Behalf Of, and messages present in several accounts. An appearance chip cannot resolve authorization ambiguity.
- **Mixed threads:** require provenance before enabling cross-account conversation aggregation; duplicate occurrences must remain identifiable.
- **Warning fatigue:** validate the acknowledgement policy on deliberate cross-account replies and normal forwarding; provide strict mode for users who prefer it.
- **Cross-device continuity:** per-machine defaults are consistent with the project direction, but optional settings sync needs explicit conflict/device-override semantics later.
- **Historical/current gaps:** classic account-following window tint, exact Superhuman shipment date, and the claimed Mimestream cross-account reply warning remain **(unverified)**. Do not promote bounded searches into universal absence claims.

## 6. Sources consulted

All web sources below were consulted on **2026-10-12**. Historical dates refer to the original material; current behavior is qualified in the report. Apple API availability was additionally checked against public documentation JSON retained under `evidence/apple/`; the summary is in [apple-api-summary.jsonl](codex-artifacts/evidence/apple-api-summary.jsonl).

The local research documents linked throughout §2 were also read; they are the project's supplied evidence, not independent proof of current product behavior.

### Mail clients and sending safety

- [Thunderbird 128](https://www.thunderbird.net/en-US/thunderbird/128.0esr/releasenotes/)
- [eM Client 11](https://www.emclient.com/blogpost.aspx?id=842)
- [Thunderbird identity-cue report](https://bugzilla.mozilla.org/show_bug.cgi?id=355520)
- [Account-color implementation](https://bugzilla.mozilla.org/show_bug.cgi?id=1482172)
- [Spark account settings](https://sparkmailapp.com/help/add-manage-accounts/choose-the-account-name-title-and-color)
- [Appearance](https://help.airmailapp.com/en-us/article/preferences-appearance-airmail-for-macos-xob04q/)
- [identities](https://help.airmailapp.com/en-us/article/how-to-manage-multiple-email-identities-in-airmail-11rrpbj/)
- [Microsoft's Profiles announcement](https://techcommunity.microsoft.com/discussions/microsoft365insider/profiles-in-outlook-for-mac/3795584)
- [Microsoft category documentation](https://support.microsoft.com/en-gb/outlook/mail/create-and-assign-color-categories-in-outlook)
- [account-color preview](https://www.emclient.com/blog/em-client-11-sneak-peek--account-colors-835)
- [Official recipe](https://support.postbox-inc.com/hc/en-us/articles/360045276134-Custom-Colors-for-Accounts)
- [Canary Mac help](https://canarymail.io/help/unified-view-macos)
- [Current documentation](https://www.getmailspring.com/docs/setting-up-multiple-accounts)
- [implementation PR](https://github.com/Foundry376/Mailspring/pull/2240)
- [MailMate manual](https://manual.mailmate-app.com/hidden_preferences.html)
- [1.0 announcement](https://mimestream.com/blog/whats-new-in-1.0)
- [unified inbox](https://mimestream.com/help/user-guide/unified-inbox)
- [Current flag guide](https://support.apple.com/en-ae/guide/mail/mlhlp1052/mac)
- [rule workaround](https://discussions.apple.com/thread/6024434)
- [send-as help](https://support.google.com/mail/answer/22370?hl=en)
- [Product announcement](https://new.superhuman.com/account-colors-136673)
- [Extension archive](https://services.addons.thunderbird.net/En-US/thunderbird/addon/account-colors/)
- [153 release notes](https://www.thunderbird.net/en-US/thunderbird/153.0/releasenotes/)
- [Cards implementation](https://bugzilla.mozilla.org/show_bug.cgi?id=1882368)
- [table-view fix](https://bugzilla.mozilla.org/show_bug.cgi?id=2059950)
- [published settings resources](https://raw.githubusercontent.com/pmachapman/eudora-mac/master/SettingsTwo.rsrc.r)
- [January 1999 review](https://atpm.com/5.01/page8.shtml)
- [archived user guide](https://archive.org/details/claris-emailer-lite-manual)
- [distribution archive](http://macintoshgarden.org/apps/musashi)
- [Mozilla help](https://support.mozilla.org/en-US/kb/containers)
- [Issue 4110](https://github.com/thunderbird/thunderbird-android/issues/4110)
- [notification portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Notification.html)
- [user report](https://support.mozilla.org/bm/questions/1191400)
- [Thunderbird](https://support.mozilla.org/en-US/kb/using-identities)
- [Mimestream](https://mimestream.com/help/user-guide/account-settings)
- [BorderColors D](https://github.com/dreadnaut/bordercolors-d)
- [Extension](https://services.addons.thunderbird.net/en-us/thunderbird/addon/identity-chooser/)
- [Composing settings](https://mimestream.com/help/user-guide/composing-settings)
- [Official help](https://support.postbox-inc.com/hc/en-us/articles/204344204-Domain-Fencing)

### Browser and workspace precedents

- [Profile help](https://support.google.com/chrome/answer/2364824?hl=en)
- [theme help](https://support.google.com/chrome_webstore/answer/148695?hl=en)
- [Microsoft discussion](https://techcommunity.microsoft.com/discussions/edgeinsiderannouncements/personalize-microsoft-edge-with-built-in-theme-colors/2230250/replies/2231786)
- [Arc help](https://resources.arc.net/hc/en-us/articles/19228064149143-Spaces-Distinct-Browsing-Areas)
- [Slack help](https://slack.com/help/articles/205166337-Change-your-Slack-theme)
- [Profile guide](https://support.apple.com/en-lamr/guide/terminal/trml107/mac)
- [Text settings](https://support.apple.com/en-mt/guide/terminal/trmltxt/mac)
- [Request 1566](https://github.com/mozilla/multi-account-containers/issues/1566)
- [Bug 1586960](https://bugzilla.mozilla.org/show_bug.cgi?id=1586960)
- [theme-maintainer account](https://github.com/catppuccin/slack)

### Apple platform and accessibility

- [Glass API](https://developer.apple.com/documentation/appkit/nsglasseffectview/tintcolor)
- [toolbar tint API](https://developer.apple.com/documentation/appkit/nstoolbaritem/backgroundtintcolor)
- [Toolbar HIG](https://developer.apple.com/design/human-interface-guidelines/toolbars)
- [Apple accessibility testing](https://developer.apple.com/documentation/accessibility/testing-system-accessibility-features-in-your-app)
- [Notification content API](https://developer.apple.com/documentation/usernotifications/unmutablenotificationcontent)
- [Dock API](https://developer.apple.com/documentation/appkit/nsdocktile)
- [status-item API](https://developer.apple.com/documentation/appkit/nsstatusitem)
- [template images](https://developer.apple.com/documentation/appkit/nsimage/istemplate)
- [macOS product page](https://www.apple.com/os/macos/)
- [26 Appearance settings](https://support.apple.com/en-gb/guide/mac-help/mchlp1225/26/mac/26)
- [27 update notes](https://support.apple.com/en-us/127257)
- [Background](https://developer.apple.com/documentation/appkit/nswindow/backgroundcolor)
- [transparency](https://developer.apple.com/documentation/appkit/nswindow/titlebarappearstransparent)
- [Apple AppKit session](https://developer.apple.com/videos/play/wwdc2025/310/)
- [Class](https://developer.apple.com/documentation/appkit/nsglasseffectview)
- [Background modifier](https://developer.apple.com/documentation/swiftui/view/toolbarbackground(_:for:)-5ybst)
- [visibility](https://developer.apple.com/documentation/swiftui/view/toolbarbackgroundvisibility(_:for:))
- [Modifier](https://developer.apple.com/documentation/swiftui/view/containerbackground(_:for:))
- [placement](https://developer.apple.com/documentation/swiftui/containerbackgroundplacement/window)
- [Effect](https://developer.apple.com/documentation/swiftui/view/glasseffect(_:in:))
- [tint](https://developer.apple.com/documentation/swiftui/glass/tint(_:))
- [Accessory API](https://developer.apple.com/documentation/appkit/nstitlebaraccessoryviewcontroller)
- [Tab accessory](https://developer.apple.com/documentation/appkit/nswindowtab/accessoryview)
- [Color](https://developer.apple.com/design/human-interface-guidelines/color)
- [Materials](https://developer.apple.com/design/human-interface-guidelines/materials)
- [Differentiate Without Color API](https://developer.apple.com/documentation/appkit/nsworkspace/accessibilitydisplayshoulddifferentiatewithoutcolor)
- [AppKit accessibility notification API](https://developer.apple.com/documentation/appkit/nsaccessibility-swift.struct/post(element:notification:userinfo:))
- [Apple Inspector](https://developer.apple.com/documentation/accessibility/accessibility-inspector)
- [Mozilla simulation guidance](https://developer.mozilla.org/en-US/docs/Web/Accessibility/Guides/Colors_and_Luminance)
- [Apple color well](https://developer.apple.com/documentation/appkit/nscolorwell)
- [SwiftUI picker](https://developer.apple.com/documentation/swiftui/colorpicker)
- [NSPopUpButton](https://developer.apple.com/documentation/appkit/nspopupbutton)
- [NSMenuItem.image](https://developer.apple.com/documentation/appkit/nsmenuitem/image)

### GTK, GNOME, Qt, and KDE

- [libadwaita appearance](https://gnome.pages.gitlab.gnome.org/libadwaita/doc/1.10/styles-and-appearance.html)
- [GNOME accessibility](https://developer.gnome.org/hig/guidelines/accessibility.html)
- [GNOME styling](https://developer.gnome.org/hig/guidelines/ui-styling.html)
- [1.6 announcement](https://blogs.gnome.org/alicem/2024/09/13/libadwaita-1-6/)
- [GTK provider documentation](https://docs.gtk.org/gtk4/method.StyleContext.add_provider.html)
- [GtkCssProvider](https://docs.gtk.org/gtk4/class.CssProvider.html)
- [Header-bar variables](https://gnome.pages.gitlab.gnome.org/libadwaita/doc/1-latest/css-variables.html)
- [header-bar HIG](https://developer.gnome.org/hig/patterns/containers/header-bars.html)
- [QPalette](https://doc.qt.io/qt-6/qpalette.html)
- [Qt style behavior](https://doc.qt.io/qt-6/style-drawing.html)
- [Kirigami Theme](https://api.kde.org/qml-org-kde-kirigami-platform-theme.html)
- [GTK announcement API](https://docs.gtk.org/gtk4/method.Accessible.announce.html)
- [GTK chooser](https://docs.gtk.org/gtk4/class.ColorDialog.html)
- [GTK chooser button](https://docs.gtk.org/gtk4/class.ColorDialogButton.html)
- [GtkDropDown](https://docs.gtk.org/gtk4/class.DropDown.html)

### Protocols and provider schemas

- [RFC 5464](https://www.rfc-editor.org/rfc/rfc5464.html)
- [RFC 8621](https://www.rfc-editor.org/rfc/rfc8621.html)
- [Label schema](https://developers.google.com/workspace/gmail/api/reference/rest/v1/users.labels)
- [RFC 9979](https://www.rfc-editor.org/rfc/rfc9979.html)
- [Graph categories](https://learn.microsoft.com/en-us/graph/api/resources/outlookcategory?view=graph-rest-1.0)
- [extension overview](https://learn.microsoft.com/en-us/graph/extensibility-overview)
- [sendAs](https://developers.google.com/workspace/gmail/api/reference/rest/v1/users.settings.sendAs)

### Palette and contrast

- [Okabe–Ito guidance](https://jfly.uni-koeln.de/color/)
- [Tol's current notes](https://sronpersonalpages.nl/~pault/)
- [ColorBrewer](https://colorbrewer2.org/)
- [authors' original palette image](https://jfly.uni-koeln.de/color/image/pallete.jpg)
- [Crate documentation](https://docs.rs/palette/latest/palette/)
- [Oklab/gamut notes](https://docs.rs/palette/latest/palette/struct.Oklab.html)
- [WCAG 2.2](https://www.w3.org/TR/WCAG22/)
- [non-text explanation](https://www.w3.org/WAI/WCAG22/Understanding/non-text-contrast)
- [Contrast definition](https://www.w3.org/TR/WCAG22/#dfn-contrast-ratio)
- [APCA guidance](https://git.apcacontrast.com/documentation/APCA_in_a_Nutshell.html)
- [current WCAG 3 draft](https://www.w3.org/TR/wcag-3.0/)