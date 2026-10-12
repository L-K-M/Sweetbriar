# 15 — Account colors

Synthesis of [raw/account-colors/codex.md](raw/account-colors/codex.md)
and [raw/account-colors/devin.md](raw/account-colors/devin.md), plus facts
Claude checked by hand. Checked 2026-10-12.

The owner's requirement: "color-code accounts and show these colors
clearly (e.g. as the window color for new mail windows)." This document
says how.

## Summary

- **The compose window carries the sending account.** Below the native
  title bar and toolbar sits a full-width identity strip: account color,
  monogram, account name and the full From address. Its color follows the
  selected From identity and changes the moment the user switches From.
  An optional **Strong** mode also tints the upper window. The editor
  always stays neutral.
- **Incoming and outgoing use different keys.**
  - A received message shows the account that holds that copy of it
    (its occurrence, D2).
  - A draft or queued message shows its selected sending identity, which
    inherits its account's color unless overridden.
  - A conversation spanning accounts shows one cue per message, never a
    single color for the whole thread.
- **Color never works alone.** Every account cue carries a label or
  symbol that distinguishes it from every other account's. Where two
  accounts share a monogram, a short label or address is shown instead.
  So the cue survives color blindness, grayscale and Differentiate
  Without Color. Color is recognition, not proof. The
  safeguards are the full address, predictable reply identity, a
  cross-account check and a frozen send route.
- **Precedent is strong.** eM Client 11 shows the account color "in the
  top bar of new message and reply windows" (announced 2026-08-06).
  Thunderbird 128 colors the compose From field (2024-07-11).
  [Spark](https://sparkmailapp.com/help/add-manage-accounts/choose-the-account-name-title-and-color),
  [Airmail](https://help.airmailapp.com/en-us/article/preferences-appearance-airmail-for-macos-xob04q/),
  [Canary](https://canarymail.io/help/unified-view-macos),
  [Mailspring](https://github.com/Foundry376/Mailspring/pull/2240) and
  [Mimestream](https://mimestream.com/help/user-guide/unified-inbox) mark
  unified-inbox rows.
  No equivalent account- or From-following compose cue was verified for
  Apple Mail or desktop Outlook (unverified as an absence). Apple Mail users
  color messages with rules; Outlook for Mac colors whole profiles
  (groups of accounts), not the sending identity.
- **One stored color per account, rendered many ways.** A single sRGB
  value lives in SQLite. The Rust core derives marker, strip, dark-mode,
  foreground and high-contrast tokens from it. Colors are per machine in
  v1 and included in backup and export. No mail protocol carries an
  account color.

## Why it matters: sending from the wrong account

Neither researcher found a population-level rate of wrong-From sends. The
failure is still well documented:
- **Thunderbird:**
  - bug 355520 (2006) asks for a compose-window identity cue;
  - bug 704613 reports wrong-identity replies with eight accounts;
  - a support thread is titled "Accidentally sending emails from the
    wrong account".
- **Outlook:** forum threads, and a VBA send-guard offered as a
  workaround.
- **Postbox:** built "Domain Fencing" (2015) for exactly this.
- **eM Client 11** markets its account colors as a way to "never send an
  email from the wrong email address".
- **Slack** recommends a different theme per workspace for the same
  reason.

Nobody re-reads their own From line. An ambient cue in the window chrome
is how people sense context before reading details, as Terminal profiles
and Chrome profile colors already show.

## Where the color appears

**P0** protects sending, **P1** completes recognition in v1, **P2** comes
later.

| Surface | Treatment | Priority |
|---|---|---|
| **Compose window** (new, reply, forward) | Identity strip with monogram, account name, full From address; optional Strong tint | P0 |
| From selector | Swatch and monogram on every choice; selected address always visible | P0 |
| Drafts, Out Box, scheduled and retrying sends | Identity chip and the actual address; the frozen route shown separately from queue state | P0 |
| Cross-account warning and send confirmation | Names both the receiving account and the sending identity, with a warning symbol and text distinct from any account hue | P0 |
| Sidebar account roots | Colored monogram next to the account name | P1 |
| Unified inbox and other mixed-account lists, search results | A fixed account marker plus a short label or monogram, in the same column in flat and threaded layouts; a short label when monograms collide | P1 |
| Mixed-account threads | A cue on each message; the collapsed summary lists account chips | P1 |
| Reader header, standalone message window | "Received in Work" chip, separate from sender avatar, labels and flags | P1 |
| Move/copy destination picker, account settings, account health surface | Account chip; cross-account operations explained in text | P1 |
| Window title | Account-prefixed title (survives the Window menu, Mission Control, Stage Manager) | P1 |
| Notifications | Account name in the system-drawn text only; neither macOS nor GNOME lets apps color banners | P1 (text) |
| Window tabs | `NSWindowTab.accessoryView` swatch on macOS; tab styling on GTK | P2 |
| Dock, menu bar | Per-account entries in menus; the Dock badge is text-only and can't be tinted per account | P2 |
| Account-specific browsing windows | Optional tint; unified windows stay neutral | P2 |

**Selection keeps its own meaning.** A selected row keeps the native
selection background, plus its account marker and label. The account
color never replaces selection or focus colors.

## Compose behaviour

1. **New message:** use the account of the selected mailbox, else the
   configured default. `mailto:`, Share, Services and automation use the
   same resolver. With no valid choice, show "Choose a sending identity"
   and disable Send.
2. **Reply and Reply All:**
   - **To a message you sent:** restore its recorded sending identity
     first. With aliases A and B in one account, replying to mail sent
     from A uses A, not the account default B. If that identity no longer
     exists, require a visible choice.
   - **To received mail:** resolve from the exact copy being replied to,
     its occurrence (Eudora's rule: reply from the personality that
     received it). Prefer a configured identity matching the original
     recipient or delivery headers, else that account's default.
   - Explain any ambiguity.
3. **Forward:** start from the original message's account, and show any
   change clearly.
4. **Resuming a draft:** restore its saved identity. What the user
   browses elsewhere never changes it.
5. **Switching From:** in one transition, update everything that depends
   on the identity:
   - the address, label, monogram, strip, Strong tint, tab and title;
   - the signature, Reply-To, signing/encryption context and Sent
     destination.
   The body is untouched and Undo still works. The color crossfades in
   about 150 ms, or instantly with Reduce Motion.
6. **Only sendable identities are offered.** The From list contains only
   identities the account backend has validated as sendable, including
   aliases and Send As or On Behalf Of grants. The strip displays that
   choice and never extends it.
7. **Send and queue:** freeze the account, identity, envelope sender and
   headers in the durable send attempt (D3). Changing the selection later
   can't reroute queued mail. A missing account or credential blocks the
   send visibly; nothing is silently substituted.

**Cross-account check (on by default):** when a reply goes out through a
different account than the one that received the original, the window
shows "Received in Work; sending from Personal (me@example.com)" with
**Use Work** and **Keep Personal**.
- An unacknowledged mismatch is confirmed at Send.
- Acknowledgement is remembered for that draft until the identity or
  recipients change.
- The check runs in the Rust send-validation path, so keyboard shortcuts
  and automation get the same result.

**Later:** domain-fencing warnings learned from correspondents (Postbox's
precedent, Thunderbird bug 688147's idea), and an optional strict mode
that asks for the identity on every new message. Recipient history may
suggest an identity, but never silently replaces one the user chose.

## macOS implementation (macOS 26/27)

All APIs below exist on the macOS 26 floor; Claude checked availability
against Apple's documentation metadata. No native prototype has been run
yet, so the look of compositing, inactive windows and thumbnails is
unverified.

```
┌──────────────────────────────────────────────────────┐
│ ●●●   New Message               [From: Work ▾]   ➤   │  native title bar + toolbar (Liquid Glass)
├──────────────────────────────────────────────────────┤
│ W  Work · Alex Example <alex@company.example>        │  identity strip, ≥ 32 pt, account color
├──────────────────────────────────────────────────────┤
│ To:                                                  │
│ Subject:                                             │
│                                                      │  neutral editor
└──────────────────────────────────────────────────────┘
```

- **Identity strip (default):**
  - Built from a custom-drawn `NSView` in an `NSTitlebarAccessoryViewController`
    with `layoutAttribute = .bottom`. AppKit handles placement and tabs.
  - **Full screen:** a bottom titlebar accessory is hidden with the menu
    bar in full screen by default. Set `fullScreenMinHeight` to the strip's
    height, and update it when text or layout grows, so the strip stays
    visible. Test with two compose windows in full screen.
  - At least 32 pt tall, growing with text size.
  - **Opaque, not glass:** the background is a restrained tint derived
    from the account color, with a saturated monogram. Opaque fills give
    computable contrast and survive Reduce Transparency and the
    clear/tinted Liquid Glass setting (Codex). Tinted glass shifts with
    the content behind it, so its contrast can't be guaranteed.
  - Each `NSWindowController` binds the strip to its own draft's identity.
- **From control:**
  - An `NSPopUpButton` with a swatch in each `NSMenuItem.image` and full
    addresses as item titles.
  - In the toolbar, it may be an `NSToolbarItem` with `style = .prominent`
    and `backgroundTintColor` (macOS 26). That is a supporting cue; the
    strip works without it.
- **Strong mode (optional):**
  - Tints the upper window through `NSWindow.backgroundColor`,
    `titlebarAppearsTransparent` and `.fullSizeContentView`.
  - The editor stays neutral.
  - No private titlebar subviews, and the traffic lights are never
    recolored.
- **Tabs:** an `NSWindowTab.accessoryView` swatch identifies inactive
  compose tabs. Tinting a whole tab needs private API, so it's avoided.
- **SwiftUI islands:** take the identity from the owning AppKit view
  model. Don't apply `.tint(accountColor)` to whole hierarchies, because
  that recolors ordinary controls. Use bounded surfaces, or
  `glassEffect(.regular.tint(_:))` for a chip.
- **System settings to honor:**

  | Setting | Response |
  |---|---|
  | Dark Mode | Use the derived dark tokens |
  | Increase Contrast | Stronger outlines and separators |
  | Reduce Transparency | Strip stays opaque (it already is) |
  | Clear/tinted Liquid Glass | Strip unaffected |
  | Accent color | Untouched: controls keep the system accent |
  | Differentiate Without Color (`NSWorkspace.accessibilityDisplayShouldDifferentiateWithoutColor`) | Stronger monograms, symbols and outlines; a distinguishing label is always shown |

- **Apple's guidance cuts both ways.** The Toolbar HIG discourages custom
  toolbar backgrounds; the Color HIG reserves tint for meaning. That is
  why the default leaves the toolbar native and puts the account in a
  contained strip of its own. The sending account is meaning.

## Linux implementation (GTK4/libadwaita)

- **Layout:** an `AdwApplicationWindow` with `AdwToolbarView`,
  `AdwHeaderBar` and the same identity strip below the header bar
  (≥ 32 logical px).
- **From selector:** a `GtkDropDown` whose list-item factories show the
  monogram, color, account name and address.
- **Styling:**
  - Register one `GtkCssProvider` for the display and put a window-scoped
    class on each compose window (`gtk_widget_add_css_class`).
  - Don't use per-widget `gtk_style_context_add_provider`, which is
    deprecated since GTK 4.10. Devin's report suggested it; Codex's
    correction is adopted.
- **Strong mode:** sets `--headerbar-bg-color`, `--headerbar-fg-color`
  **and** `--headerbar-backdrop-color` for that window class. Without
  the backdrop variable, the cue disappears when the window loses focus.
- **Accent:**
  - libadwaita allows a per-widget accent override (`--accent-bg-color`,
    `--accent-color`, `--accent-fg-color`), using the CSS variables
    introduced in libadwaita 1.6 ([maintainer's 1.6 announcement](https://blogs.gnome.org/alicem/2024/09/13/libadwaita-1-6/),
    2024-09-13). Sweetbriar
    doesn't use it: ordinary controls keep the system accent, and
    `AdwStyleManager` keeps reporting the system value anyway.
  - Variants come from `style-dark.css`, `style-hc.css` and
    `style-hc-dark.css`, or from the `AdwStyleManager` dark and
    high-contrast properties. The app can't turn off high contrast.
- **Qt/KDE fallback:** the same strip drawn in-app with a scoped
  `QPalette` or Kirigami `Theme`. Server-side decorations stay the window
  manager's business.

## Color design

**Default palette:** seven chromatic Okabe–Ito colors, assigned in order
to new accounts, plus a separate neutral choice and a custom picker.

| Order | Name | sRGB |
|---|---|---|
| 1 | Blue | `#0072B2` |
| 2 | Orange | `#E69F00` |
| 3 | Bluish green | `#009E73` |
| 4 | Reddish purple | `#CC79A7` |
| 5 | Sky blue | `#56B4E9` |
| 6 | Vermilion | `#D55E00` |
| 7 | Yellow | `#F0E442` |

**Assignment:**
- A color is persisted when the account is created.
- Existing accounts are never recolored when others are added, removed
  or reordered, or when the palette changes.
- Beyond seven accounts, reuse the least-used color and rely on the
  monogram, inviting the user to customize.
- Never derive colors from the provider: two Gmail accounts must differ.
  A brand color may only be a suggestion for the first account of a
  provider.
- Seven is a default, not a limit of human perception. Tol's bright and
  muted schemes are fallbacks if tests of the rendered design favor them.

**Derived tokens:**
- The core converts the stored color to OKLCH (the Rust `palette` crate),
  then clamps lightness and chroma per role, with gamut mapping:
  - list marker;
  - light strip and dark strip;
  - strip foreground;
  - high-contrast outline.
- Nothing extra is stored when Dark Mode changes.
- This avoids Apple Mail's failure, where rule colors become
  unrecognizable in Dark Mode.

**Contrast:**
- **Targets** (WCAG 2.2 as the engineering baseline):
  - 4.5:1 for strip text;
  - 3:1 for large text and essential markers;
  - never identity by hue alone.
- **Neither black nor white works on every palette color.** White on
  Orange is 2.25:1, white on Yellow 1.32:1, white on Blue 5.19:1, and
  black on Blue only 4.05:1 (Codex's calculation). The core picks the
  foreground per derived surface.
- **APCA** serves only as a supplementary readability check.

**Visual grammar,** so hues can overlap without confusion:

| Meaning | Shape |
|---|---|
| Account | Monogram, chip or a fixed account rail |
| Flag (RFC 9979's seven colors) | Flag glyph |
| Gmail label, Graph category | Named tag chip |
| Warning or error | Status symbol plus text |

Provider logos stay separate. The picker warns, without refusing, when a
custom color is close to another account's.

## Data model and sync

```text
AccountAppearance   account_id, color_srgb (#RRGGBB), symbol?, assignment (palette version + slot | custom), revision
IdentityAppearance  identity_id, color_override_srgb? (null = inherit), symbol_override?, revision
```

- **Resolution:** incoming goes occurrence → account; outgoing goes draft
  or send attempt → identity → account. A receiving account is never
  inferred from the From address or a matching Message-ID.
- **Storage:**
  - Stored in SQLite, acknowledged after the durable commit (D1).
  - Included in backup generations and in a versioned JSON settings
    export with `"colorSpace": "srgb"`.
  - Retained for disconnected accounts whose archived mail remains.
- **Sync:** none in v1. Colors are labelled "This device" (D23).
  - IMAP METADATA (RFC 5464) could carry a private vendor annotation on
    servers that support it, but Gmail and Exchange Online don't. Possible
    later seeding, never a dependency.
  - JMAP Identity, Gmail `sendAs` and labels, and Graph categories have no
    account-color field. Repurposing label or category colors would change
    the user's mailbox.

## Accessibility

- **Screen readers:**
  - The From control's accessible name is the full identity, for example
    "From, Alex at company.example, Work account".
  - Swatches are marked decorative.
  - An intentional From change posts one announcement: an `NSAccessibility`
    notification on macOS, `gtk_accessible_announce` on GTK (4.14+).
  - Focus doesn't move.
- **Differentiate Without Color, Increase Contrast, Reduce Transparency and
  GNOME high contrast** are handled as above. A distinguishing label or
  symbol is always visible, so color is never the only carrier. The full
  account identity is exposed to accessibility APIs.
- **Release checks:**
  - keyboard-only and screen-reader runs of new, reply, forward, From
    switching, warning acknowledgement, the Out Box and color settings;
  - long, localized and right-to-left names, and duplicate monograms;
  - light and dark, active and inactive windows, contrast and transparency
    settings;
  - protanopia, deuteranopia, tritanopia and grayscale simulation of
    rendered screenshots (including selected rows and thumbnails), then
    tests with people who have color-vision differences;
  - two compose windows, rapid identity changes, reopened drafts, palette
    edits, restart and restore, and queued sends staying on their frozen
    route.

## Settings

- **Accounts → Appearance:**
  - palette swatches with names, plus Custom… (`NSColorWell` or SwiftUI
    `ColorPicker`; `GtkColorDialogButton`; opacity disabled);
  - a monogram or symbol;
  - light and dark compose previews;
  - a note: "Stored on this device; included in export."
- **Identities:** "Use account color" by default, with an override.
- **General → Appearance:** **Standard strip** (default), **Strong window
  tint**, or **Minimal marker**. The full From address shows in every
  mode.
- **Sending safety:**
  - cross-account check (on);
  - always choose the identity (off);
  - per-account domain rules (later).

## v1 and later

- **v1:** all P0 and P1 surfaces; Standard and Strong modes on both
  platforms; the cross-account check; per-machine storage with export.
- **Later:**
  - tab accessories;
  - menu-bar and Dock presentation;
  - tinted account-specific browsing windows;
  - learned domain-fencing warnings;
  - profile colors, if profiles ship;
  - optional settings sync (stable IDs and revisions are reserved now);
  - IMAP METADATA seeding.

## Conflicts between the reports

| Topic | Devin | Codex | Resolution |
|---|---|---|---|
| Strip material | Tinted `NSGlassEffectView`, a thin 3–4 pt strip plus label | Opaque custom view, ≥ 32 pt | Opaque: computable contrast, robust to transparency settings and Liquid Glass changes. Glass tint only on the bounded From chip |
| GTK styling | Per-widget `gtk_style_context_add_provider` | Deprecated since 4.10; display provider plus window CSS classes | Codex |
| Palette | 8 Okabe–Ito including gray | 7 chromatic + separate neutral | Same colors; neutral offered separately |
| Wrong-account warning | Learned correspondent warning, on by default | Deterministic cross-account reply check, on by default | Deterministic check in v1; learned warnings later |
| Superhuman | "No per-account color" | Ships Account Colors with 12 choices (product announcement; date unverified) | Codex cites the announcement; Superhuman counted as a precedent |

## Strongest counter-argument

"A tinted mail window looks un-Mac-like, and Apple's toolbar guidance
discourages custom backgrounds." The default leaves the toolbar and
editor native and puts the account in one contained strip with text.
That is the same shape as eM Client 11's top bar. Strong mode is for
users who want the Terminal-profile look. If a future macOS changes
Liquid Glass again, the opaque strip survives.

A second counter-argument: color can create false confidence while the
wrong identity is selected. That is why the full address, the reply
resolver, the cross-account check and the frozen send route carry the
safety. The color helps the user notice.

## Open

- Native rendering on macOS 26 and 27, GNOME/Ubuntu Flatpak, inactive
  windows and real Stage Manager, Mission Control and Activities
  thumbnails.
- Recognition tests with 2–4 accounts and with many, comparing Standard
  and Strong modes.
- Alias, catch-all, shared-mailbox Send As and On Behalf Of semantics:
  the strip shows the identity but can't settle authorization.
- Warning fatigue on deliberate cross-account replies.
- A hands-on look at eM Client 11's top bar, the closest shipping design.

## Sources

All checked 2026-10-12. The raw reports carry many more.

- **Precedents:**
  - [eM Client 11 account colors](https://www.emclient.com/blog/em-client-11-sneak-peek--account-colors-835), [Thunderbird 128 release notes](https://www.thunderbird.net/en-US/thunderbird/128.0esr/releasenotes/), [Thunderbird 153 release notes](https://www.thunderbird.net/en-US/thunderbird/153.0/releasenotes/);
  - [Thunderbird bug 355520](https://bugzilla.mozilla.org/show_bug.cgi?id=355520), [Postbox Domain Fencing](https://support.postbox-inc.com/hc/en-us/articles/204344204-Domain-Fencing), [Outlook for Mac profiles](https://techcommunity.microsoft.com/discussions/microsoft365insider/profiles-in-outlook-for-mac/3795584).
- **AppKit and SwiftUI** (availability via Apple's documentation metadata):
  - [`NSGlassEffectView.tintColor`](https://developer.apple.com/documentation/appkit/nsglasseffectview/tintcolor), [`NSToolbarItem.backgroundTintColor`](https://developer.apple.com/documentation/appkit/nstoolbaritem/backgroundtintcolor);
  - [`NSTitlebarAccessoryViewController`](https://developer.apple.com/documentation/appkit/nstitlebaraccessoryviewcontroller), [`fullScreenMinHeight`](https://developer.apple.com/documentation/appkit/nstitlebaraccessoryviewcontroller/fullscreenminheight), [`NSWindowTab.accessoryView`](https://developer.apple.com/documentation/appkit/nswindowtab/accessoryview);
  - [`titlebarAppearsTransparent`](https://developer.apple.com/documentation/appkit/nswindow/titlebarappearstransparent), [`toolbarBackground(_:for:)`](https://developer.apple.com/documentation/swiftui/view/toolbarbackground(_:for:)), [`glassEffect(_:in:)`](https://developer.apple.com/documentation/swiftui/view/glasseffect(_:in:));
  - [Differentiate Without Color](https://developer.apple.com/documentation/appkit/nsworkspace/accessibilitydisplayshoulddifferentiatewithoutcolor), [Reduce Transparency](https://developer.apple.com/documentation/appkit/nsworkspace/accessibilitydisplayshouldreducetransparency);
  - HIG: [Toolbars](https://developer.apple.com/design/human-interface-guidelines/toolbars), [Color](https://developer.apple.com/design/human-interface-guidelines/color).
- **GTK and libadwaita:**
  - [`gtk_style_context_add_provider` (deprecated 4.10)](https://docs.gtk.org/gtk4/method.StyleContext.add_provider.html), [libadwaita CSS variables](https://gnome.pages.gitlab.gnome.org/libadwaita/doc/1.10/css-variables.html);
  - [libadwaita styles and appearance](https://gnome.pages.gitlab.gnome.org/libadwaita/doc/1.10/styles-and-appearance.html), [`gtk_accessible_announce`](https://docs.gtk.org/gtk4/method.Accessible.announce.html).
- **Color and contrast:**
  - [Okabe–Ito palette](https://jfly.uni-koeln.de/color/), [WCAG 2.2](https://www.w3.org/TR/WCAG22/), [WCAG use of color](https://www.w3.org/WAI/WCAG22/Understanding/use-of-color.html);
  - [RFC 9979](https://www.rfc-editor.org/rfc/rfc9979.html), [RFC 5464](https://www.rfc-editor.org/rfc/rfc5464.html). Claude recomputed the four contrast ratios from WCAG's relative-luminance formula.
