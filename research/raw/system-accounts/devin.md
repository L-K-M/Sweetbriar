<!-- provenance: helper=devin model=swe-2-max started=2026-10-12T04:23:53+00:00 exit=124 brief=research/briefs/ -->

# Using system-level accounts (macOS Internet Accounts, GNOME Online Accounts, KDE Accounts)

**Status: DRAFT — findings partially written; GOA/KDE/precedents sections pending helper verification.**

Author: Devin (SWE-2 Max), 2026-10-12. Research for the Sweetbriar email client.
Question from the owner: "macOS and I think also Linux allow logging into
accounts on system level. Can we use these accounts?"

Time-sensitive facts carry the date they were checked. Settled decisions this
report must respect: own OAuth app registrations (D26), Microsoft's managed
consent policy and Google's restricted-scope verification
([research/06-account-backends](../../06-account-backends.md)), the
contacts source (D22: own store, macOS Contacts as read-only enrichment,
CardDAV later), and the Rust core owning accounts and sync
([research/01-architecture](../../01-architecture.md)).

## TL;DR

- **On macOS, the answer is no — for mail.** Internet Accounts has no public
  API for mail accounts: `ACAccountStore` is deprecated since macOS 12
  (2021), its only ever-public account types were social networks (Twitter,
  Facebook, LinkedIn, Weibo), and mail/iCloud/Google/Exchange accounts were
  never exposed. The account database (`~/Library/Accounts`) and
  `~/Library/Mail` are TCC-protected, and Mail's OAuth material lives in the
  data-protection keychain, unreachable by a third-party signature.
- **What macOS does give you:** calendar and contacts *data* from
  Internet-Accounts-configured Exchange/CalDAV/CardDAV accounts via EventKit
  and Contacts.framework (with user permission). That is real, supported,
  and useful for D22 — but it is data, not credentials.
- **One honest "import" path exists on macOS:** Mail's AppleScript
  dictionary exposes account names and server settings (no secrets) under
  the Automation permission. Good for a "migrate from Apple Mail" wizard.
- **Microsoft SSO on macOS is real but enterprise-scoped:** MSAL on macOS
  automatically uses the Enterprise SSO plug-in (Company Portal app +
  MDM-pushed config). A Sweetbriar Swift UI can acquire Graph tokens via
  MSAL and hand them to the Rust core — but only on managed devices.
  Consumer path stays our own `ASWebAuthenticationSession` OAuth, which now
  reuses the user's default browser session in Safari, Chrome, Edge — and
  Firefox as of 158 (2026-10-13).
- **GNOME Online Accounts is the opposite situation: a real, documented
  D-Bus API any app may call** — including `GetAccessToken` — and GOA's
  Google provider requests `https://mail.google.com/` (full Gmail IMAP/SMTP
  scope), while its Microsoft 365 provider requests Graph
  `mail.readwrite`/`mail.send`. Evolution and Geary consume it today,
  including inside Flatpak. The catch: tokens belong to GNOME's OAuth
  clients, so identity, consent screens, quotas and verification are
  GNOME's, not Sweetbriar's.
- **KDE's KAccounts/libaccounts/signond exists but is weakly maintained and
  weakly consumed**; KMail largely does its own setup.
- **Recommendation:** treat system accounts as an optional *import source*
  and *token source of last resort*, never as Sweetbriar's account
  authority. Keep own-OAuth as the primary path (D26 stands); add a
  credential-source abstraction in the core (`sb-platform` leaf); use GOA on
  Linux as a first-class convenience path; on macOS use EventKit/Contacts
  for enrichment and MSAL-brokered SSO where the enterprise offers it.

## Findings

### 1. macOS Internet Accounts

#### 1.1 What Internet Accounts is on macOS 26/27

System Settings → Internet Accounts is a single pane backed by the per-user
launch agent `accountsd` (`com.apple.accountsd`, at
`/System/Library/Frameworks/Accounts.framework/Versions/A/Support/accountsd`).
It brokers account records and credentials for Apple apps: account
definitions live in `~/Library/Accounts/Accounts4.sqlite` (the
`Accounts4.sqlite` name dates to macOS 10.12) and OAuth/password material
lives in the keychain
([hardenmac on accountsd](https://hardenmac.com/what-is-com-apple-accountsd/);
[forensic write-up of Accounts4.sqlite](https://www.cyberengage.org/post/understanding-macos-app-preference-files-mru-files-shared-file-lists-and-account-artifacts-for-di),
checked 2026-10-12). `accountsd` holds private entitlements including
`com.apple.private.accounts.allaccounts` and
`com.apple.private.security.storage.Mail`
([Jonathan Levin's entitlement database, macOS 15](https://newosxbook.com/ent.php?exec=accountsd&osVer=MacOS15)).

Account types the pane accepts today (checked on Apple Support docs for
macOS 26/27, 2026-10-12): iCloud (via Apple Account), Google, Microsoft
Exchange, Yahoo, AOL, and "Other" (IMAP/POP mail, CalDAV calendar, CardDAV
contacts, LDAP directory)
([Apple Mac User Guide, "Add an internet account"](https://support.apple.com/en-ca/guide/mac-help/mh43559/mac)).
Each account exposes feature toggles per service (Mail, Contacts,
Calendars, Notes, Reminders, Freeform…) that only Apple apps honor. For
Microsoft 365 accounts, macOS still authenticates via EWS — Apple is
"actively working with Microsoft" on Graph for a future macOS 27 update,
absent as of 27.0.1 ([MacRumors,
2026-10-01](https://www.macrumors.com/2026/10/01/apple-mail-mac-stop-sync-microsoft-365/);
[heise, 2026-10](https://www.heise.de/en/news/Exchange-Web-Services-shutting-down-Apple-Mail-is-not-yet-ready-11471692.html)).

#### 1.2 Is there a public API to read those accounts? No.

**Accounts.framework is dead for this purpose.** Apple's own documentation
gives `ACAccountStore` an availability range of macOS 10.8.0–12.0.0
(deprecated in macOS 12 Monterey, 2021) and iOS 6.0–15.0
([ACAccountStore](https://developer.apple.com/documentation/accounts/acaccountstore),
[ACAccountType](https://developer.apple.com/documentation/accounts/acaccounttype);
checked 2026-10-12). The deprecation message tells developers to "use the
appropriate non-Apple SDK corresponding to the type of account you want to
reference"
([Apple Forums thread 756517](https://developer.apple.com/forums/thread/756517)).

Even before deprecation, the public framework only ever exposed **social**
account types — `ACAccountTypeIdentifierTwitter`, `Facebook`, `LinkedIn`,
`SinaWeibo`, `TencentWeibo` — all now deprecated
([Accounts deprecated symbols](https://developer.apple.com/documentation/accounts/deprecated-symbols)).
There was never a public identifier for iCloud, Google, Exchange or mail
accounts. The identifiers that Mail and accountsd use internally
(`com.apple.account.*` types in `ZACCOUNTTYPE`) are private; calling the
XPC interface they back requires private entitlements a third-party
signature cannot satisfy.

**Bottom line:** no supported API lets a sandboxed or Developer ID app
enumerate Internet Accounts, read their server settings, or obtain their
credentials. This is the intended design, not an oversight — Apple treats
account credentials as belonging to the system and to Apple apps.

#### 1.3 What you CAN reach indirectly — calendars and contacts

Two supported frameworks expose *data synced through* Internet Accounts:

- **EventKit.** `EKEventStore.sources` returns one `EKSource` per account
  (local, iCloud/CalDAV, `exchange`, subscribed, birthdays) with the
  calendars under it ([EKSource](https://developer.apple.com/documentation/eventkit/eksource),
  [EKSourceType](https://developer.apple.com/documentation/eventkit/eksourcetype)).
  An Exchange account added in Internet Accounts appears as an
  `EKSourceType.exchange` source; a CalDAV "Other" account appears as
  `calDAV`. Requires user permission: since macOS 14, write-only and
  full-access levels exist
  ([TN3152](https://developer.apple.com/documentation/technotes/tn3152-migrating-to-the-latest-calendar-access-levels)).
  This is how Fantastical, BusyCal and Outlook-for-Mac's calendar can show
  "the same calendars as Calendar.app" without re-login.
- **Contacts.framework.** `CNContactStore` reads the unified contact
  database, whose containers are typed `local`, `exchange`, `cardDAV`,
  `unassigned` — i.e., contacts synced from Exchange and CardDAV accounts
  configured in Internet Accounts are included
  ([CNContainerType](https://developer.apple.com/documentation/contacts/cncontainertype);
  [Nylas's Contacts framework explainer](https://developer.nylas.com/docs/cookbook/contacts/apple-contacts-api/)).
  Access needs `NSContactsUsageDescription` and the user can grant full or
  limited (selected-contacts) access
  ([Accessing the contact store](https://developer.apple.com/documentation/contacts/accessing-the-contact-store)).
  This is exactly the D22 "read-only enrichment" path already recommended.

Note the asymmetry: there is **no "MailKit" equivalent**. Apple provides
EventKit for calendars and Contacts for contacts, but nothing that vends
mailbox data or mail account configuration to third parties.

#### 1.4 Keychain: can we read what Mail stored? Effectively, no.

macOS has two keychains: the legacy file-based keychain (deprecated
direction; `SecKeychain*` APIs deprecated in macOS 12) and the
data-protection keychain ([TN3137](https://developer.apple.com/documentation/technotes/tn3137-on-mac-keychains)).
Per Apple's platform-security docs, "Mail accounts" and "LDAP, CalDAV,
CardDAV" credentials are keychain items accessible "after first unlock" —
but accessible *to the system*, not to arbitrary apps
([Keychain data protection](https://support.apple.com/guide/security/keychain-data-protection-secb0694df1a/web)).

Empirically verified by the `apple-mail-mcp` project (April 2026):
zero `ptcl=imap`/`imps` items exist in the login or system keychain on a
modern macOS — Mail.app's OAuth material lives in the TCC-protected
Accounts database and data-protection keychain, "not reachable from an
unsigned user-scoped process"
([apple-mail-mcp commit notes](https://github.com/s-morgan-jeffries/apple-mail-mcp/commit/c90c10d42e120f3fdf1318d6390b9d71de7e8c59)).
The `gena="Google OAuth"` generic-password items that do exist in the
login keychain belong to third-party apps (e.g., Fantastical) that ran
their *own* OAuth flow, not to Mail.

Even where a legacy login-keychain item exists, its ACL is bound to the
creating app's code signature; a Sweetbriar request would produce a
keychain prompt naming Sweetbriar — and for OAuth tokens held by
`accountsd`, access-group restrictions simply deny us at the securityd
level. Reading another app's keychain items is both unreliable and
indefensible against our credential-safety stance (D26: "credential
safety" — the user should never need to hand us another app's secrets).

#### 1.5 Private APIs and protected files

- `~/Library/Accounts/` and `~/Library/Mail/` are TCC-protected:
  `~/Library/Mail` has been in the protected list since Mojave, and both
  effectively require Full Disk Access for a third-party process
  ([Eclectic Light on Mojave protections](https://eclecticlight.co/2018/09/06/working-with-mojaves-privacy-protection/);
  [Apple.SE: Envelope Index requires FDA](https://apple.stackexchange.com/questions/478129/authorisation-denied-accessing-macos-library-mail-v10-maildata-envelope-index)).
  Full Disk Access cannot be requested programmatically, is not available
  to App Store apps, and is far more privilege than reading accounts
  justifies.
- Directly editing `Accounts4.sqlite` would corrupt accountsd's view and
  is user-hostile; people only do it to *remove* broken accounts
  ([Apple.SE workaround](https://apple.stackexchange.com/questions/339161/how-to-delete-gamecenter-and-icloud-from-internet-accounts-using-the-terminal)).
- Private framework usage is an App Store rejection under guideline
  2.5.1 ([private-API rejection notes](https://appstorereject.com/md/rejections/apple/2/guideline-251-software-requirements-using-private-or-undocumented-apis)).
  Notarization itself is a malware/code-signing scan, not App Review
  ([Notarizing macOS software](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution)) —
  but the private path fails *technically* anyway: `com.apple.private.*`
  entitlements are only honored for Apple-signed/platform code, so our
  Developer-ID binary simply gets denied at the XPC/entitlement check.

#### 1.6 AppleScript: the one honest "account import" path

Mail.app's scripting dictionary exposes `account` objects with their
settings — name, email addresses, enabled state, server configuration —
but never passwords or tokens. A third-party app can enumerate them under
the Automation (Apple Events) TCC permission. Verified pattern used in the
wild: enumerate accounts via JXA `Mail.accounts()`; needs only the
"control Mail" automation grant, not Full Disk Access
([idemeum checkMailAccountConfig.ts](https://github.com/idemeum/skills/blob/main/checkMailAccountConfig.ts)).
In a sandboxed build this requires
`com.apple.security.temporary-exception.apple-events` scoped to
`com.apple.mail` (the temporary-exception entitlements are nominally for
App Review approval) or whatever scripting access groups Mail's sdef
declares; for a Developer-ID build it just needs the user's Automation
consent ([QA1802](https://developer.apple.com/library/archive/qa/qa1802/_index.html)).

This yields a genuinely useful onboarding feature: "Import account
settings from Apple Mail" — server names, usernames, mailbox structure —
followed by our own OAuth/password prompts for credentials.

#### 1.7 Microsoft Enterprise SSO plug-in and Platform SSO

This is the **one supported way** a third-party Mac app can get
system-brokered credentials — but only for Microsoft Entra accounts on
managed devices.

- The **Enterprise SSO plug-in** ships inside the Intune Company Portal
  app (macOS 10.15+; Microsoft Authenticator on iOS). Requirements: the
  device is MDM-enrolled, the plug-in is enabled by pushed configuration,
  and Apple's associated-domain check approves the plug-in for the IdP
  ([Microsoft Enterprise SSO plug-in](https://learn.microsoft.com/en-us/entra/identity-platform/apple-sso-plugin),
  checked 2026-10-12).
- **Platform SSO** (macOS 13+) additionally signs the user into the Mac
  itself with Entra credentials, Touch ID or a Secure Enclave key; it
  includes the same SSO app extension for apps
  ([Apple Platform SSO](https://support.apple.com/guide/deployment/platform-sso-for-macos-dep7bbb05313/web);
  [Microsoft macOS PSSO](https://learn.microsoft.com/en-us/entra/identity/devices/macos-psso)).
- **App side:** MSAL for iOS/macOS (≥ 1.1.0) automatically invokes the
  plug-in for interactive and silent token requests, account enumeration
  and removal — no special config beyond the standard
  `msauth.<bundle-id>://auth` redirect and keychain-sharing group
  ([Integrate with the SSO broker](https://learn.microsoft.com/en-us/entra/identity/devices/macos-psso-deployment-eocustomers)).
  Non-MSAL apps get SSO only if the admin allow-lists their bundle ID in
  the SSO extension profile (`AppPrefixAllowList`), which routes their
  URLSession/ASWebAuthenticationSession requests to login endpoints
  through the extension
  ([Intune SSO app extension config](https://learn.microsoft.com/en-us/intune/device-configuration/templates/configure-enterprise-sso-plugin-macos)).

**Fit for Sweetbriar:** MSAL exists for iOS/macOS (ObjC/Swift). The Swift
UI layer can run `MSALPublicClientApplication` for the "Microsoft 365"
account type; on managed Macs with the plug-in, the user gets one-click
or zero-click SSO; elsewhere it falls back to the same
`ASWebAuthenticationSession` flow we'd run anyway. The acquired access
token (plus refresh bookkeeping owned by MSAL's cache in the app-group
keychain) is handed to the Rust core as an opaque credential — same shape
as a self-acquired OAuth token. The cost: one more token-source type and
linking MSAL. It does **not** remove the consent problem — on tenants
with Microsoft's managed consent policy, our app registration still needs
admin consent for `Mail.ReadWrite`; the broker just supplies SSO for
already-consented scopes.

#### 1.8 ASWebAuthenticationSession: browser-session reuse is now real

`ASWebAuthenticationSession` on macOS opens the authentication URL in the
user's **default browser if that browser implements
`ASWebAuthenticationSessionWebBrowserSessionHandling`**, otherwise
Safari; the session shares the browser's cookies unless
`prefersEphemeralWebBrowserSession = true`
([ASWebAuthenticationSession](https://developer.apple.com/documentation/authenticationservices/aswebauthenticationsession);
[browsers opt in via Info.plist](https://developer.apple.com/documentation/authenticationservices/supporting-single-sign-on-in-a-web-browser-app)).
So for a user already signed into Google/Microsoft in their browser,
OAuth is one click — *if* their browser participates.

Browser support status (checked 2026-10-12):

| Browser | ASWebAuthenticationSession handler |
|---|---|
| Safari | Always supported |
| Chrome/Edge/Brave/Arc (Chromium) | Supported — Chromium implements `ASWebAuthenticationSessionRequest` ([chromium source](https://chromium.googlesource.com/chromium/src/+/main/chrome/browser/mac/auth_session_request.mm)) |
| Firefox | Implemented in Firefox 157 milestone per Bugzilla, but testers report it actually working in the 158 beta/nightly; 158 ships 2026-10-13 ([bug 1921535](https://bugzilla.mozilla.org/show_bug.cgi?id=1921535); [eduVPN verification](https://codeberg.org/eduVPN/apple/issues/589)) |

Practical consequence: Sweetbriar's OAuth flow should use
`ASWebAuthenticationSession` (system API, default-browser session reuse,
no copied URLs), with a documented fallback to opening the system browser
+ localhost loopback for the rare unsupported-browser case. This is
already the D26 design; the browser-support news merely makes it better.

#### 1.9 Managed configuration and MDM

Three distinct mechanisms, only two of which help a third-party app:

1. **System account payloads** (`com.apple.mail.managed`,
   `com.apple.eas.account`, CalDAV/CardDAV payloads; DDM
   `com.apple.configuration.account.mail`, macOS 13+). These create
   *system* Internet Accounts for Apple Mail only — invisible and useless
   to us ([Apple Mail payload](https://support.apple.com/guide/deployment/mail-payload-settings-dep9c14bfc5/web);
   [DDM account.mail declaration](https://github.com/apple/device-management/blob/release/declarative/declarations/configurations/account.mail.yaml)).
2. **Managed preferences** — any MDM can push a plist into our bundle
   ID's defaults domain (Intune "preference file", Jamf "Application &
   Custom Settings") ([Intune preference files](https://learn.microsoft.com/en-us/intune/device-configuration/templates/configure-preference-file-macos);
   [Jamf](https://learn.jamf.com/r/en-US/technical-articles/Deploying_Custom_Computer_Configuration_Profiles_Using_the_Application_and_Custom_Settings_Payload)).
   We could define a documented `accounts` key so admins pre-seed server
   settings/identities — but never credentials.
3. **NEW in macOS 27 — the `ManagedApp` framework:** third-party apps can
   adopt `ManagedApp` to receive *managed app configurations and secrets*
   (passwords, certificates, hardware-bound identities, attested device
   keys) deployed via `com.apple.configuration.app.managed` / declarative
   app management
   ([Distribute managed apps](https://support.apple.com/en-am/guide/deployment/dep575bfed86/web);
   [WWDC26 app-management notes](https://support.apple.com/guide/deployment/app-management-updates-depd567c9ffa)).
   This is the first real channel for an enterprise to provision a
   third-party app with account material (e.g., SCEP certs, tenant
   hints). Worth tracking for enterprise Sweetbriar.

#### 1.10 Other macOS mechanisms worth knowing

- **AutoFill credential provider + credential exchange.** Third-party
  password managers can vend credentials into system AutoFill
  (`ASCredentialProviderExtension`), and macOS 26 adds FIDO CXP/CXF
  credential *import/export* (`ASCredentialImportManager` /
  `ASCredentialExportManager`) — Apple Passwords can transfer
  passwords/passkeys app-to-app ([Apple security guide](https://support.apple.com/guide/security-pdf/credential-provider-extensions-sec6319ac7b9/web);
  [ASCredentialExportManager](https://developer.apple.com/documentation/authenticationservices/ascredentialexportmanager)).
  Useful tangentially: a user could migrate an *app password* into our
  onboarding; and AutoFill can fill the IdP form inside
  ASWebAuthenticationSession automatically.
- **Extensible SSO is an IdP-side framework** (`ASAuthorizationSingleSignOnProvider`,
  `com.apple.extensiblesso` payload): Okta/Jamf/etc. build plug-ins like
  Microsoft's. Sweetbriar consumes the result (browser session + MSAL),
  it doesn't build a plug-in.
- **App Intents mail domain** — Apple ships a mail schema; Siri/Intelligence
  routes to the *default mail app*. Already in the plan (08-macos-ui);
  unrelated to system accounts but relevant to "default app" UX.

### 2. GNOME Online Accounts (GOA)

GOA is the mirror image of macOS Internet Accounts: a per-user accounts
daemon (`goa-daemon`) whose D-Bus API is *documented for any app to call*.

**Version and reach (checked 2026-10-12).** Current GOA is 3.58.x — the
generated docs report library version 3.58.1
([GOA D-Bus docs](https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/)).
Ubuntu 26.04 LTS ships GNOME 50 with `gnome-online-accounts` 3.58.x
([Ubuntu 26.04 package manifest](http://ca.releases.ubuntu.com/26.04/ubuntu-26.04-desktop-amd64.manifest);
[release notes](https://documentation.ubuntu.com/release-notes/26.04/changes-since-previous-interim/)).

**D-Bus API.** `org.gnome.OnlineAccounts` exposes an ObjectManager at
`/org/gnome/OnlineAccounts/Manager`; each account object carries
interfaces for whatever features are enabled:

- `org.gnome.OnlineAccounts.Account`: `ProviderType`, `ProviderName`,
  `Id`, `Identity`, `PresentationIdentity`, `AttentionNeeded`, `IsLocked`,
  per-feature `*Disabled` properties, `Remove()`,
  `EnsureCredentials()` (fails → sets `AttentionNeeded`; interactive auth
  is the GUI's job)
  ([docs](https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/dbus-org.gnome.OnlineAccounts.Account.html)).
- `org.gnome.OnlineAccounts.OAuth2Based`: `GetAccessToken() →
  (access_token, expires_in)` — call it whenever you need a token; GOA
  refreshes internally. It also exposes `ClientId` and `ClientSecret`
  properties
  ([docs](https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/dbus-org.gnome.OnlineAccounts.OAuth2Based.html)).
- `org.gnome.OnlineAccounts.PasswordBased`: `GetPassword()`.
- `org.gnome.OnlineAccounts.Mail`: `EmailAddress`, `Name`,
  `ImapSupported/ImapHost/ImapUserName/ImapUseSsl/ImapUseTls`,
  `SmtpSupported/SmtpHost/SmtpUserName/SmtpUseAuth/SmtpAuthLogin/
  SmtpAuthPlain/SmtpAuthXoauth2/…`
  ([docs](https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/dbus-org.gnome.OnlineAccounts.Mail.html)).
- `Calendar`, `Contacts`, `Documents`, `Files`, `Maps`, `Music`,
  `Printers`, `Todo`, `Ticketing`, `ReadLater`, `Chat`, `Photos` (several
  deprecated).

**Providers compiled in (GOA master `goabackend`, checked 2026-10-12):**
`google`, `ms_graph` (shown as "Microsoft 365"), `exchange` (EWS — for
on-prem/hosted Exchange), `imap_smtp` (generic mail), `owncloud`
(Nextcloud), `webdav` (generic CalDAV/CardDAV/files), `kerberos`,
`fedora`
([meson.build](https://raw.githubusercontent.com/GNOME/gnome-online-accounts/master/src/goabackend/meson.build);
[meson_options.txt](https://raw.githubusercontent.com/GNOME/gnome-online-accounts/master/meson_options.txt)).
Google Files support is now off by default (`google_files=false`; Ubuntu
notes GOA removed Google Drive integration in 26.04).

**Whose OAuth client? GNOME's — this is the central catch.**

- The Google provider uses a compile-time client id+secret (default
  `44438659992-…apps.googleusercontent.com` with a baked-in secret,
  overridable only at build time) and requests:
  `userinfo.email userinfo.profile calendar m8/feeds carddav
  mail.google.com tasks` — yes, **`https://mail.google.com/`, the full
  restricted Gmail IMAP+SMTP scope**
  ([goagoogleprovider.c](https://raw.githubusercontent.com/GNOME/gnome-online-accounts/master/src/goabackend/goagoogleprovider.c),
  fetched 2026-10-12). The Mail interface it builds advertises
  `imap.gmail.com`/`smtp.gmail.com` with `SmtpAuthXoauth2`.
- The Microsoft 365 provider uses client id
  `b155a604-3c31-4079-93b0-6bb6aa9d5464` (GNOME's Entra registration,
  public client, PKCE, no secret) and requests `offline_access` plus
  Graph scopes including `mail.readwrite mail.readwrite.shared mail.send
  mail.send.shared mailboxsettings.read`, calendars/contacts/files/tasks
  readwrite and `user.read` ([goamsgraphprovider.c](https://raw.githubusercontent.com/GNOME/gnome-online-accounts/master/src/goabackend/goamsgraphprovider.c)).
  Uniquely, it also offers a "Use Organization Account" mode where the
  user supplies **their own Tenant ID and Client ID**, stored in the
  account keyfile — an enterprise can point GOA at its own app
  registration. (History lesson: until GOA 3.57.1 the default M365
  client was *unverified*, so users got the consent warning — GNOME
  had to rotate the registration; see
  [GOA NEWS, issue #335](https://raw.githubusercontent.com/GNOME/gnome-online-accounts/master/NEWS).)
- So a token from `GetAccessToken` is a token issued *to GNOME's
  application*: the consent screen the user saw said "GNOME" (or the
  org's app), Google's restricted-scope verification covers GNOME's app,
  and Microsoft's consent dialog names GNOME's registration. Sweetbriar
  using the token is indistinguishable at the API from Evolution using
  it. Evolution is a GNOME app; Sweetbriar is not — see the analysis in
  the recommendation section below.

**Who consumes it.** gnome-control-center (the UI), gvfs (Nextcloud/WebDAV
mounts), gnome-calendar/-contacts/-maps, Endeavour, Nautilus — and the
two relevant mail clients:

- **Evolution** via evolution-data-server: EDS has a GOA backend
  (`module-gnome-online-accounts`) that mirrors each GOA account into EDS
  sources (mail, calendar, contacts). Mail auth flows through the OAuth2
  credential provider, which calls `GetAccessToken` at connect time — so
  token refresh is GOA's problem, not Evolution's. `AttentionNeeded`
  surfaces as a re-auth prompt; deleting a GOA account removes the EDS
  sources and the account vanishes from Evolution. For Evolution that is
  fine — the EDS copy is a mirror of the server — but Sweetbriar must NOT
  copy that removal behavior, since our store is the archive.
- **Geary** links libgoa directly, lists GOA accounts in its welcome/
  account dialog, and similarly uses `GetAccessToken` for XOAUTH2
  ([Geary Flatpak manifest](https://github.com/flathub/org.gnome.Geary/blob/master/org.gnome.Geary.yaml)
  shows the wiring).

**Flatpak.** Both Geary and Evolution ship on Flathub with
`--talk-name=org.gnome.OnlineAccounts` (Geary additionally talks to
`org.gnome.ControlCenter` to deep-link the account pane and to
`org.gnome.evolution.dataserver.*` for EDS contacts)
([Geary manifest](https://github.com/flathub/org.gnome.Geary/blob/master/org.gnome.Geary.yaml);
[Evolution manifest](https://github.com/flathub/org.gnome.Evolution/blob/master/org.gnome.Evolution.json)
— verified 2026-10-12).
Flathub review accepts session-bus talk-names case-by-case; GOA is a
recognized name used by shipped apps. **No xdg-desktop-portal covers
online accounts** — the only "Account" portal
(`org.freedesktop.impl.portal.Account`, `GetUserInformation`) returns the
user's own name/avatar, not service accounts; elementary filed a
portal request that was never implemented
([elementary/portals#19](https://github.com/elementary/portals/issues/19)).

**Snap.** snapd ships an `accounts-service` interface granting exactly
the GOA session-bus surface — but `deny-auto-connection`, i.e. manual
`snap connect` or a store assertion is required
([snapd accounts_service.go](https://github.com/canonical/snapd/blob/master/interfaces/builtin/accounts_service.go)).
A `.deb` has no confinement at all.

**Limits that matter for a mail client:**

- Scopes are fixed per provider — an app cannot ask GOA for arbitrary or
  narrower scopes (already noted in [09-linux](../../09-linux.md)).
- `GetAccessToken` is per-access-token; the app must call it fresh each
  connection and handle `AttentionNeeded` (→ "Fix in Settings → Online
  Accounts").
- Account removal, feature toggles (`MailDisabled`) and daemon absence
  (KDE session, minimal installs, Flatpak without the talk-name) are all
  real states to handle.
- GOA's Google client is a *public* client with a public "secret" — fine
  for OAuth semantics, but it means every user of every distro shares one
  client identity and quota.
- Microsoft's managed consent policy applies to GNOME's app just as it
  does to ours: `Mail.ReadWrite`/`Mail.ReadBasic` are excluded from user
  consent in default-managed tenants, so GOA M365 setup hits the same
  wall — except an org can use GOA's "Organization Account" mode with its
  own Tenant/Client ID.
- The `exchange` (EWS) provider is dead for Exchange Online by the same
  EWS timeline that hits Apple Mail.

### 3. KDE and other desktops

**KAccounts** is KDE's system-level accounts facility: the
`kaccounts-integration` KCM plus `kaccounts-providers`, built on the
Accounts-SSO stack (`libaccounts-qt` + `signond`, the Nokia/Intel lineage
also used by Sailfish and the old Ubuntu Online Accounts). It is still
packaged — Ubuntu carries `kaccounts-integration 4:26.08.0`
([Launchpad](https://blueprints.launchpad.net/ubuntu/+source/kaccounts-integration/4:26.08.0-0ubuntu1))
— but the packaging activity is misleading: recent commits in both repos
are release-bot tags and l10n-script translation churn, not feature work
([invent.kde.org commit history](https://invent.kde.org/network/kaccounts-integration/-/commits/master),
checked 2026-10-12). And the picture under the hood is worse:

- **KDE PIM doesn't consume it — at all.** `grep` for
  `KAccounts|kaccounts|AccountsSSO|signond` across the current `kmail`,
  `kmail-account-wizard` and `libkgapi` source trees returns **zero
  hits** (checked 2026-10-12, invent.kde.org master). KDE's own
  replacement project says it plainly: "Not many applications actually
  use it. Notably absent is KDE PIM"
  ([KOnlineAccounts README](https://github.com/KDE/konlineaccounts)).
  Configuring Google in System Settings → Online Accounts does not
  configure KMail — it never did
  ([KDE Discuss thread](https://discuss.kde.org/t/why-does-every-kde-pim-program-have-its-own-account-configuration-ui-why-no-kcm-for-that/6803)).
- **The provider list is a shell.** `kaccounts-providers` still ships a
  `google.provider` (plus Nextcloud/ownCloud), but the Google *services*
  that would make it useful — `google-calendar.service`,
  `google-contacts.service`, etc. — live in the source tree under
  `providers/disabled/` and `services/disabled/` upstream
  ([kaccounts-providers tree](https://invent.kde.org/network/kaccounts-providers/-/tree/master/providers),
  checked 2026-10-12). There is no mail service at all; KAccounts was
  never a mail-account facility, only a credential one.
- **The Accounts-SSO base is unmaintained and sandbox-hostile.** Apps
  read the accounts DB directly from the home directory — all-or-nothing
  access, no per-app grant, unusable under Flatpak confinement. Upstream
  `signond` (gitlab.com/accounts-sso) is kept on life support by the
  Sailfish maintainer — last tag `VERSION_8.61`, March 2022; occasional
  fixup commits in 2026, no development
  ([accounts-sso/signond](https://gitlab.com/accounts-sso/signond),
  checked 2026-10-12).
- **KDE is building a replacement.** `KOnlineAccounts` (created
  2025-12-02, prototype) is a proper daemon (`konlineaccountsd`) with a
  D-Bus API and per-app access control; providers so far are Nextcloud,
  Mastodon, Google ([repo](https://github.com/KDE/konlineaccounts)).
  Watch it, don't build on it yet.

**Other desktops:**

- **Cinnamon / MATE / Xfce / Budgie:** since GOA 3.50 moved to GTK4,
  these desktops lost their embedded account panel and Linux Mint's XApp
  project ships `gnome-online-accounts-gtk`, a standalone GTK4 frontend
  over the *same* `goa-daemon` — so GOA accounts on Mint are still GOA
  accounts readable through the same D-Bus API
  ([xapp-project/gnome-online-accounts-gtk](https://github.com/xapp-project/gnome-online-accounts-gtk);
  [Mint 22 notes](https://www.linuxmint.com/rel_wilma_whatsnew.php)).
  Coverage on those desktops depends on users having set accounts up at
  all — far fewer do.
- **elementary OS:** its own Online Accounts plug writes
  evolution-data-server sources directly (EDS collections, libsecret
  secrets) rather than GOA
  ([switchboard-plug-onlineaccounts](https://github.com/elementary/switchboard-plug-onlineaccounts)).
  Reading it would mean integrating EDS — a heavyweight dependency — for
  a small audience.
- **COSMIC (System76):** no system accounts facility found (unverified —
  still a young DE).

**Linux summary:** GOA is the only game that matters on Linux, and it
covers GNOME + the Mint-family desktops (Cinnamon/MATE/Xfce via
goa-gtk). On KDE, Sweetbriar's own auth path is the only option today;
KOnlineAccounts may change that in a year or two.

### 4. Precedents: what real mail clients do

The pattern is remarkably uniform — **every serious third-party mail
client does its own OAuth and treats system accounts as at most an
indirect convenience**:

- **Apple Mail** is the sole consumer of macOS Internet Accounts for
  mail (and it is currently broken for Exchange Online until Apple's
  Graph update ships).
- **Outlook for Mac** does its own account setup; Microsoft docs say to
  add accounts in-app, and Internet Accounts "is for setting up email
  account in native mail application"
  ([Microsoft Q&A](https://learn.microsoft.com/en-us/answers/questions/4634602/internet-accounts-settings-in-mac-when-using-outlo)).
  On managed Macs it rides the Enterprise SSO plug-in via MSAL.
- **Thunderbird**: own OAuth2 flows with its own client IDs for Google
  and Microsoft on every platform; no system-account integration anywhere.
- **Mimestream** (Gmail-only): own OAuth, completed Google's restricted-
  scope verification, uses `ASWebAuthenticationSession` on macOS 12.3+,
  refresh token stays on-device
  ([Mimestream security page](https://mimestream.com/trust/security-and-privacy);
  [account setup](https://mimestream.com/help/user-guide/adding-accounts)).
- **Spark, Airmail, Canary, eM Client, Mailspring** (Mac): all do their
  own OAuth flows; none can read Internet Accounts because there is no
  API to do it with (§1.2).
- **Calendar apps are the counter-example that proves the rule:**
  Fantastical and BusyCal *can* reuse Internet Accounts — through
  EventKit, the supported data API for calendars. There is no such API
  for mail.
- **Evolution and Geary (Linux):** consume GOA properly, as covered in
  §2 — including inside Flathub sandboxes. These are the only
  third-party-ish mail clients that genuinely use a system account store
  for credentials — and they are GNOME apps using GNOME's OAuth
  registrations.
- **KMail/Akonadi:** own wizard + own Google OAuth app; does not use
  KAccounts.
- **iOS/iPadOS** tells the same story as macOS: Apple's account store is
  off-limits to third-party mail; every iOS mail app does its own auth.
- **On Android** the model is different by design (AccountManager is a
  public API) — irrelevant to our platforms today but a hint of what a
  real system-account API looks like when a platform chooses to have one.

## Options compared

### What each "use" of system accounts is actually possible

| Use | macOS | Linux (GNOME) | Linux (KDE) |
|---|---|---|---|
| (a) Token source for mail sync | **Impossible** — no API, keychain unreachable | **Possible** — GOA `GetAccessToken` yields GNOME-client tokens for Google & M365 | **Not viable** — KAccounts unused by PIM; KOnlineAccounts is a prototype |
| (b) Import/autofill of server settings & identities | **Possible** — Mail's AppleScript dictionary (settings only, no secrets) | **Possible** — GOA `Mail` interface gives IMAP/SMTP host+user | Same as (a) — DB is technically readable but coverage is tiny |
| (c) SSO broker | **Possible, enterprise-only** — Entra via Enterprise SSO plug-in/Platform SSO + MSAL | **Partial** — GOA *is* the broker, for GNOME-client scopes | No |
| (d) Indirect contacts/calendars | **Yes** — EventKit & Contacts.framework expose Internet-Accounts-synced data | **Yes** — via GOA-linked apps/EDS data; or talk to EDS directly | Partial via Akonadi |

### Honesty check on "Use an account from System Settings"

| Platform | What it can honestly mean |
|---|---|
| macOS | "Import settings you already set up in Apple Mail" (AppleScript read) + "use your system calendars/contacts" (EventKit/Contacts). Never "we'll sign you in automatically". |
| GNOME | "Use this account from Online Accounts" — enumerate, import IMAP/SMTP settings, and for Google/M365 either (i) copy the *identity* and run our own OAuth (recommended), or (ii) use the GOA token directly (working but inherits GNOME's client identity). |
| KDE | Nothing today; fall back to our own setup wizard. Watch KOnlineAccounts. |

### The token-source question, side by side

| Path | Whose OAuth client | Consent screen shows | Refresh owned by | Failure modes we see |
|---|---|---|---|---|
| Sweetbriar own OAuth (D26) | Sweetbriar | "Sweetbriar" | us | our bugs only; full control |
| GOA `GetAccessToken` | GNOME | "GNOME" (or org via custom client id) | goa-daemon | AttentionNeeded→Settings; feature disabled; account removed; daemon absent; shared quota; **scope fixed** |
| MSAL brokered (macOS managed) | Sweetbriar app reg (+ broker) | ours | MSAL+plug-in | requires MDM+Company Portal; else silent fallback to normal flow |
| AppleScript import (macOS) | n/a (settings only) | n/a | n/a | Automation consent denied; Mail not configured |

## Recommendations for Sweetbriar

### Per platform

**macOS — "import settings, enrich data, broker where offered."**

1. **Do not use system accounts for credentials.** It is impossible
   (§1.2, §1.4). Own OAuth via `ASWebAuthenticationSession` remains the
   path (D26) — now with real default-browser session reuse in Safari,
   Chromium browsers and Firefox ≥158 (§1.8).
2. **Ship an "Import from Apple Mail" onboarding step** that reads
   Mail's accounts over AppleScript: server names, usernames, mailbox
   structure — then runs our own credential prompts. It is honest, needs
   only Automation consent, and gives the classic-client "it just knew my
   settings" feel (§1.6).
3. **Use EventKit + Contacts.framework as optional enrichment** per D22 —
   read-only, our store stays authoritative, never write back (§1.3).
4. **MSAL for the M365 account type in the Swift UI layer** — on managed
   Macs with the Enterprise SSO plug-in this yields true SSO; elsewhere
   MSAL falls back to the same browser flow. Tokens cross to Rust as
   opaque credentials. Evaluate in the enterprise phase, not the pilot
   (§1.7).
5. **Adopt `ManagedApp` (macOS 27) when enterprise deployment matters** —
   it is the supported channel for MDM-provisioned app config and even
   secrets/certs (§1.9).

**Linux (GTK client) — "GOA as a first-class convenience, never the authority."**

1. **Offer "Import from Online Accounts"** — enumerate GOA accounts,
   prefill IMAP/SMTP settings and identity, then run *our own* OAuth flow
   for branded clients (recommended default), or `PasswordBased.GetPassword`
   for password accounts where the user consents.
2. **Offer GOA tokens as a secondary token source** — a real convenience
   on GNOME desktops: `GetAccessToken` gives working Gmail-IMAP and
   Graph-Mail tokens today (§2). But present it honestly:
   "sign in through Online Accounts" not "sign in to Sweetbriar" — the
   consent was given to GNOME, and Google/Microsoft see GNOME's app.
   Strongly prefer own OAuth for anything beyond an import helper or a
   power-user option (see counter-arguments below).
3. **Handle the full state machine:** daemon absent (KDE, minimal
   installs, Flatpak without the talk-name), `MailDisabled`,
   `AttentionNeeded` (surface "fix in Settings → Online Accounts"),
   account removal (pause the account — **never delete local history**,
   matching D26's revocation semantics).
4. **Flatpak:** request `--talk-name=org.gnome.OnlineAccounts` — the
   Geary/Evolution precedent exists and Flathub accepts it; document why
   in the manifest comments and expect reviewer questions.
   **Snap:** `accounts-service` interface exists but is
   deny-auto-connection → manual `snap connect`; treat as best-effort.
   **deb:** unconstrained.
5. **KDE:** no integration today; own wizard. Re-evaluate when
   KOnlineAccounts ships.

### How it fits the architecture

- **Credential source is an `sb-platform` seam, not an account type.**
  In the layering from [01-architecture](../../01-architecture.md),
  platform credential sources belong in `sb-platform` (which already owns
  Keychain/Secret Service). Define a narrow `CredentialSource` trait:
  `OwnOAuth | GoaToken | MsalBroker | Password | AppPassword`. The
  backends ask the source for a token; the source owns refresh.
- **Identity and namespace stay stable across token-source changes.**
  An account's identity is (provider, mailbox/UPN/email) — resolved once
  at setup. Whether the token came from our OAuth, GOA, or MSAL does not
  change the sync namespace. Swapping the credential source later (e.g.,
  GOA account deleted → user does own OAuth for the same mailbox) keeps
  all local data; it is a *re-authentication*, not a new account.
- **Revocation/attention semantics are already right.** D26 says
  "revocation pauses the account and never deletes history." Map
  `AttentionNeeded`, 401s, and removed-GOA-accounts onto the same
  "needs attention" account health state; never quiesce by deleting.
- **The Rust core never links GOA.** On Linux, `sb-native-client` can
  call D-Bus directly (zbus) inside `sb-platform`; on macOS, the Swift
  layer supplies MSAL tokens over the existing command path. No new FFI
  shapes.

### The strongest counter-argument — and what would change this

**Against GOA tokens:** Evolution and Geary are GNOME applications;
GNOME's OAuth registrations exist for GNOME apps. When Sweetbriar calls
`GetAccessToken`, Google's and Microsoft's logs see GNOME's client — we
are, in effect, borrowing GNOME's verification status and quota. Google
could reasonably regard a non-GNOME commercial app riding GNOME's
restricted-scope client as verification circumvention, and GNOME could
reasonably restrict the D-Bus API or rotate the client id in response.
Microsoft's tenant consent likewise names "GNOME," which an admin may
legitimately refuse for a third-party they didn't evaluate. Even where
nobody objects, the UX lies: our consent screens and audit trails should
name us. **This is why GOA tokens should be a convenience path, never
the recommended one.** The import use (enumerate + prefill, then own
OAuth) captures most of the value with none of the borrowed identity.

**What would change the recommendation:**
- If GNOME published an explicit third-party API policy for GOA tokens
  (or a portal), GOA could move from "convenience" to "supported."
- If Google/Microsoft confirmed in writing that third-party use of GOA
  tokens is within terms — unlikely — same.
- If our own OAuth verification stalls (e.g., Google CASA timeline
  slips), GOA tokens become the *interim* path for Gmail on Linux —
  flagged to the user.
- If Apple ever ships a "MailKit" / account-broker API (as it did
  EventKit/Contacts), revisit macOS. File the Feedback Assistant request
  now either way.

## Risks and open questions

- **Google's view of non-GNOME apps using GOA tokens** — unverified;
  treat as a policy risk until checked. Does GNOME's client even retain
  `mail.google.com` verification permanently, and does the shared quota
  ever throttle? (Geary/Evolution haven't reported systematic blocks,
  but a third-party commercial app is a different profile.)
- **Microsoft tenant consent for GNOME's app** — under default-managed
  tenants `mail.readwrite` needs admin consent; test whether admins will
  approve an app they can't audit. GOA's org-account mode (custom
  Tenant/Client ID) may be the practical answer on enterprise Linux.
- **GOA scope drift** — the scope list is compile-time and has changed
  across releases (Drive now off by default). Pin assumptions; re-check
  per GOA release.
- **`AttentionNeeded` semantics** — verify what actually clears it
  (interactive auth in Settings only, or EnsureCredentials retries).
- **MSAL-on-macOS brokered path** — confirm on a real Intune-enrolled
  test device that a Developer-ID-signed, non-sandboxed app can use the
  SSO plug-in (the docs assume sandbox-capable apps; check keychain
  access-group requirements for the brokered cache).
- **AppleScript read of Mail accounts** — verify which fields Mail's
  sdef actually returns on macOS 27 (server hosts, usernames, aliases)
  and whether a *sandboxed* build can get the Automation grant without
  a temporary-exception entitlement App Review would question.
- **KOnlineAccounts trajectory** — watch; it is the first plausible
  cross-desktop Linux broker with per-app grants.
- **ManagedApp on macOS 27** — undocumented in detail so far; prototype
  the config delivery before promising it in the enterprise story.
- **AOL/Yahoo in Internet Accounts** — Yahoo's own mail OAuth requires
  manual approval even when credentials could be extracted; irrelevant
  anyway since they can't be.

## Sources

All URLs below were fetched during this research on 2026-10-12.

**Apple documentation**

- [Accounts framework](https://developer.apple.com/documentation/accounts.md) —
  ACAccountStore/ACAccountType availability ending at macOS 12
- [Accounts deprecated symbols](https://developer.apple.com/documentation/accounts/deprecated-symbols.md) —
  account-type identifiers (Facebook, Twitter, LinkedIn, Sina Weibo,
  Tencent Weibo) all deprecated
- [Internet Accounts user guide](https://support.apple.com/en-ca/guide/mac-help/mh43559/mac) —
  provider list and feature toggles on macOS 26/27
- [Mail: add and manage email accounts](https://support.apple.com/guide/mail/add-and-manage-email-accounts-mail35803/mac)
- [EventKit EKSource](https://developer.apple.com/documentation/eventkit/eksource.md) —
  source types incl. `exchange`, `calDAV`
- [EventKit access levels (TN3152)](https://developer.apple.com/documentation/technotes/tn3152-migrating-to-the-latest-calendar-access-levels.md) —
  macOS 14+ full-access vs write-only
- [Contacts: accessing the contact store](https://developer.apple.com/documentation/contacts/accessing-the-contact-store) —
  `NSContactsUsageDescription`, partial access
- [Keychain data protection](https://support.apple.com/guide/security/keychain-data-protection-secb0694df1a/web)
  and [TN3137 on macOS keychains](https://developer.apple.com/documentation/technotes/tn3137-on-mac-keychains.md) —
  file vs data-protection keychain model
- [ASWebAuthenticationSession](https://developer.apple.com/documentation/AuthenticationServices/ASWebAuthenticationSession)
  and [`prefersEphemeralWebBrowserSession`](https://developer.apple.com/documentation/authenticationservices/aswebauthenticationsession/prefersephemeralwebbrowsersession) —
  default-browser + non-ephemeral = shared browser session
- [Platform SSO for macOS deployment](https://support.apple.com/guide/deployment/platform-sso-for-macos-dep7bbb05313/web) —
  macOS 13+ requirement, SSO extension mechanics
- [Apple Mail MDM payload](https://support.apple.com/guide/deployment/mail-payload-settings-dep9c14bfc5/web) —
  `com.apple.mail.managed` configures Apple Mail only
- [App Sandbox entitlements](https://developer.apple.com/library/archive/documentation/Miscellaneous/Reference/EntitlementKeyReference/Chapters/EnablingAppSandbox.html)
  — `security.automation.apple-events` / temporary exceptions
- [Apple Mail scripting QA1802](https://developer.apple.com/library/archive/qa/qa1802/_index.html)

**Microsoft documentation**

- [Microsoft Enterprise SSO plug-in for Apple devices](https://learn.microsoft.com/en-us/entra/identity-platform/apple-sso-plugin) —
  Company Portal delivery, MDM requirement, MSAL ≥1.1.0 integration
- [Platform SSO on macOS](https://learn.microsoft.com/en-us/entra/identity/devices/macos-psso)

**GNOME / Linux**

- [GOA services documentation](https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/services.html) —
  generated docs, version 3.58.1 (as of 2026-10-12)
- [Account D-Bus interface](https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/dbus-org.gnome.OnlineAccounts.Account.html) —
  `EnsureCredentials`, `Remove`, `AttentionNeeded`, feature-disabled flags
- [OAuth2Based interface](https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/dbus-org.gnome.OnlineAccounts.OAuth2Based.html) —
  `GetAccessToken`, client-id properties
- [Mail interface](https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/dbus-org.gnome.OnlineAccounts.Mail.html) —
  IMAP/SMTP properties, `SmtpUseAuth`, `SmtpAuthXoauth2`
- [Ubuntu 26.04 desktop manifest](http://ca.releases.ubuntu.com/26.04/ubuntu-26.04-desktop-amd64.manifest) —
  `gnome-online-accounts` 3.58 on GNOME 50
- [Ubuntu 26.04 release notes](https://documentation.ubuntu.com/release-notes/26.04/changes-since-previous-interim/)
- GOA provider sources (GNOME client IDs and scopes):
  `src/goabackend/goagoogleprovider.c`, `goamsgraphprovider.c`,
  `goaexchangeprovider.c`, `goaimapsmtpprovider.c` (via
  github.com/GNOME/gnome-online-accounts mirror)
- [Flatpak portals](https://flatpak.github.io/xdg-desktop-portal/) —
  Account portal is name/avatar only, no online-accounts portal
- [Flatpak sandbox permissions](https://docs.flatpak.org/en/latest/sandbox-permissions.html) —
  `--talk-name` mechanism
- [Geary Flatpak manifest](https://github.com/flathub/org.gnome.Geary/blob/master/org.gnome.Geary.yaml) —
  `--talk-name=org.gnome.OnlineAccounts`
- [Evolution Flatpak manifest](https://github.com/flathub/org.gnome.Evolution/blob/master/org.gnome.Evolution.json) —
  `--talk-name=org.gnome.OnlineAccounts`, verified 2026-10-12
- [snapd accounts-service interface](https://github.com/canonical/snapd/blob/master/interfaces/builtin/accounts_service.go) —
  deny-auto-connection
- [elementary online-accounts portal request](https://github.com/elementary/portals/issues/19)
- [gnome-online-accounts-gtk (XApp)](https://github.com/linuxmint/gnome-online-accounts-gtk) —
  GOA frontend for Cinnamon/MATE/Xfce
- [elementary switchboard onlineaccounts plug](https://github.com/elementary/switchboard-plug-onlineaccounts) —
  EDS-backed, not GOA

**KDE**

- [KOnlineAccounts](https://github.com/KDE/konlineaccounts) — README
  justifying KAccounts replacement; KDE PIM absent; prototype
- [kaccounts-integration on Launchpad](https://blueprints.launchpad.net/ubuntu/+source/kaccounts-integration/4:26.08.0-0ubuntu1)
- [KDE invent network group](https://invent.kde.org/groups/network) —
  kaccounts-integration/providers recent tags are release-bot/l10n churn
- [KDE Discuss: why no shared account config in KDE PIM](https://discuss.kde.org/t/why-does-every-kde-pim-program-have-its-own-account-configuration-ui-why-no-kcm-for-that/6803)
- [KDE bug 413472 — KMail Google verification](https://bugs.kde.org/show_bug.cgi?id=413472)

**Mail client precedents and policies**

- [Mimestream security & privacy](https://mimestream.com/trust/security-and-privacy) —
  own OAuth, restricted-scope verification, ASWebAuthenticationSession
- [Mimestream account setup](https://mimestream.com/help/user-guide/adding-accounts)
- [Microsoft Q&A: Outlook vs Internet Accounts](https://learn.microsoft.com/en-us/answers/questions/4634602/internet-accounts-settings-in-mac-when-using-outlo)
- [Google restricted-scope verification](https://developers.google.com/identity/protocols/oauth2/production-readiness/restricted-scope-verification)
- [Google verification FAQ](https://support.google.com/cloud/answer/13463817?hl=en)
- [Firefox 157 release notes](https://www.firefox.com/en-US/firefox/157.0/releasenotes/)
  and [Mozilla bug 1921535](https://bugzilla.mozilla.org/show_bug.cgi?id=1921535) —
  ASWebAuthenticationSession in Firefox
- Chromium source — `as_web_auth_session` implementation (non-Safari
  default-browser support)

**Project-internal sources** — `research/01-architecture.md`,
`research/06-account-backends.md` (D26, EWS timeline, Microsoft managed
consent, Google CASA), `research/09-linux.md`, `task.md`.

