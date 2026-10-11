<!-- provenance: helper=glm model=zai-coding-plan/glm-5.3 started=2026-10-11T18:14:20+00:00 exit=0 brief=research/briefs/ -->

# Sweetbriar on Linux: a native-feeling front end over a shared Rust core

Research report for the Sweetbriar email client project, 2026-10-11.
All version numbers and dates below were verified against live web sources on
2026-10-11 unless marked **(unverified)**. Items marked *Opinion* are the
author's assessment, not sourced fact.

---

## 1. TL;DR

- **GTK 4 + libadwaita, written in Rust on gtk-rs/Relm4, is the right default
  for a "native on Ubuntu (GNOME + Yaru)" mail client.** The stack is
  first-class on GNOME, fully usable from Rust, and the list widgets are built
  for exactly the 100k+ row problem a mail client has. It will *not* feel
  native on KDE Plasma — that is the accepted trade-off (or a future second
  front end via Qt/cxx-qt).
- The Rust binding ecosystem is healthy and current: gtk4 crate **0.11.5**
  (2026-09-20), libadwaita crate **0.9.2**, Relm4 **0.11.0** (2026-04-08),
  WebKitGTK bindings (webkit6 crate) **0.6.1** tracking WebKitGTK 2.52+
  features. GTK itself is at **4.22.5** stable; GNOME **51** is the current
  release.
- Qt 6 (now **6.12 LTS**, released 2026-09-30) via **cxx-qt 0.10.0** (KDAB,
  2026-08-24) is the credible alternative and the only way to be
  KDE-native, but it puts a C++/QML layer between the Rust core and the UI.
- Pure-Rust toolkits are not yet in the same class: iced **0.14.0** still
  describes itself as "experimental software", and COSMIC's iced-based
  desktop only shipped 1.0 in Pop!_OS 24.04 LTS on 2025-12-11. Slint 1.18 is
  polished but is its own visual world, not GNOME/KDE-native.
- HTML mail rendering should use **WebKitGTK 2.54** (stable 2.54.1,
  2026-10-02) with `enable-javascript` and `enable-javascript-markup` off and
  `auto-load-images` off — first-class settings that exist precisely for this.
- The graveyard of Linux mail clients teaches one lesson above all: **the
  sync engine is the product.** Thunderbird's data-loss ESRs (102, 128),
  Mailspring's 1.26.0 "major sync reliability release", and Geary's death by
  under-resourcing all point at where Sweetbriar's Rust core must be
  unbreakable.
- Distribution: **Flatpak on Flathub first, plus a .deb/PPA**; the XDG portal
  system (Email, Secret, Background, Notification portals) is the sanctioned
  way for a sandboxed mail client to integrate; Thunderbird today ships as a
  Canonical-verified snap at 157.0.1-1 (2026-10-06).
- Share with macOS: **view models in the Rust core**, HTML sanitization with
  **ammonia** (4.2.x, releases as of 2026-10-11), and localization strings in
  **Fluent** (fluent-rs). Keep all UI chrome, layout, and platform
  integration per-platform.
- The core API must be designed *now* as an async command/event bus with
  stable IDs and snapshots (the "mailsync" pattern), because that is what
  makes a Linux front end cheap to add after macOS ships.

---

## 2. Findings

### 2.1 Toolkit options and their maturity in 2026 (Q1)

**GTK 4 / libadwaita.** GTK's current stable release is **4.22.5**, with
4.23.4 unstable and 3.24.52 still maintained for the GTK 3 world
([gtk.org](https://www.gtk.org/), checked 2026-10-11). GNOME's current
release is **GNOME 51**, with 50 and 49 the previous two six-monthly
releases ([release.gnome.org](https://release.gnome.org/)). libadwaita is
GNOME's application library and the design language every core GNOME app
ships with.

The Rust story is the strongest of any non-Rust toolkit. The `gtk4` crate is
at **0.11.5** (published 2026-09-20; the 0.11 line began 2026-02-20), the
`libadwaita` crate at **0.9.2** (2026-07-07), and both are maintained under
the gtk-rs / GNOME umbrella with the same feature-flag-per-C-API-version
scheme as the rest of GNOME's Rust bindings
([crates.io/crates/gtk4](https://crates.io/crates/gtk4),
[crates.io/crates/libadwaita](https://crates.io/crates/libadwaita)). On top
of that sits **Relm4 0.11.0** (2026-04-08), an Elm-architecture framework
that markets itself as "build truly native applications" on gtk-rs, with a
book, async support, and reusable components
([relm4.org](https://relm4.org/),
[crates.io/crates/relm4](https://crates.io/crates/relm4)). Since Sweetbriar's
core is Rust, a GTK front end in Rust means **one language, zero FFI
boundary** for everything except GObject refcounts the bindings already
handle.

Native feel: a libadwaita app *is* the definition of native on Ubuntu's
default GNOME session (Yaru theming, GNOME HIG, CSD headers, dark style
preference). On KDE Plasma the same app will look and behave like a GNOME
transplant — libadwaita deliberately does not follow foreign desktop themes
*(qualitative assessment; widely discussed but not re-verified against a
primary source for this report)*.

**Qt 6 / QML via cxx-qt.** Qt's stable release is **6.12 (LTS), released
30 September 2026**, supported to September 2031, with 6.8 LTS also still in
support (to October 2029) and 6.11 from March 2026
([Wikipedia: Qt (software)](https://en.wikipedia.org/wiki/Qt_(software)),
release table, checked 2026-10-11). Qt is GPL/LGPL/commercial dual-licensed
(same source). KDE Plasma is built on Qt/QML, so a Qt 6 front end is the only
way to be truly native on KDE, and Qt's Widget/QML maturity for complex
desktop apps is unmatched.

The Rust bridge is **cxx-qt 0.10.0** (published 2026-08-24) from KDAB —
"Safe interop between Rust and Qt" — with 0.9.x/0.8.x releases through
2026 showing an active, professionally backed project
([crates.io/crates/cxx-qt](https://crates.io/crates/cxx-qt), docs:
[kdab.github.io/cxx-qt/book](https://kdab.github.io/cxx-qt/book/)). The model
is QML for the view, Rust QObject-backed types for logic/state bridged
through cxx. It works, but you now maintain QML, CMake, a C++ toolchain, and
an FFI layer — a real tax for a team whose core is Rust.

**COSMIC / iced.** Upstream iced is at **0.14.0** (released 2025-12-07) and
its README still says "**Iced is currently experimental software**"
([crates.io/crates/iced](https://crates.io/crates/iced),
[github.com/iced-rs/iced](https://github.com/iced-rs/iced) — 31.7k stars,
checked 2026-10-11). Meanwhile System76's fork powers a real desktop: Pop!_OS
**24.04 LTS with the COSMIC Desktop "Epoch 1" shipped 2025-12-11**, written
in Rust, with COSMIC 1.0.8 out 2026-02-24 and "Epoch 2 and 3" on the roadmap
([System76 blog](https://blog.system76.com/post/pop-os-letter-from-our-founder),
[COSMIC 1.0.8](https://blog.system76.com/post/cosmic-1-0-8-released),
[Epoch 2/3 roadmap](https://blog.system76.com/post/cosmic-epoch-2-and-3-roadmap)).
That proves the iced family can ship a desktop, but an email client needs
text editing, IME, accessibility and drag-and-drop depth that COSMIC's own
apps are still building out; and COSMIC is one desktop, not Ubuntu's default.
*(Opinion)* Not the horse for Sweetbriar's Linux default in 2026, but a
watch-list item: if iced stabilizes, a Rust-native front end becomes
attractive again.

**Slint.** Very actively developed — crate **1.18.1** (2026-09-21), with
1.15→1.18 all landing between February and September 2026
([crates.io/crates/slint](https://crates.io/crates/slint),
[github.com/slint-ui/slint](https://github.com/slint-ui/slint), 24.1k stars).
Slint is declarative (.slint markup compiled to Rust), renderer-agnostic
(femtovg/Skia/software), and even gains a *Qt style* when Qt is installed.
Licensing is a triple choice: **Royalty-free License for proprietary
desktop/mobile/web apps, GPLv3 for open source, or a paid commercial license
for embedded** (README, checked 2026-10-11). The catch for Sweetbriar: Slint
apps look like Slint. It targets "native" in the machine-code sense, not in
the follows-GNOME-HIG / Yaru / KDE-Breeze sense. *(Opinion)* Excellent for
embedded, wrong for "must feel native on Ubuntu and later KDE."

**Others (brief).** Tauri/wry, Electron, Flutter were not researched in depth
here; Electron is the incumbent answer (Mailspring uses it — see 2.4), and
Sweetbriar's brief explicitly wants native. Xilem/Makepad/Egui are either
pre-1.0 or clearly non-native in style *(Opinion)*.

### 2.2 Large lists, rich text, and HTML mail (Q1, cont.)

**Huge lists.** GTK 4's list stack was redesigned exactly for "large dynamic
lists": `GtkListView` "presents a large dynamic list of items", using a
factory that "generate[s] one row widget for each **visible** item" — i.e.
view-port virtualization driven by `GListModel`, with selection models,
rubberband selection, and `scroll_to()`; `GtkColumnView` adds "multiple
columns with headers" and is the documented tool for table-like views
([docs.gtk.org: GtkListView](https://docs.gtk.org/gtk4/class.ListView.html)).
This is the architecture GNOME Files, Software and others use for large
models; 100k+ rows are the design target rather than a fight. In gtk-rs you
drive it from Rust with custom `ListModel`/`ListItemFactory` implementations,
and the model can be a thin window over the Rust core's snapshot store.
*(Opinion)* For a mailbox list this beats the old GtkTreeView and keeps
jank out of the message list; Qt Quick's model/view ListView is the
equivalent on the Qt side (not re-verified against Qt docs for this report).

**Rich text editing.** For composition, the pragmatic pattern used by
existing clients is an embedded editor widget: GTK's `GtkTextView` +
Pango markup for plain-ish rich text, or the WebKitGTK-based composer
(Geary's composer supported "rich, styled text with images, links, and
lists" — [GNOME wiki archive, Geary](https://wiki.gnome.org/Apps/Geary)).
HTML composition inside a locked-down WebKitGTK editor (JS off for untrusted
content, on only for the compose surface) is the proven route; Evolution
added Markdown formatting to its composer
([Wikipedia: GNOME Evolution](https://en.wikipedia.org/wiki/GNOME_Evolution)).

**HTML mail rendering with JavaScript disabled.** WebKitGTK is the web engine
of the GNOME platform with **multi-process isolation** ("web pages cannot
crash the main application") and current stable **2.54.1 (2026-10-02)** /
2.54.0 (2026-09-16) ([webkitgtk.org](https://webkitgtk.org/)). The Rust
bindings are the `webkit6` crate at **0.6.1** (2026-03-11), whose feature
flags track WebKitGTK API 2.42 → 2.52
([crates.io/crates/webkit6](https://crates.io/crates/webkit6)).

Crucially, WebKitGTK's `Settings` object exposes exactly the switches a mail
client needs, and the Rust docs even show the canonical example "// Disable
JavaScript":

- `enable-javascript` — "whether or not JavaScript executes within a page";
- `enable-javascript-markup` — "all JavaScript-related elements and
  attributes are removed from the document **during parsing**" (stronger
  than disabling execution);
- `auto-load-images` — turn off to block remote-image tracking beacons;
- plus `allow-*-from-file-urls`, `disable-web-security`, media and storage
  toggles.

Source: [webkit6-rs `Settings`
docs](https://world.pages.gitlab.gnome.org/Rust/webkit6-rs/stable/latest/docs/webkit6/struct.Settings.html).
The Qt alternative, Qt WebEngine, is Chromium/Blink-based
([Wikipedia: Chromium, "Use in app
frameworks"](https://en.wikipedia.org/wiki/Chromium_\(web_browser\)#Use_in_app_frameworks))
and carries Chromium's weight and sandboxing complexity; with WebKitGTK,
GNOME maintains the security patch stream for you (see WebKitGTK's 2026
security advisory cadence on its home page). *(Opinion)* WebKitGTK is the
default choice for a GTK front end; a Qt front end would use Qt WebEngine
with equivalent `JavascriptEnabled`-style settings.

### 2.3 Native feel: Ubuntu (GNOME + Yaru) vs KDE (Q1, cont.)

- On **Ubuntu GNOME**, native = GTK4 + libadwaita + GNOME HIG + portals.
  Ubuntu ships GNOME as default and has defaulted to Thunderbird (not
  Evolution) as its mail client since 11.10
  ([Wikipedia: GNOME Evolution, Distribution](https://en.wikipedia.org/wiki/GNOME_Evolution)).
- On **KDE**, native = Qt/QML (Kirigami-style apps). A GTK/libadwaita app on
  KDE is the mirror image of a Qt app on GNOME: functional, theme-adjacent,
  but visibly foreign. There is no credible single-toolkit answer to "native
  on both"; even Slint's Qt style only dresses widgets like Qt. *(Opinion,
  consistent with the design positions of both projects.)*
- Wayland/X11: GTK 4 and Qt 6 are both Wayland-first with X11 fallback;
  nothing here forces a choice. (General platform status; not separately
  re-verified.)

### 2.4 Lessons from Linux mail clients (Q2)

**Geary (GTK/Vala) — design loved, project starved.** Geary was a
conversation-view, WebKitGTK-based GNOME client written in Vala with an
SQLite local store, GNOME Online Accounts integration and GNOME Keyring for
passwords; its keyword search (`from:john`, `is:unread`) was a signature
feature ([GNOME wiki archive](https://wiki.gnome.org/Apps/Geary),
[Wikipedia: Geary](https://en.wikipedia.org/wiki/Geary_\(e-mail_client\))).
Its history is the canonical sustainability warning: Yorba's 2013 crowdfunding
drive raised only US$50,860 of a $100,000 goal and folded; the project was
adopted by GNOME, rebooted by Michael Gratton in 2016, and the **last stable
release remains 46.0 on 20 May 2024** — over two years before this report
(Wikipedia, citing the GitLab tag). Lesson: a beautiful GTK client with one
maintainer is one burnout away from Geary's fate.

**Evolution (GTK/C) — the enterprise survivor.** Still shipping: **3.62.0 on
2026-09-11**, part of GNOME with Red Hat-backed development since 2013,
Microsoft Exchange support via the `evolution-ews` (Exchange Web Services)
package, plus S/MIME, GPG, search folders, and the Evolution Data Server
(EDS) that other GNOME apps (Calendar, Contacts) build on
([Wikipedia: GNOME Evolution](https://en.wikipedia.org/wiki/GNOME_Evolution)).
Lesson: EDS shows both the value and the cost of a shared PIM data layer —
powerful integration, heavyweight plumbing.

**KMail / Akonadi (Qt/C++) — architecture as a double-edged sword.** Akonadi
(v25.12.0, 2025-12-11) is KDE's "storage service for personal information
management," an extensible, concurrent-access server that applications talk
to through models, deliberately introduced to end per-app storage
duplication in KDE 4
([Wikipedia: Akonadi](https://en.wikipedia.org/wiki/Akonadi)). Recurring
user complaints about Akonadi's complexity, its database maintenance, and
failure modes are common in the KDE community **(unverified — not backed by a
primary source in this report)**. Lesson: Sweetbriar's core must not require
a user-visible daemon/database triad; keep the sync engine private to the
app, with the file format and CLI as the public surface.

**Thunderbird — scale and regressions.** Current stable is **157.0.1** (with
115.18.0 kept for Windows 7/8.1) and the ESR line is 153
([thunderbird.net](https://www.thunderbird.net/en-US/),
[Betterbird](https://betterbird.eu/)). The Betterbird project (a
fine-tuned Thunderbird; current **153.4.0esr-bb10**, 2026-09-29) documents
the quality history bluntly: Thunderbird 102 "shipped with a broken backend
that caused **data loss**"; 128 brought "permanent data loss for 32-bit
users and ongoing IMAP folder corruption"; their tracked early-release
regression counts per ESR run 43 → 184 → 270 → 395 (v102) → 248 (v128) →
56 (v140) → 158 (v153, as of 2026-08-31); Thunderbird only hired QA staff
and a release manager in mid-2024
([betterbird.eu](https://betterbird.eu/)). Betterbird itself demonstrates
the demand for old-school features Thunderbird left behind: multi-line
message list view (requested since 2003), regex search terms (requested
since 1999), correct behavior with >500 folders, Linux system-tray support
(requested since 1999) — all in their feature table. Lesson: (1) mail
*data loss* is a reputational kill shot; (2) there is a real audience for
exactly the classic-client features (Eudora/Mailsmith-style power) that
Sweetbriar wants to revive.

**Mailspring — the Electron client that keeps fighting sync.** Mailspring is
GPL-3.0: an Electron/TypeScript/React UI over a **C++ sync engine**
("mailsync", based on Mailcore2) that runs locally
([github.com/Foundry376/Mailspring](https://github.com/Foundry376/Mailspring),
17.9k stars). Version **1.26.0 (2026-10-02)** is billed as "a major sync
reliability release": the engine was rewritten to "track every folder a
message lives in, rather than forcing each message into a single folder,"
permanently fixing mail "flickering" between folders for iCloud,
self-addressed mail, Exchange duplicate Sent copies and **ProtonMail
labels** — and it shipped with a new automated test suite running against
real Dovecot, Cyrus and "fake problematic IMAP servers" in Docker
([1.26.0 release
notes](https://github.com/Foundry376/Mailspring/releases/tag/1.26.0)).
Security hardening in the same release (links open in the browser, remote
image blocking in previews) mirrors the WebKitGTK settings discussion above.
Lesson: the "one message, many folders" data model and server-farm-in-CI
testing are the two ideas worth stealing wholesale; also note an
Electron client *with a native core* still fights UI-level native feel
(the release fixes "two title bars appearing on KDE and GNOME").

**Betterbird as distribution case.** Betterbird ships Flatpak (Flathub),
AppImage, AUR and distro packages from a small team
([betterbird.eu](https://betterbird.eu/)) — evidence that Flathub is the
default reach move for a new Linux mail client.

**Claws Mail — the durability baseline.** Still releasing (4.4.0 on
2026-03-09), GTK-based, storing mail in the plain **MH format** "which
features fast access and data security," import/export from almost anything
([claws-mail.org](https://www.claws-mail.org/)). Lesson: a boring, open,
per-message on-disk format is a 25-year survival asset — align with the
project's "never lose mail" goal at the storage layer.

### 2.5 Platform integration (Q3)

- **GNOME Online Accounts (OAuth for Google and Microsoft).** GOA was
  substantially rewritten in GNOME 46 (2024-03-20): Microsoft 365 accounts
  with **OneDrive** support, authentication through the **default web
  browser** (enabling hardware keys and other methods), a new **WebDAV**
  provider, and a redesigned settings UI; the old IMAP/SMTP-style account
  providers did not survive the rewrite **(unverified for the removal
  wording; the additions are sourced)**
  ([GNOME 46 release notes](https://release.gnome.org/46/)). Geary's
  documentation confirms apps can simply "pick up your existing GNOME Online
  Accounts" ([wiki](https://wiki.gnome.org/Apps/Geary)). Practical read for
  Sweetbriar: GOA is a *convenience import* for Google/Microsoft OAuth
  tokens, not a complete account backend; the core must own OAuth flows
  itself (Thunderbird-style device/browser flows) and merely *accept* GOA
  tokens when present. *(Opinion.)*
- **Secret Service / libsecret.** The `libsecret` Rust bindings are at
  **0.9.0** (2026-03-17) with feature flags tracking libsecret 0.18 through
  **0.21.2** — actively maintained under the GNOME Rust umbrella
  ([crates.io/crates/libsecret](https://crates.io/crates/libsecret)). Store
  account credentials and OAuth refresh tokens in the Secret Service
  (GNOME Keyring/KWallet backends), never in plain files.
- **XDG portals.** The portal framework exposes well-defined D-Bus
  interfaces for sandboxed apps, with **Email** (compose with
  attachments/addresses), **Secret**, **Background** (autostart/background
  requests), **Notification**, Clipboard and more, each documented, with
  backends for GNOME, KDE, COSMIC, wlroots, elementary and others
  ([xdg-desktop-portal docs](https://flatpak.github.io/xdg-desktop-portal/),
  [GitHub](https://github.com/flatpak/xdg-desktop-portal)). The Rust client
  crate is **ashpd 0.13.13** (2026-07-17)
  ([crates.io/crates/ashpd](https://crates.io/crates/ashpd)). There is also
  an `org.freedesktop.background.Monitor` interface for exposing background
  apps — relevant to how GNOME presents a resident sync process.
- **LocalSearch (Tracker) search integration.** GNOME's filesystem indexer
  is now **LocalSearch**, "designed to have a low memory footprint during
  indexing, and to power fast queries across file metadata," with documented
  D-Bus query APIs
  ([localsearch docs](https://gnome.pages.gitlab.gnome.org/localsearch/)).
  For Sweetbriar, system-wide mail search should stay *inside* Sweetbriar's
  index (permissions and UX), with optional localsearch registration as a
  future enhancement. *(Opinion; LocalSearch indexes files, not app-private
  databases.)*
- **mailto: handler, D-Bus, systemd user services.** Registering as a
  `mailto:` handler is done via the desktop entry's
  `x-scheme-handler/mailto` MIME type (standard XDG mechanism; spec not
  re-verified here). Notifications should go through the Notification
  portal or libnotify. For background sync, the modern pattern is a
  **systemd user service** (or D-Bus-activated helper) that the GUI talks
  to — the same shape as Akonadi but private and optional; the portal
  Background interface is the sanctioned way to request autostart inside a
  sandbox (see portal docs above).

### 2.6 Distribution and sandboxing (Q4)

- **Ubuntu base.** Current LTS is **26.04 (released April 2026**, standard
  security maintenance to May 2031; dev codename "Resolute Raccoon"), with
  24.04 LTS (Apr 2024) still everywhere
  ([ubuntu.com/about/release-cycle](https://ubuntu.com/about/release-cycle)).
  Target 24.04/26.04 for CI.
- **Snap.** Thunderbird is published as a Canonical-verified snap at
  **157.0.1-1** (latest/stable updated 2026-10-06); Mozilla's snap channels
  changed in 2026 with user action required by 2026-07-21 to stay on ESR
  ([snapcraft.io/thunderbird](https://snapcraft.io/thunderbird)). A snap
  gets you into Ubuntu's store by default, but as a third-party publisher
  you'd rely on the same confinement stack as Flatpak with less ecosystem
  momentum outside Ubuntu. *(Opinion.)*
- **Flatpak/Flathub.** The portal system (above) exists precisely so a
  sandboxed Flatpak can do mail-client things: open URIs, send
  notifications, request background running, use the Secret portal.
  Betterbird's Flathub presence from a tiny team shows the burden is low
  ([betterbird.eu downloads](https://betterbird.eu/),
  [Flathub: eu.betterbird.Betterbird](https://flathub.org/apps/details/eu.betterbird.Betterbird)).
  Sandbox implications to plan for: mail store inside the sandbox's
  XDG_DATA_HOME (or a portal-granted location), credentials via
  Secret portal/libsecret, autostart via Background portal, and
  `--filesystem=xdg-run/gvfs` style escapes are *not* needed if you commit
  to portals. *(Opinion/plan, grounded in the portal docs.)*
- **.deb / PPA and AppImage.** Keep a .deb (or Debian-hosted package) for
  server-style installs and for users who resent sandboxes; Claws Mail's
  longevity in distro repos is the model. AppImage is a nice-to-have
  (Betterbird ships one) but solves nothing a Flatpak doesn't.

### 2.7 Sharing UI code with macOS without losing native feel (Q5)

What travels well between a Swift/AppKit-SwiftUI front end and a Rust/GTK
front end is everything that is *not* UI:

- **View models in the Rust core.** Mailspring's architecture (Electron UI +
  native mailsync engine with a message/event protocol) is the proof that
  the split works; their 1.26.0 sync rewrite was done entirely in the
  engine and benefited every UI
  ([release notes](https://github.com/Foundry376/Mailspring/releases/tag/1.26.0)).
  Sweetbriar should go further: mailbox/thread/message view models,
  search state machines, drafts & outbox state, and account configuration
  validation all live in Rust, exposed to Swift and to the Linux GUI as
  data + events. The UI layers render and interpret; they never own state.
  *(Opinion/Recommendation.)*
- **Shared localization.** Project Fluent is a localization system built
  for "natural-sounding translations" — asymmetric messages, plural rules
  per CLDR, terms for brand consistency — with a Rust implementation
  (fluent-rs, Apache-2.0) and Firefox as the flagship user
  ([projectfluent.org](https://projectfluent.org/)). Use `.ftl` files as
  the single source of strings; Swift side there are Fluent-compatible
  bindings **(unverified — not checked for this report)**; worst case,
  render strings to `.strings` at build time from `.ftl`.
- **Shared HTML sanitizer.** **ammonia** is the standard Rust
  allowlist-based HTML sanitizer (keywords: html/sanitization/security/xss;
  18.2M downloads; releases as fresh as **2026-10-11**, current line
  4.2.x) ([crates.io/crates/ammonia](https://crates.io/crates/ammonia)).
  Sanitize-and-rewrite (cid: → cache paths, remote-image placeholder) once
  in the core; both platforms then feed identical, safe HTML to WebKit
  (WebKitGTK) / WKWebView (macOS) with JavaScript disabled. This is the
  single highest-leverage piece of shared "UI-adjacent" code.
- **What must stay per-platform:** all chrome (windows, toolbars, menus,
  shortcuts), the message-list widgets (GtkColumnView vs NSTableView),
  composers, settings UI, notifications, drag-and-drop, theme/dark-mode
  plumbing, mailto handling, and the web-view embedding itself. The
  recommendation here is explicitly *not* "share the UI toolkit" — sharing
  SwiftUI or GTK across OSes is exactly the native-feel killer Sweetbriar
  wants to avoid. *(Opinion.)*

---

## 3. Options compared

### 3.1 Toolkits

| Toolkit (binding) | Current, verified 2026-10-11 | Native on Ubuntu/GNOME | Native on KDE | From Rust | Large lists | HTML mail web view | Maturity risk |
|---|---|---|---|---|---|---|---|
| **GTK4 + libadwaita (gtk-rs / Relm4)** | GTK 4.22.5; gtk4 0.11.5; libadwaita 0.9.2; Relm4 0.11.0 | **Yes (definitionally)** | No (GNOME-styled guest) | **First-class, zero FFI** | GtkListView/GtkColumnView built for large dynamic lists | WebKitGTK 2.54 via webkit6 0.6.1, JS-off settings built in | Low; bindings track GNOME releases |
| **Qt 6/QML (cxx-qt)** | Qt 6.12 LTS (2026-09-30); cxx-qt 0.10.0 (2026-08-24) | Guest (Qt styling) | **Yes (definitionally)** | Good but C++/QML/FFI layer | Qt Quick model/view (mature) | Qt WebEngine (Chromium-based) | Low for Qt; medium for the bridge |
| **COSMIC / iced** | iced 0.14.0 (2025-12-07), "experimental"; COSMIC 1.0 in Pop!_OS 24.04 LTS (2025-12-11) | Own look (COSMIC), guest on GNOME/KDE | Guest | **Native** | Custom (widget-level virtualization) | None standard (embed yourself) | **High** for a productivity app |
| **Slint** | 1.18.1 (2026-09-21); royalty-free desktop license | Slint look (Qt style only if Qt present) | Same | **Native Rust** | Built-in ListView/TableView (its own) | None standard | Low-mid; "native" ≠ desktop-native |
| **Electron + Rust core (Mailspring pattern)** | Electron 44.x-era; proven by Mailspring 1.26.0 | Least native | Least native | Core in Rust, UI in TS | DOM/Virtualized lists | Chromium built in | Proven but off-brief (not native) |

### 3.2 Distribution

| Channel | Evidence (2026-10-11) | Pros for a mail client | Cons |
|---|---|---|---|
| Flatpak/Flathub | Portal system exists for it; Betterbird ships there | One build for all distros; portals for Secret/Notification/Background/Email | Sandbox data lives in `~/.var/app/...`; must commit to portals |
| Snap | Thunderbird snap 157.0.1-1 (Canonical-verified, 2026-10-06) | Default store presence on Ubuntu | Ubuntu-centric; same confinement questions |
| .deb (PPA / Debian) | Claws/Evolution model; Ubuntu 26.04 LTS base | No sandbox; sysadmin-friendly | Old GTK/WebKit versions on LTS unless you bundle |
| AppImage | Betterbird ships one | Zero-install tryout | No portal integration story; updates manual |

---

## 4. Recommendations for Sweetbriar

*The following is the author's recommendation (opinion), with the strongest
counter-argument at the end.*

1. **Build the Linux front end in GTK 4 + libadwaita, in Rust, with Relm4
   (or vanilla gtk-rs) as the architecture layer.** Rationale: it is the
   only option that is simultaneously (a) definitionally native on the
   Ubuntu/GNOME target named in the brief, (b) written in the same language
   as the core, (c) equipped with purpose-built large-list widgets and a
   maintained WebKit binding with JavaScript-disable settings, and (d)
   portable enough to run unchanged inside a Flatpak. Qt/cxx-qt is the
   documented fallback if KDE-native feel becomes a hard requirement.
2. **Render HTML mail in WebKitGTK with `enable-javascript` and
   `enable-javascript-markup` false, `auto-load-images` false; sanitize and
   rewrite HTML in the core with ammonia first.** Compose in a WebKitGTK
   editor instance where JS is enabled but content is trusted/own-generated.
3. **Design the core API now as a UI-agnostic, async command/event bus** —
   the lesson of Mailspring's mailsync: typed commands in, ordered events
   out (mailbox deltas, thread rebuilds, sync state), plus snapshot queries
   with stable IDs and a "message can live in many folders" model from day
   one. Concretely: expose the core over an in-process Rust API
   (`sweetbriar-core`), with a thin serialization layer (JSON-RPC-style or
   protobuf over stdio/socket) used both for tests and for any future
   split-process/systemd-user-service mode. Keep all identifiers
   opaque strings/hashes; never expose IMAP UIDs or SQLite rowids upward.
4. **Keep secrets in the Secret Service via libsecret (crate 0.9.0), use
   portals (ashpd) for notifications/background/email-compose in the
   Flatpak, and register `mailto:` via the desktop entry.** Accept GOA
   tokens for Google/Microsoft when present; run OAuth2 flows in the core
   otherwise.
5. **Distribute Flatpak-first on Flathub, with a .deb built from the same
   CI, and keep the mail store in an open, plain format** (per-message
   files à la MH or Maildir under the hood, or at minimum a documented,
   exportable SQLite) — Claws Mail's 4.x longevity and "data security" claim
   is the model for the never-lose-mail guarantee.
6. **Share strings via Fluent (.ftl) and the sanitizer via ammonia; share
   nothing that draws pixels.** macOS (Swift) and Linux (Rust/GTK) front
   ends should be able to be written by people who never read each other's
   code, against the same view-model layer.

**Strongest counter-argument (against the GTK recommendation):** by choosing
GTK/libadwaita you are deciding that KDE users — a large, vocal fraction of
the Linux desktop, and disproportionately the kind of power users who mourn
Eudora and Mailsmith — get a guest-grade app: GNOME styling, GNOME dialogs,
no Kirigami/plasma integration. Qt 6.12 via cxx-qt would make Sweetbriar
feel at home on Plasma *and* ships a mature Chromium-based web view, at the
cost of a QML/C++ layer on top of the Rust core and a "Qt app" feel on
GNOME — i.e., you trade the named Ubuntu target for KDE. The honest answer
is that no single toolkit wins both desktops in 2026, and if Sweetbriar
later finds its audience skewing KDE, the core-bus architecture in
recommendation 3 is what makes a cxx-qt second front end affordable rather
than a rewrite.

---

## 5. Risks and open questions

- **gtk-rs API churn:** the gtk4 crate follows GTK's 6-month cadence
  (0.11.0 in Feb 2026 → 0.11.5 in Sept 2026); pin versions per release and
  expect minor breakage on majors.
- **libadwaita on non-GNOME desktops** will not follow system themes
  *(qualitative; verify against current libadwaita policy before shipping a
  KDE-styled variant)*.
- **Flatpak mail-store location** (`~/.var/app/…`) surprises users migrating
  from Claws/Evolution; decide early whether the .deb and Flatpak share a
  store path, and document backup/restore.
- **GOA token scope:** whether current GOA (post-GNOME-46 rewrite) exposes
  usable IMAP/SMTP-capable OAuth tokens to third-party apps for Google and
  Microsoft 365 was **not verified**; pilot this before promising
  "add account from GNOME Settings."
- **Background sync UX:** GNOME's background-apps monitor and the Background
  portal cover autostart, but battery/network-aware scheduling policies
  (GNOME's `power-profiles`, NetworkManager) need design work; Akonadi's
  reputation shows how badly a heavy resident daemon reads. 
- **Thunderbird as incumbent:** at 157/ESR-153 with snap+flatpak+deb reach,
  Thunderbird is the default on Ubuntu; Sweetbriar's wedge is the classic
  feature set (Betterbird's table is effectively the market survey) and
  Rust-core reliability, not parity-at-all-costs.
- **Open questions:** Does WebKitGTK 2.54's print/printing API cover
  "print a message" acceptably? Do GNOME's Yaru accents override
  libadwaita styling in 26.04's GNOME version (unverified)? Is there a
  maintained Fluent implementation for Swift (unverified)?

---

## 6. Sources (consulted 2026-10-11)

- GTK: https://www.gtk.org/ (stable 4.22.5 / unstable 4.23.4 / 3.24.52)
- GtkListView docs: https://docs.gtk.org/gtk4/class.ListView.html
- GNOME release notes index (GNOME 51 latest): https://release.gnome.org/
- GNOME 46 release notes (Online Accounts rewrite): https://release.gnome.org/46/
- gtk4 crate: https://crates.io/crates/gtk4 (0.11.5, 2026-09-20; via crates.io API)
- libadwaita crate: https://crates.io/crates/libadwaita (0.9.2, 2026-07-07)
- Relm4: https://relm4.org/ and https://crates.io/crates/relm4 (0.11.0, 2026-04-08)
- iced: https://crates.io/crates/iced (0.14.0, 2025-12-07) and https://github.com/iced-rs/iced
- Slint: https://crates.io/crates/slint (1.18.1, 2026-09-21) and https://github.com/slint-ui/slint
- cxx-qt: https://crates.io/crates/cxx-qt (0.10.0, 2026-08-24); book: https://kdab.github.io/cxx-qt/book/
- Qt: https://en.wikipedia.org/wiki/Qt_(software) (6.12 LTS, 2026-09-30; release table)
- WebKitGTK: https://webkitgtk.org/ (2.54.1, 2026-10-02; 2.54.0, 2026-09-16)
- webkit6 crate: https://crates.io/crates/webkit6 (0.6.1) and Settings docs:
  https://world.pages.gitlab.gnome.org/Rust/webkit6-rs/stable/latest/docs/webkit6/struct.Settings.html
- Qt WebEngine is Chromium-based: https://en.wikipedia.org/wiki/Chromium_(web_browser) (Use in app frameworks)
- System76 / COSMIC: https://blog.system76.com/post/pop-os-letter-from-our-founder (Pop!_OS 24.04 LTS + COSMIC, 2025-12-11); https://blog.system76.com/post/cosmic-1-0-8-released; https://blog.system76.com/post/cosmic-epoch-2-and-3-roadmap
- Geary: https://en.wikipedia.org/wiki/Geary_(e-mail_client) (46.0, 2024-05-20; history) and https://wiki.gnome.org/Apps/Geary (archived wiki)
- Evolution: https://en.wikipedia.org/wiki/GNOME_Evolution (3.62.0, 2026-09-11; EWS; Ubuntu default since 11.10)
- Akonadi: https://en.wikipedia.org/wiki/Akonadi (v25.12.0, 2025-12-11; architecture)
- Thunderbird: https://www.thunderbird.net/en-US/ (157.0.1 current; ESR 153)
- Betterbird: https://betterbird.eu/ (153.4.0esr-bb10, 2026-09-29; TB data-loss history, regression counts, feature table, packaging)
- Claws Mail: https://www.claws-mail.org/ (4.4.0, 2026-03-09; MH format)
- Mailspring: https://github.com/Foundry376/Mailspring (README; GPL-3.0; Electron + C++ mailsync) and https://github.com/Foundry376/Mailspring/releases/tag/1.26.0 (1.26.0, 2026-10-02, sync reliability rewrite)
- libsecret crate: https://crates.io/crates/libsecret (0.9.0, 2026-03-17; feature flags to libsecret 0.21.2)
- ashpd: https://crates.io/crates/ashpd (0.13.13, 2026-07-17)
- XDG Desktop Portal docs (portal list incl. Email, Secret, Background, Notification; backends): https://flatpak.github.io/xdg-desktop-portal/ and https://github.com/flatpak/xdg-desktop-portal
- LocalSearch: https://gnome.pages.gitlab.gnome.org/localsearch/
- Ubuntu release cycle: https://ubuntu.com/about/release-cycle (26.04 LTS, Apr 2026)
- Thunderbird snap: https://snapcraft.io/thunderbird (157.0.1-1, 2026-10-06; channel change notice)
- Betterbird on Flathub: https://flathub.org/apps/details/eu.betterbird.Betterbird
- Project Fluent: https://projectfluent.org/
- ammonia: https://crates.io/crates/ammonia (4.2.x; releases 2026-10-11)
