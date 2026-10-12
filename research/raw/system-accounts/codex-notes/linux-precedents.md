# Evidence memo: KDE/other-desktop account systems + mail-client precedents

Research date: 2026-10-12. Scope: task.md questions 3 (KDE stack, Cinnamon/XFCE/COSMIC)
and 4 (how named mail clients handle system accounts; whether any third-party Mac
client reads Internet Accounts). Tags: **[proven]** = stated in a consulted primary
source; **[inferred]** = my conclusion from those sources; **[unverified]** = not
confirmed. Premise check: macOS 27 "Golden Gate" is real — released 2026-09-14,
Apple silicon only (Intel dropped) [proven]. Sources at end.

## 1. KDE: KAccounts / Accounts-SSO / signond

**Architecture [proven].** KAccounts is a KDE Frameworks-style abstraction over two
upstream "Accounts-SSO" components (Nokia/Intel heritage): *libaccounts* (accounts DB;
GLib and Qt flavors; sqlite at `~/.config/libaccounts-glib/accounts.db`) and *signond*
(auth daemon + plugins, e.g. `signon-plugin-oauth2`), per the kaccounts-integration
README and Ubuntu Touch AppArmor policy files. Split responsibilities: libaccounts owns
account metadata/services; signond owns authentication sessions and credential
operations; `signon-ui` pops the browser window; `kaccounts-providers` ships
provider/service XML (providers = Google, Nextcloud, ownCloud only; a `disabled/` dir
holds retired Facebook/Identi.ca/Google service files).

**D-Bus surface [proven].** Session-bus names use the legacy
`com.google.code.AccountsSSO.*` namespace: `SingleSignOn` at
`/com/google/code/AccountsSSO/SingleSignOn` exposes `AuthService`, `AuthSession`,
`Identity` interfaces; accounts management is `com.google.code.AccountsSSO.Accounts.Manager`
at `/com/google/code/AccountsSSO/Accounts/Manager` (Ubuntu Touch AppArmor profiles and
libaccounts `Accounts-1.0` API constants confirm both names). Auth flow: a client asks
`AuthService.getAuthSessionObjectPath` for an `AuthSession`, which runs a SASL-style
plugin (oauth2 plugin handles Google); `signon-ui` is the D-Bus-activated helper that
opens the browser window. KOnlineAccounts' README notes the key security consequence:
apps "more or less directly read account data from a database in the home
directory" — read access to `accounts.db` exposes *all* accounts of all types, no
per-app scoping; Ubuntu Touch papers over this with AppArmor mediation, Plasma does not.

**Provider coverage [proven].** The shipped Google provider embeds one shared KDE OAuth
client ID *and client secret in plaintext* (google.provider.in), requesting
userinfo/calendar/tasks/contacts/YouTube-upload scopes — **no Gmail/IMAP scope, and no
mail service file exists anywhere in kaccounts-providers**. The NixOS signond-packaging
PR (2026) states the baked-in KDE client "is not approved by Google for Drive access"
and must be overridden at build time. So even in-scope services hit consent-screen
walls.

**KMail/Akonadi usage [proven].** KDE PIM does not get mail from KAccounts. Evidence:
(a) KOnlineAccounts README: "Not many applications actually use it. Notably absent is
KDE PIM." (b) KDE Discuss (KDE staff): KAccounts and Akonadi are "a completely
different system"; users grant Google permissions twice. (c) KMail's docs show its own
Account Wizard (name/email/password + server autodiscovery). There *is* a narrow
bridge: Phabricator R165 added optional KAccounts support to `akonadi_control` so a
new KAccounts Google account auto-creates Akonadi calendar/contacts resources via
`org.kde.Akonadi.Accounts` — contacts/calendar only, never mail [proven].

**Maintenance [proven].** signond is minimally alive: last master commits are Apr 2026
(hang fix) authored by a Jolla (Sailfish) dev, merged by Alberto Mardegan — i.e.,
upstream survives on Sailfish/UBports needs, not KDE. Nicolas Fella (KDE) writes
(Nov 2025 blog): the Accounts-SSO stack "receives very little development activity, to
the point where it was hard to get the required patches for Qt6 support in"; his signond
Qt6 MR (!36) dates to Jul 2022, and NixOS pins `signon-plugin-oauth2` to an unmerged Qt6
MR tip — corroboration that Qt6 support upstreams poorly. kaccounts-integration itself
still ships in KDE Gear (26.04.0, Apr 2026, but "no code change" releases).

**Replacement underway [proven].** KDE's `konlineaccounts` (invent.kde.org/system) is a
prototype daemon+DBus service "designed to replace the current KAccounts system." Per
Fella's post, `konlineaccountsd` exposes accounts over D-Bus with xdg-desktop-portal-inspired
per-app grants (apps see only accounts they were granted), and the daemon owns the OAuth
flow. Rationale listed in the README: tiny app adoption ("notably absent is KDE PIM"),
not sandbox-friendly, complexity spread over ~a dozen repos "outside the direct control
of KDE and mostly unmaintained." Prototype stage, not production (README, Oct 2026 checkout);
KDE Discuss thread shows interest but no committed consumers.

## 2. Other Linux desktops

- **GNOME baseline** (context for precedents): goa-daemon exposes
  `org.gnome.OnlineAccounts` on the session bus (cited in Flatpak `--talk-name` args in
  a GNOME Discourse thread) and is actively maintained — gnome-46/48-era NEWS shows an
  MS-365 Graph provider with credential-rotation fixes, the generic IMAP/SMTP provider
  (enabled by default since 3.9.1, May 2013) gaining mail-autoconfig XML support, and
  the Google provider dropping its embedded client secret. Providers today: Google,
  Microsoft personal, Microsoft 365, Exchange, Nextcloud, WebDAV, Kerberos, IMAP/SMTP
  [proven]. Note the OAuth-identity wrinkle relevant to D26: NEWS lists bug "#335
  Default Microsoft 365 client is unverified" — shared desktop clients carry the same
  consent/verification risks as KDE's.
- **Cinnamon [proven]:** uses GOA, not its own stack. cinnamon-control-center shipped a
  GOA panel (`cinnamon-control-center-goa`); when GOA 3.50 moved to GTK4, Mint switched
  Cinnamon Settings to `xapp-project/gnome-online-accounts-gtk` (a GTK3 frontend over
  the same daemon). Account data therefore flows to GOA-aware apps only.
- **XFCE [proven]:** no online-accounts facility at all; Xubuntu ships none; documented
  user workaround is installing `gnome-online-accounts` + `gnome-control-center` (or the
  GTK fork) manually. Geary on XFCE works if GOA is installed and pre-configured.
- **COSMIC [proven]:** upstream (pop-os/cosmic-settings) has only local *Users*
  settings; no system online accounts as of COSMIC's 1.x era. A community project
  `cosmic-utils/accounts` ("Accounts for COSMIC", Rust, GOA-inspired, ~37 stars, no
  releases) exists but is unofficial and immature [proven].
- **elementary OS [proven]:** Pantheon's switchboard Online Accounts plug writes
  accounts into evolution-data-server sources (libedataserver), not GOA; sandboxed apps
  have documented trouble retrieving host EDS credentials.

## 3. Mail-client precedents (setup path → system-account use)

| Client | Setup (documented) | System accounts? |
|---|---|---|
| Thunderbird | Own OAuth2 provider registry in source (`OAuth2Providers.sys.mjs`; built-in creds/scopes for Google/MS/etc., browser or internal flow) | **No.** Bug 847989 "Support GNOME Online Accounts" open since 2013, status NEW; staff: "the core team will not develop" it [proven] |
| Geary | GOA accounts auto-appear; FAQ: "Geary uses gnome-online-accounts to implement both OAuth2 authentication and single-sign-on." Without GOA, only password login — which Google no longer accepts → GOA effectively required for Gmail [proven] | Yes: GOA |
| Evolution | GNOME help: GOA-configured mail accounts are "automatically picked"; "not all options of such accounts can be edited in Evolution"; removal only via Online Accounts. Accounts materialize as evolution-data-server `.source` files under `~/.config/evolution/sources` [proven] | Yes: GOA → EDS |
| KMail | Own Account Wizard + manual IMAP/POP config (docs.kde.org); Akonadi, not KAccounts [proven] | No (see §1) |
| Mimestream | Google-only; in-app "Sign in with Google" via ASWebAuthenticationSession (AuthenticationServices); refresh token on-device in Keychain; never touches a Mimestream server [proven] | No documented Internet Accounts use |
| Spark (Readdle) | Provider picker + OAuth; first account becomes "email for sync" synced via Spark's cloud [proven] | No documented Internet Accounts use |
| Airmail | Own provider picker (Gmail/iCloud/Yahoo/O365/EWS/manual IMAP-POP3). Its "Local Accounts" are on-disk mailboxes/imported archives (EMLX/MBox), **not** system accounts [proven] | No documented Internet Accounts use |
| Canary | Own OAuth flows (Google/Office365 sign-in pages in-app) + manual providers [proven] | No documented Internet Accounts use |
| Outlook for Mac | Tools→Accounts OAuth; "new Outlook" syncs non-Microsoft accounts (Google, iCloud, Yahoo, IMAP) through the Microsoft Cloud unless disabled [proven] | No documented Internet Accounts use |
| eM Client | Automatic setup wizard + own OAuth window for Gmail/Outlook/Yahoo/AOL [proven] | No documented Internet Accounts use |
| Mailspring | Own Mailspring ID + in-app provider sign-in [proven] | No (own cloud identity instead) |

## 4. Does any third-party Mac mail client read Internet Accounts?

**No public evidence of any.** The mechanism blocks it, not just the market:

- The only public Accounts API, `ACAccountStore`/Accounts.framework, was deprecated in
  macOS 12 and always covered social account types (Twitter/Facebook/…) — never mail
  [proven: Apple docs metadata; dev-forums thread 756517 shows a developer hitting the
  deprecation when trying to create an Internet Account programmatically].
- Mail-capable Internet Accounts live behind private surfaces: the `InternetAccounts`
  keychain access group appears in `com.apple.private` entitlement lists usable only by
  Apple-signed binaries (XPN security research); the account DB sits in
  `~/Library/Accounts` under SIP/TCC-adjacent protections [proven].
- Every surveyed vendor (§3) documents its own sign-in — OAuth via
  ASWebAuthenticationSession/browser or app passwords — and none documents reading
  Internet Accounts. **[inferred]** no supported path exists; the sqlite could
  theoretically be read with Full Disk Access outside the sandbox, but no mail client
  publicly documents doing so **[unverified for a negative by nature]**.

## 5. Implications for Sweetbriar (memo scope)

- Linux "system accounts" are fragmented, not one API: GOA (real, maintained, mail-capable,
  used by Geary/Evolution), KAccounts (no mail service, KDE PIM ignores it, upstream
  near-stagnant, replacement prototype in flight), Cinnamon=GOA-by-proxy, XFCE=nothing,
  COSMIC=nothing-official, Pantheon=EDS sources. **[inferred]** A system-account
  integration story on Linux ≈ "GOA + maybe konlineaccounts later"; KAccounts offers no
  mail value.
- D26 (own OAuth registrations) is validated by precedent: every client above runs its
  own client IDs; even GOA/KDE ship shared IDs with documented consent problems (KDE
  client "not approved for Drive"; GNOME had "Default Microsoft 365 client is
  unverified" bug #335). Using GNOME/KDE's tokens would import their app-identity
  problems and ToS ambiguity.
- D22 (contacts): the one working cross-app precedent is contacts/calendar, not mail —
  KDE's KAccounts→Akonadi bridge and GOA's Contacts service both cover PIM data. On
  macOS, EventKit/Contacts.framework similarly expose account-derived data legally
  (see companion memo).
- Rust-core ownership: any Linux integration should be a *token/config source* behind a
  core trait (GOA `GetAccessToken` over session D-Bus is the only realistic feed),
  matching how Evolution treats GOA accounts as externally-managed, non-editable.

## Consulted URLs

KDE/Accounts-SSO:
- https://github.com/KDE/kaccounts-integration
- https://github.com/KDE/kaccounts-providers (+ /tree/master/providers, /tree/master/services, /tree/master/services/disabled)
- https://github.com/KDE/kaccounts-providers/blob/master/providers/google.provider.in
- https://github.com/KDE/konlineaccounts
- https://nicolasfella.de/posts/a-new-online-accounts-system/ (also https://planet.kde.org/nicolas-fella-2025-11-15-a-new-online-accounts-system/)
- https://invent.kde.org/system/konlineaccounts/-/blob/master/README.md
- https://gitlab.com/accounts-sso/signond (+ /-/commits/master, /-/merge_requests/36)
- https://github.com/NixOS/nixpkgs/pull/517355
- https://phabricator.kde.org/R165:b2b0dd482b213ba774d582e427c37b400dc4b997
- https://discuss.kde.org/t/why-does-every-kde-pim-program-have-its-own-account-configuration-ui-why-no-kcm-for-that/6803
- https://docs.kde.org/trunk_kf6/en/kmail/kmail2/getting-started.html , https://userbase.kde.org/KMail/Account_Wizard
- https://forums.ubports.com/assets/uploads/files/1785441953417-usr.bin.morph-browser.txt (D-Bus names, accounts.db path)
- https://lazka.github.io/pgi-docs/Accounts-1.0/constants.html
- https://github.com/OpenMandrivaAssociation/distribution/issues/3020

GNOME/desktops:
- https://github.com/GNOME/gnome-online-accounts/blob/master/NEWS , https://gitlab.gnome.org/GNOME/gnome-online-accounts/-/raw/gnome-48/NEWS , /-/raw/gnome-46/NEWS
- https://lists.gnome.org/archives/gnome-announce-list/2013-May/msg00001.html (IMAP/SMTP default)
- https://help.gnome.org/gnome-help/accounts-add.html
- https://help.gnome.org/evolution/intro-first-run.html
- https://wiki.gnome.org/Apps/Geary/FAQ
- https://discourse.gnome.org/t/oauth2-in-geary/30757
- https://github.com/linuxmint/cinnamon/commit/d22f889c376734f0ca5d904885c2772e790fbadc
- https://github.com/linuxmint/cinnamon-control-center/issues/335
- https://packages.ubuntu.com/jammy/x11/cinnamon-control-center-goa
- https://snaprails.blogspot.com/2024/07/online-accounts-in-xubuntu-2404-noble.html
- https://github.com/cosmic-utils/accounts
- https://github.com/pop-os/cosmic-settings , https://rustutils.com/tools/cosmic-settings/
- https://github.com/elementary/switchboard-plug-onlineaccounts , https://discourse.gnome.org/t/flatpack-retrieve-credentials-from-host-eds/7359

macOS/Internet Accounts:
- https://www.apple.com/os/macos/ , https://www.macrumors.com/2026/09/14/apple-releases-macos-golden-gate/ , https://support.apple.com/en-us/149035 , https://osxdaily.com/2026/09/14/macos-27-golden-gate-released-mac-download-update/
- https://developer.apple.com/documentation/accounts/acaccountstore
- https://developer.apple.com/forums/thread/756517 (fetch blocked; content via search snippet)
- https://blog.xpnsec.com/bypassing-macos-privacy-controls/
- https://stackoverflow.com/questions/23141694/how-do-clients-access-the-internet-account-database (fetch blocked; question premise via snippet)

Mail clients:
- https://mimestream.com/help/user-guide/adding-accounts , /supported-accounts , https://mimestream.com/trust/security-and-privacy
- https://support.readdle.com/spark/spark-onboarding/connect-to-your-email-account-in-spark , /add-more-email-accounts-to-spark
- https://help.airmailapp.com/en-us/article/add-new-account-airmail-for-macos-nnue2q/
- https://canarymail.io/help/adding-a-gmail-account-in-canary-macos , /adding-an-office365-account-in-canary-macos
- https://support.microsoft.com/en-us/outlook/add-email-accounts-in-outlook-for-mac-using-microsoft-cloud , /sync-account-in-outlook-for-mac-to-microsoft-cloud
- https://www.emclient.com/webdocumentation/en/10.0/eMClient/Content/Accounts/Create%20New%20Account.htm , https://www.emclient.com/blog/how-do-i-create-or-add-a-new-email-account-in-em-client-606
- https://www.getmailspring.com/docs/what-is-a-mailspring-id , /quick-start-guide
- https://bugzilla.mozilla.org/show_bug.cgi?id=847989 , https://source-docs.thunderbird.net/en/latest/backend/oauth.html , https://github.com/thunderbird/thunderbird-desktop/blob/e7422cc1d82fcb4bfb8e0989e1ed3982d5f0f24c/mailnews/base/src/OAuth2Providers.sys.mjs
