# 09 — The Linux front end

Synthesis of [raw/linux-native/glm.md](raw/linux-native/glm.md) and
[raw/linux-native/codex.md](raw/linux-native/codex.md). Both recommend the
same stack independently. Checked 2026-10-11.

## Summary

- **GTK 4 + libadwaita + gtk-rs** (Relm4 optional) for Ubuntu/GNOME. It
  is written in Rust like the core, so it calls the core directly with no
  FFI. The GTK main context stays the UI executor; the engine's Tokio
  runtime returns results over bounded channels.
- **Qt 6 + Kirigami via CXX-Qt is the strongest alternative.** It is the
  only truly KDE-native option, and Qt's `QTextEdit` document model could
  make the composer easier. It costs a C++/QML layer and CMake. Pick it
  if KDE becomes an equal launch priority, or if the composer prototype
  fails on GTK.
- **Not yet:** iced/COSMIC (experimental; accessibility labelled
  experimental), Slint (its own look; web view issue still open),
  Electron/Tauri (off-brief).
- **HTML:** WebKitGTK API 6.0 (engine 2.54.1, released 2026-10-02) through
  the `webkit6` crate. `enable-javascript` and `enable-javascript-markup`
  off (script stripped at parse time), `auto-load-images` off, plus the
  shared Rust sanitizer and resource policy.
- **Distribution:** Flatpak on Flathub first, plus a supported Ubuntu
  `.deb`. Snap after credential and background tests (snap user daemons
  are still experimental). AppImage optional.

## Toolkit facts (2026-10-11)

| Toolkit | Versions | Native on GNOME/Ubuntu | Native on KDE | From Rust | Lists | HTML | Risk |
|---|---|---|---|---|---|---|---|
| GTK 4.22 + libadwaita (gtk4 0.11, libadwaita 0.9, Relm4 0.11) | Active, tracks GNOME 51 | Yes | Guest (doesn't follow foreign themes) | Direct | `GtkListView`/`GtkColumnView` recycle rows from a `GListModel` | WebKitGTK 6.0 | Low |
| Qt 6.12 LTS + Kirigami (cxx-qt 0.10, KDAB) | Active | Guest | Yes | C++/QML bridge | QML ListView | Qt WebEngine (Chromium) | Medium (bridge) |
| iced 0.14 / COSMIC | "Experimental" | Own look | Guest | Direct | Custom | None standard | High |
| Slint 1.18 | Active | Own look | Own look | Direct | Built in | None | Medium |

## Lessons from Linux mail clients

- **Thunderbird:** data-loss regressions in 102 and 128 (per Betterbird's
  tracking); Gloda search unreliability; Supernova UI slowness. Its native
  Graph support shipped in 154.
- **Betterbird's feature table is a demand survey:** multi-line message
  list, regex search, more than 500 folders, tray icon.
- **Geary:** a beautiful design, starved of resources (last stable 46.0,
  May 2024). One maintainer is one burnout away.
- **Evolution:** survives with Red Hat backing; EDS shows the cost of a
  heavyweight shared PIM layer.
- **KMail/Akonadi:** a visible PIM daemon plus database. Akonadi's own
  documentation warns that unsynced changes can live in its database.
  Keep Sweetbriar's engine private, with export and a CLI as the public
  surface.
- **Mailspring:** an Electron UI over a C++ sync engine. The 1.26.0
  (2026-10-02) rewrite moved to a many-folders-per-message model and CI
  against real and "problematic" IMAP servers. It still fought title bar
  duplication on GNOME and KDE.
- **Claws Mail:** plain MH storage as a 25-year survival asset.

## Platform integration

- **GNOME Online Accounts:** GOA lists Microsoft 365 mail, but its token
  interface **can't request arbitrary scopes**. Treat GOA tokens as an
  optional convenience import; the core runs its own OAuth flows. Validate
  against real tenants.
- **Secrets:** the Secret Service via `libsecret` (crate 0.9) or the Secret
  portal (`ashpd`); never plain files. Recovery after keyring loss is a
  product decision.
- **Portals (ashpd 0.13):** Notification, Background, Email (compose
  hand-off), OpenURI, file chooser. **Background permission doesn't
  promise uninterrupted execution.** Windows closed, logged out, suspended
  and keyring locked are distinct states to test.
- **Background sync:** a systemd user service or a D-Bus-activated helper
  is the shape that matches a future macOS agent, with the same ownership
  lease. GNOME's background-apps monitor shows it to users.
- **Search:** keep Sweetbriar's own index authoritative; LocalSearch
  (ex-Tracker) export is optional.
- **`mailto:`** via `x-scheme-handler/mailto` in the desktop file; notification
  activation.
- **Fonts and appearance:** check Yaru accents and dark mode on a real
  Ubuntu installation and the Flatpak runtime.

## Distribution

| Channel | Pros | Cons |
|---|---|---|
| **Flatpak / Flathub** (first) | One build for every distro; portals for secrets, notifications, background, email | Store under `~/.var/app/...` (document backup and migration); must commit to portals |
| **.deb (Ubuntu 24.04 / 26.04 LTS)** | No sandbox; familiar to sysadmins | Old GTK/WebKit on LTS unless bundled |
| Snap (later) | Ubuntu store presence | User daemons experimental; password-manager interface needs testing |
| AppImage (optional) | Try without installing | No portals; manual updates |

WebKitGTK security updates must flow promptly for years, whatever the
package format. Disabling JavaScript doesn't remove the need for engine
patches.

## What to share with macOS, and what not to

- **Shared (Rust):** MIME, sanitizing and resource policy, search and
  query grammar, rules, account backends, durable drafts and outbox,
  commands, paged observable queries, view-state semantics, typed errors,
  localization strings. Fluent `.ftl` is a candidate; Swift Fluent support
  is unverified, so the fallback is generating `.strings`/`.xcstrings` at
  build time.
- **Per platform:** windows, menus, toolbars, list widgets, composer,
  settings, notifications, drag and drop, theme plumbing, web view
  embedding, `mailto:` handling.
- **The core must not assume any UI event loop or platform keychain.**
  Headless core tests run without either.

## Prototype gates before committing to GTK (codex)

1. **Lists and search:** 100k then 1M messages, concurrent arrivals and
   deletes, sorting, cancellation. Targets: cached first rows within
   100 ms, no sustained stalls beyond 16.7 ms frames.
2. **Composition:** formatting subset, nested quotes, pasted HTML, inline
   images, signatures, undo, spelling, RTL, CJK input methods, screen
   reader, two windows editing one draft, forced termination after
   acknowledged saves.
3. **HTML and privacy:** hostile corpus; no script, no unsolicited network
   or file access (including CSS and fonts); links, CID, printing, zoom,
   dark mode, accessibility.
4. **Desktop and distribution:** GNOME/Yaru and KDE on Wayland; Flatpak and
   .deb; locked or missing secrets; GOA vs standalone OAuth; denied
   background permission; suspend; `mailto:` and notification activation.

Failing a gate reopens that specific decision (most likely the composer)
without touching the shared core.

## Open questions

- **What does "native Linux" mean at launch:** GNOME first, or GNOME and KDE
  equally?
- **What composer fidelity is required?** A defined formatting subset is
  far cheaper than arbitrary HTML editing.
- **Do GOA grants work for Graph and Gmail scopes on real accounts?**

## Key sources

- [GTK scalable lists](https://blogs.gnome.org/gtk/2020/06/07/scalable-lists-in-gtk-4/), [gtk-rs main event loop](https://gtk-rs.org/gtk4-rs/stable/latest/book/main_event_loop.html), [libadwaita](https://world.pages.gitlab.gnome.org/Rust/libadwaita-rs/stable/latest/docs/libadwaita/)
- [WebKitGTK releases](https://webkitgtk.org/news.html), [webkit6 Settings](https://world.pages.gitlab.gnome.org/Rust/webkit6-rs/stable/latest/docs/webkit6/struct.Settings.html)
- [CXX-Qt](https://github.com/KDAB/cxx-qt), [KDE application design](https://develop.kde.org/hig/kde_app_design/), [QTextEdit](https://doc.qt.io/qt-6/qtextedit.html)
- [GOA services](https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/services.html), [GOA OAuth2 interface](https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/dbus-org.gnome.OnlineAccounts.OAuth2Based.html)
- [XDG portals](https://flatpak.github.io/xdg-desktop-portal/), [Background portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Background.html), [Flatpak permissions](https://docs.flatpak.org/en/latest/sandbox-permissions.html)
- [Betterbird](https://betterbird.eu/), [Mailspring 1.26.0](https://github.com/Foundry376/Mailspring/releases/tag/1.26.0), [Akonadi](https://userbase.kde.org/Akonadi), [Claws Mail](https://www.claws-mail.org/)
