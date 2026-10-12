# Topic: Color-coding accounts

The project owner says: "One feature I need is to color-code accounts and
show these colors clearly (e.g. as the window color for new mail windows)."
This is a requirement, not a question of whether. Research how to do it
well.

A snapshot of the project's research is in `./research/` (start with
`research/00-summary.md`). Relevant: identities as first-class (D14), the
macOS UI stack (AppKit shell with SwiftUI islands, macOS 26 floor,
`research/08-macos-ui.md`), the Linux UI (GTK4/libadwaita,
`research/09-linux.md`), per-machine metadata (D23), accessibility (D29),
Apple's flag colours (RFC 9979, `research/07-standards.md`) and the classic
client studies (`research/classics/`).

## Questions

1. **Precedents.** Which mail clients color-code accounts or identities,
   and how: Thunderbird (which version added account colors, where they
   show), Spark, Airmail, Outlook (new Mac and Windows), eM Client,
   Postbox, Canary, Mailspring, MailMate, Mimestream, Apple Mail (flags
   only?), Gmail and Outlook web, Superhuman, and the classic clients
   (Eudora personalities, Mailsmith, Claris Emailer, Musashi; check the
   snapshot's classics documents first). Outside mail: Chrome and Edge
   profiles (window frame color), Firefox Multi-Account Containers (tab
   color), Arc spaces, Slack workspaces, macOS Terminal profiles. What
   works, what users complain about (cite bug trackers and forums).
2. **Where the color should appear** in a mail client, as an exhaustive
   list, then prioritized: sidebar, unified inbox rows, mixed-account
   threads, reader header, **compose window**, Out Box, notifications,
   Dock and menu bar, search results, window tabs, Stage Manager and
   Mission Control thumbnails. What can and cannot be colored on macOS
   (for example notification banners) and on GNOME.
3. **The compose window.** The color follows the selected From identity:
   new message, reply, forward, and live changes when the user switches
   From. Safety value: preventing mail sent from the wrong account (find
   evidence of how common wrong-account sends are and how products
   mitigate them, such as warnings when replying across accounts).
4. **macOS implementation** on macOS 26/27 with Liquid Glass:
   - How to tint a window's title bar and toolbar in AppKit and SwiftUI:
     `NSWindow.backgroundColor` with `titlebarAppearsTransparent`,
     toolbar appearance under Liquid Glass, `NSGlassEffectView` tint, SwiftUI
     `toolbarBackground`, `containerBackground(for: .window)`,
     `glassEffect(.regular.tint(...))`, a titlebar accessory strip, window
     tab accessories. Verify each API exists on macOS 26 and how it looks.
   - Apple HIG guidance on custom window or toolbar colors and tinting
     glass. The system appearance settings that interact: Dark Mode,
     Increase Contrast, Reduce Transparency, the clear/tinted Liquid Glass
     option, accent color, Differentiate Without Color.
   - The clearest legible design that still looks native.
5. **Linux implementation**: GTK4/libadwaita per-window styling (header
   bar background via CSS classes, libadwaita accent-color variables and
   whether an app can override them per window), GNOME HIG guidance on
   custom colors, high-contrast mode; what Qt/KDE would need if the Qt
   fallback is chosen.
6. **Color design**:
   - Palette: how many distinguishable colors, color-vision-deficiency
     safe palettes (Okabe-Ito, Paul Tol and others), automatic assignment,
     a custom picker, light and dark variants derived from one stored
     color.
   - Contrast for text on tinted surfaces (WCAG 2.2, APCA).
   - Never color alone: pair with the account name, a monogram, symbol or
     emoji.
   - Avoid confusion with flag colours (RFC 9979), Gmail label colours,
     Graph category colours and provider brand colours.
7. **Data model and sync**: where the color lives (account, with an
   identity-level override?), stored locally (SQLite settings, part of
   export); whether any protocol can carry it across devices (IMAP
   METADATA RFC 5464, JMAP, Graph, Gmail) or whether it stays per machine.
8. **Accessibility**: VoiceOver/Orca announcements, Differentiate Without
   Color, Increase Contrast, testing with color-vision-deficiency
   simulation.
9. **Recommendation**: a concrete specification for Sweetbriar:
   - where colors appear in v1 vs later;
   - exactly how the compose window shows the account on macOS and on
     Linux, with API names;
   - the palette and defaults;
   - the settings UI;
   - the strongest counter-argument (for example that a tinted window
     looks un-Mac-like) and how to answer it.
