# Topic: A native-feeling Linux/Ubuntu front end on a shared Rust core

Questions:
1. Toolkit options and their maturity in 2026: GTK4 + libadwaita via
   gtk-rs (and Relm4), Qt 6/QML via cxx-qt (KDE), COSMIC/iced, Slint,
   others. Which feels native on Ubuntu (GNOME + Yaru) and on KDE? Which
   handles large lists (GtkListView/GtkColumnView with models of 100k+
   items), rich text editing, and embedding a web view for HTML mail
   (WebKitGTK 6, Qt WebEngine) with JavaScript disabled?
2. Lessons from Linux mail clients: Geary (GTK, Vala), Evolution, KMail/
   Akonadi, Thunderbird, Mailspring, Claws Mail, Betterbird; what users
   praise and complain about; performance and data-safety problems
   (e.g. Akonadi).
3. Platform integration: GNOME Online Accounts (OAuth for Google and
   Microsoft), Secret Service / libsecret, desktop notifications, XDG
   portals, background apps, localsearch (Tracker) search integration,
   mailto: handler, D-Bus, systemd user services for background sync.
4. Distribution: Flatpak (Flathub), Snap (Ubuntu), .deb, AppImage;
   sandbox implications for a mail client.
5. How much UI code can be shared with macOS without losing native feel
   (view models in Rust? shared strings and localization? shared HTML
   sanitizer?) and what must be per-platform.
6. Recommend a toolkit and architecture for Sweetbriar on Linux, and
   what the Rust core API must look like now so that the Linux front end
   is cheap to add later.
