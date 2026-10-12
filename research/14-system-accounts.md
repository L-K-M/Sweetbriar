# 14 — System-level accounts (macOS, GNOME, KDE)

Synthesis of [raw/system-accounts/codex.md](raw/system-accounts/codex.md)
and [raw/system-accounts/devin.md](raw/system-accounts/devin.md) (Devin's
run hit its 60-minute limit; its report is a near-complete draft), plus
facts Claude checked by hand against Apple's documentation metadata, the
GNOME Online Accounts and KDE source trees, and Mozilla's bug tracker.
Checked 2026-10-12.

The owner asks: "macOS and I think also Linux allow logging into accounts
on system level. Can we use these accounts?"

## Answer

| Platform | Can Sweetbriar use the system's mail accounts? | What it can use |
|---|---|---|
| **macOS** (Internet Accounts) | **No.** No public API exposes them | Contacts and calendars synced by those accounts (with permission); browser sign-in reuse; Microsoft's enterprise SSO broker on managed Macs; a settings importer via Apple Mail's scripting dictionary (to be tested) |
| **GNOME** (Ubuntu, Fedora Workstation; Cinnamon too) | **Technically yes**, via a documented D-Bus API that hands out OAuth tokens, passwords and IMAP/SMTP settings | Server settings and identities; IMAP/SMTP password accounts; OAuth tokens. GNOME's policy asks third-party apps to contact its maintainers before integrating at all and to keep their own identity, and **OAuth tokens belong to GNOME's app registration** |
| **KDE Plasma** (KAccounts) | **No** | Nothing for mail: no mail provider, and its Google provider lacks the Gmail scope. Its prototype successor, KOnlineAccounts, has no mail support yet |

**Recommendation:** Sweetbriar keeps its own sign-in everywhere (D26
stands). System accounts are optional *sources*, never the authority for
Sweetbriar's accounts:
- on macOS: contacts and enterprise SSO;
- on GNOME: settings import and password accounts after contacting the
  GOA maintainers (GNOME's published request for any integration); OAuth
  token reuse only with a documented agreement (Sweetbriar's own, stricter
  release gate).

The Rust core gets a `CredentialSource` abstraction, so an account's
identity and history never depend on where its tokens come from.

## macOS

### Internet Accounts has no public mail API

- **The Accounts framework is a dead end.** `ACAccountStore` was
  deprecated in macOS 12 ("Use appropriate non-Apple SDK"). The only
  account types it ever exposed were Twitter, Facebook, Sina Weibo, Tencent
  Weibo and LinkedIn, all deprecated in macOS 10.13 (Apple documentation
  metadata, checked by Claude). Google, Exchange, iCloud and IMAP accounts
  were never public.
- **The private store is out of bounds.**
  - `accountsd` keeps account metadata in `~/Library/Accounts`
    (`Accounts4.sqlite`), and Mail's data sits in `~/Library/Mail`. Both are
    protected by mandatory access controls, even outside the sandbox.
  - Parsing them is unsupported; the schemas and protections change between
    releases.
  - Private APIs break App Store guideline 2.5.1. Notarization doesn't make
    them supported.
- **Keychain:**
  - A legacy-keychain password can sometimes be released to another app
    after a user prompt.
  - Data-protection-keychain items (where OAuth material lives) need
    Apple's access groups, which no third-party signature has.
  - Even a readable token would belong to Apple's client registration, not
    Sweetbriar's. Not a viable path.
- **No surveyed third-party Mac mail client reuses Internet Accounts
  credentials.** Mimestream, Spark, Canary, Airmail, Outlook, eM Client
  and Mailspring all run their own provider sign-in.

### What macOS does offer

1. **Contacts and calendars from those accounts.**
   - With permission, `CNContactStore` returns contacts synced by
     CardDAV/Exchange accounts, giving recipient completion for D22 without
     Sweetbriar implementing those logins.
   - `EKEventStore` does the same for calendars: full access on macOS 14+;
     write-only access can't read events.
   - These expose data, never credentials or server settings. Native
     macOS has no "limited contacts" mode: `CNAuthorizationStatus.limited`
     exists on iOS, iPadOS, Mac Catalyst, visionOS and watchOS only (Apple
     metadata, checked by Claude). Devin's draft assumed otherwise.
   - Container ids are external references, not Sweetbriar account ids.
2. **Browser sign-in reuse.**
   - `ASWebAuthenticationSession` runs Sweetbriar's own OAuth in the
     default browser if that browser supports the API, otherwise in Safari.
     Safari, Chrome, Edge and Brave support it.
   - Firefox implemented it in bug 1921535 (fixed 2026-09-02), which
     shipped in Firefox 157 on 2026-09-29 (Bugzilla status flags; Mozilla
     release calendar). eduVPN testers report it working only in 158
     betas (158 ships 2026-10-13), so treat Firefox support as available
     but unproven until tested.
   - If the user is already signed in to Google or Microsoft there, adding
     an account can take one click. Consent, account choice and MFA can
     still appear.
3. **Microsoft enterprise SSO.**
   - On an MDM-managed Mac with Company Portal and an MDM-pushed
     configuration, Microsoft's Enterprise SSO plug-in serves any app built
     on MSAL **automatically**; only non-MSAL apps need admin allow-listing.
     Platform SSO adds Mac login integration.
   - Sweetbriar's Swift shell would use MSAL with **Sweetbriar's own client
     id**: try silent acquisition, show UI when needed, and hand the Rust
     core an access token, its expiry and an account handle.
   - MSAL owns the refresh material. The broker signs the user in, but
     **doesn't grant consent**: the tenant must still approve Sweetbriar
     (06-account-backends).
4. **Managed configuration (enterprise).**
   - MDM can push managed preferences into Sweetbriar's own preferences
     domain (`com.apple.ManagedClient.preferences`): tenant, address hints,
     server presets, locked policies.
   - macOS 27 adds Managed App Configuration (`ManagedApp`, WWDC26), which
     can also provision secrets intentionally.
   - Apple's Mail/Exchange account payloads configure Apple's apps only.
   - Sweetbriar defines a small, versioned preset schema.
5. **Import settings from Apple Mail (to be qualified).** Mail's scripting
   dictionary exposes accounts and, per Devin, server settings, without
   secrets.
   - A one-time "Import settings from Apple Mail" wizard could read them
     with the user's Automation permission.
   - It would preview them, then have the user authorize Sweetbriar
     separately.
   - Which properties macOS 26/27 actually expose is unverified until
     tested.

**Not useful:** MailKit (extends Apple Mail itself), Sign in with Apple
(app identity, not mailbox access), credential exchange (macOS 26;
migrates passwords between password managers), and App Intents.

## GNOME Online Accounts (GOA)

### The API

GOA 3.58.1 (released 2026-04-12; GNOME 50) is a per-user D-Bus service,
`org.gnome.OnlineAccounts`, with account objects under
`/org/gnome/OnlineAccounts`. Claude checked the interface XML and provider
sources:

| Interface | What it gives a consumer |
|---|---|
| `Account` | id, provider, identity, `AttentionNeeded`, per-service disabled flags (`MailDisabled` …); `EnsureCredentials()`, `Remove()` |
| `OAuth2Based` | `GetAccessToken()` → token + seconds left; `ClientId`/`ClientSecret` properties. **No scope argument** |
| `PasswordBased` | `GetPassword(id)` (e.g. `imap-password`, `smtp-password`) |
| `Mail` | `EmailAddress`, `Name`, `ImapHost`, `ImapUserName`, `ImapUseSsl/Tls`, `SmtpHost`, `SmtpAuthXoauth2` … (ports embedded in host strings) |
| `Exchange`, `Calendar`, `Contacts` | host or service URI only |

**Providers:**
- Google;
- Microsoft 365 (`ms_graph`);
- Exchange (EWS, which Exchange Online is retiring);
- IMAP/SMTP;
- Kerberos, ownCloud/Nextcloud, WebDAV;
- Fedora (off by default).

The Windows Live provider was removed in 3.55, although some GOA docs
still list it.

**Scopes:**
- **Google** requests `https://mail.google.com/` (full Gmail
  IMAP/SMTP) and exports `imap.gmail.com`/`smtp.gmail.com` with XOAUTH2.
- **Microsoft 365** requests Graph `mail.readwrite`, `mail.send`,
  shared-mailbox, calendar, contacts and files scopes. Its tokens are Graph
  tokens, not IMAP tokens.

**Consumers:**
- **Geary** calls `EnsureCredentials` then `GetAccessToken` or
  `GetPassword`, and maps GOA's settings to its own.
- **Evolution** maps GOA accounts through Evolution Data Server.
- Both ship on Flathub with `--talk-name=org.gnome.OnlineAccounts`.

### Whose app is it? The catch

- **The tokens belong to GNOME's registrations.** By default, GOA's tokens
  are issued to GNOME's Google client
  (`44438659992-….apps.googleusercontent.com`) and GNOME's Microsoft client
  (`b155a604-…`). Organisations can supply their own Microsoft client and
  tenant id when adding an account. GNOME's default Microsoft 365 client
  was itself unverified until a fix (GOA NEWS, issue #335), so users saw
  an unverified-app warning. Google's client id is a build-time option,
  not something an app can choose.
- **GNOME's policy:**
  - Third-party apps "should contact the GNOME Online Accounts maintainers
    as well as the GNOME design and release team first".
  - Applications with their own brands "ought to be exposed to services and
    users using those identities, and should not be falsely advertised as
    'GNOME'" (GOA goals page, checked by Claude).
  - GOA exists for core GNOME apps and generic functionality.
- **Provider terms:** Google requires accurate app identity and its own
  verification for restricted scopes. Microsoft treats app credentials as
  non-transferable. Using GOA's tokens doesn't hand Sweetbriar GNOME's
  verification. Under Microsoft's managed consent policy, GNOME's client
  isn't on the exception list either, so tenants must approve it like any
  other.
- **Shared clients can lose access.** In June 2024 Google blocked KDE's
  shared client for Drive: "Google blocked us from using this back in June
  because we weren't able justify our API usage" (KDE bug 480779). GOA's
  Google client could be cut off the same way, taking every consumer with
  it.

**So, on GNOME:**
- **GNOME's request covers any integration.** Contact the GOA maintainers
  and the GNOME design and release teams before shipping one. The policy
  makes no exception for settings import or password accounts.
- **After that contact:**
  - read GOA's account list, identities and server settings as an import
    and autofill source;
  - consume GOA **IMAP/SMTP password** accounts;
  - authorize Google and Microsoft accounts through Sweetbriar's own
    registrations.
- **OAuth token reuse needs a documented agreement.** This is
  Sweetbriar's own, stricter release gate, not a technical restriction of
  the API (Geary already calls `GetAccessToken`).
  - The agreement means written confirmation from the GOA maintainers,
    covering consent-screen attribution, permitted consumers, scopes and
    policy responsibility. Sweetbriar's identity is shown to the user at
    first use.
  - GNOME's agreement doesn't transfer Google's verification or override
    tenant consent.
  - Then `GetAccessToken` becomes a Linux convenience path, with our own
    sign-in as the fallback.
  - Sweetbriar never copies GNOME's client id into its own authorization
    flow.
- An organisation can also register its own client, or Sweetbriar's, in
  GOA's Microsoft 365 setup. That keeps the grant with the right app, but
  it creates a new GOA account rather than reusing an existing one.

### Packaging

| Package | What it needs |
|---|---|
| **Flatpak** | `--talk-name=org.gnome.OnlineAccounts`: a static bus permission, reviewed case by case on Flathub, with Geary and Evolution as precedent. No portal covers online accounts: `org.freedesktop.portal.Account` only returns the local user's name and avatar |
| **Snap** | The `accounts-service` interface, which is not auto-connected (not the similarly named `online-accounts-service`, an Ubuntu stack) |
| **.deb** | The session bus directly |

### Lifecycle rules

- **Removal never deletes local data.**
  - GOA objects disappear when the daemon restarts, so GNOME's own guidance
    forbids destroying data on removal.
  - A removed or attention-needed system account pauses network activity
    only. Offline reading, search and compose keep working, and a reconnect
    action appears.
  - Sends with unknown outcome stay unknown.
- **Mail must be enabled, and stay enabled.**
  - The user can switch Mail off for an account in Online Accounts. GOA
    then removes the `Mail` interface asynchronously, while the account
    and its `OAuth2Based` interface can remain.
  - Sweetbriar checks that Mail is enabled before binding and before any
    network effect. It watches `MailDisabled` and interface changes.
  - On disablement it invalidates outstanding credential leases and
    pauses GOA-backed mail operations. Local data stays, and the user can
    sign in independently instead.
- **Token expiry may be unknown.** `GetAccessToken` returns 0 seconds
  when the expiry is unknown. That means neither "expired now" nor "valid
  forever". Sweetbriar reacquires through GOA when needed, with bounded
  retries after authorization failures.
- **Refresh belongs to GOA.** Sweetbriar asks for a new token, calls
  `EnsureCredentials` on authorization failure, watches `AttentionNeeded`,
  and keeps no copied refresh token.
- **Temporary and locked accounts are labelled.** GOA's `IsTemporary`
  accounts are "silently disregarded after logout", so Sweetbriar skips
  them for binding, or labels them visibly. `IsLocked` accounts were
  added by an administrator and are treated as an enterprise-managed
  source.
- **Imported settings get Sweetbriar's TLS policy.** GOA's
  `*AcceptSslErrors` flags are never adopted silently.

## KDE and other desktops

- **KAccounts** (Plasma's Online Accounts; kaccounts-integration and
  kaccounts-providers 26.08.2, Sept/Oct 2026, actively released):
  - It sits on Accounts-SSO (libaccounts and signond). Signond's raw D-Bus
    API is documented as unstable; clients are meant to use
    `libsignon-glib` or `libsignon-qt`.
  - Providers are Google, Nextcloud and ownCloud only.
  - The Google provider asks for userinfo, calendar, tasks, contacts and
    YouTube upload. It has **no Gmail scope**, and its calendar and contacts
    service files are disabled.
  - There is no Microsoft provider, no IMAP/SMTP provider and no mail
    service.
  - KMail sets up mail itself through Akonadi resources; kdepim-runtime
    doesn't depend on KAccounts.
- **KOnlineAccounts** is KDE's successor (Nicolas Fella, 2025-11-15), with
  a daemon and per-app account access. Its README says "prototype stage,
  not ready for production". Providers are Nextcloud, Mastodon and Google,
  and its consumers include KDE PIM (in master). No mail functionality is
  documented. Revisit when it ships with mail support.
- **Cinnamon** uses GOA (an XApp GTK frontend), so the GOA adapter works
  there. **XFCE and COSMIC** have no documented mail-account broker
  (unverified as a universal negative).
- **Detection:** at runtime, use GOA wherever its service exists. Don't ask
  KDE users to install GOA; the frontend for adding accounts is GNOME
  Settings.

## Architecture: credential sources in the core

Three records stay separate:

1. **Mailbox account:** Sweetbriar UUID, provider, validated principal
   and mailbox, history and settings (D1, D2).
2. **Transport namespace:** remote ids and sync cursors per backend.
3. **Credential binding:**
   - source kind and external handle (GOA `Account:Id`, which never
     changes; the D-Bus object path is only a per-session handle; MSAL
     account id …);
   - authority or tenant, and the effective client id;
   - capabilities and a binding generation.

`CredentialSource` is an async contract on the platform boundary
(`sb-platform`):

- **Request:** account, resource or audience, scopes or protocol, a
  claims challenge, and whether interaction is allowed.
- **Result:** a short-lived credential lease, its expiry (known or
  unknown) and the source generation, or a typed failure:
  - `SourceUnavailable`, `SourceRemoved`;
  - `NeedsUserInteraction`, `ConsentRequired`;
  - `CredentialsLocked`, `CapabilityMissing`;
  - transient network failure.

| Source | Owns refresh | Sweetbriar's job |
|---|---|---|
| Own OAuth (`oauth2` crate, PKCE per RFC 8252/7636) | Sweetbriar | Serialize refresh; store refresh tokens in Keychain or Secret Service; handle revocation |
| GOA | GOA daemon | Ask for tokens; `EnsureCredentials`; follow `AttentionNeeded`; no copied refresh token |
| MSAL (Swift) | MSAL / broker | Silent acquisition; forward claims challenges; request UI when interaction is required |
| App password | User or provisioner | Secure storage and replacement |

**Rules:**
- **Identity:** a GOA object path or MSAL cache key is never an account's
  primary identity.
- **Scope:** GOA has no scope argument, so a request it can't satisfy
  returns `CapabilityMissing` instead of a token for another resource.
- **Stale callbacks:** disconnecting or rebinding bumps the binding
  generation, so a late token callback can't revive a removed account.
- **Shared grants:** disconnecting a GOA account from Sweetbriar never
  revokes the grant for other apps.
- **Changing a credential source** (GOA → own OAuth, browser → MSAL):
  - first confirm, through a trustworthy provider identity response, that
    the new credential reaches the **same principal and mailbox**; aliases,
    display names or decoded tokens don't count;
  - only then swap the binding atomically;
  - the account UUID, namespace and cursors stay.
- **Changing transport** (Gmail IMAP → Gmail API) is a different
  operation. It is reconciled through the occurrence model, never by
  resetting history or merging on Message-ID.

**Crates:** `zbus` for D-Bus, `oauth2` for owned flows, `secrecy` for
secrets. Pin the audited versions.

## Onboarding wording

| Context | Label |
|---|---|
| macOS | "Sign in with Google", "Sign in with Microsoft"; the browser may already be signed in, but Sweetbriar gets its own grant |
| Managed Microsoft Mac | "Use your work sign-in" when the broker is detected; missing organisation consent is explained separately |
| macOS contacts | "Use contacts from this Mac" |
| Apple Mail settings (once qualified) | "Import settings from Apple Mail"; preview, then sign in |
| GNOME | "Use an account from Online Accounts": eligible accounts and what will happen (settings import, password reuse, or sign-in) |
| Elsewhere | The normal provider or manual setup |

On macOS, "use an account from System Settings" would promise credential
access Sweetbriar can't have, so that wording is reserved.

## Strongest counter-argument

"This keeps duplicate setup exactly where a native app should remove it.
GOA already holds working Gmail and Microsoft 365 tokens; Geary and
Evolution show the experience." That is persuasive **if** GNOME and the
providers agree to Sweetbriar using the shared clients and the lifecycle
and sandbox tests pass. GOA OAuth would then become the recommended
Linux path, an evidence-backed change to D26 for GNOME only. On macOS,
only a documented Apple mail-account broker or a provider partnership
would change the answer. For KDE, a production KOnlineAccounts with mail
consumers would.

## Conflicts between the reports and with Claude's first answer

| Topic | Claim | Resolution |
|---|---|---|
| GOA as a Linux shortcut | Claude's interim answer to the owner: GOA could let Sweetbriar skip Google's sign-in setup on Linux; Devin: GOA as a "first-class convenience path" | **Corrected.** GNOME's policy asks third-party apps to contact its maintainers before any integration and to keep their own identity, and Google's verification doesn't transfer. Contact first for everything; token reuse also needs a documented agreement |
| Firefox and `ASWebAuthenticationSession` | Earlier forum reports: unsupported; Devin: supported from Firefox 158 (2026-10-13) | Bug 1921535 fixed 2026-09-02 and shipped in Firefox 157 (2026-09-29); working end to end only in 158 betas per eduVPN testers; untested here |
| KDE | Devin: "weakly maintained"; Codex: in transition to KOnlineAccounts | KAccounts still ships releases; the successor is a prototype. For mail, neither helps |

## Open

- **A signed macOS probe:** Contacts permission, browser callbacks
  (including Firefox), and what Mail's scripting dictionary exposes on
  macOS 26 and 27.
- **An MSAL test** in an independently administered tenant: broker
  detection, silent acquisition and the consent gate.
- **A GOA probe** on the cohort's Ubuntu/Flatpak versions: the effective
  client ids, service disablement, daemon restart, locked secrets.
- **A conversation with GOA maintainers,** if Linux GOA token reuse is
  wanted.
- **A source-switch and removal test** proving drafts and history stay
  intact.

## Sources

All checked 2026-10-12. The raw reports carry many more.

- **Apple:**
  - [ACAccountStore availability metadata](https://developer.apple.com/tutorials/data/documentation/accounts/acaccountstore.json) and the [Twitter account-type identifier](https://developer.apple.com/tutorials/data/documentation/accounts/acaccounttypeidentifiertwitter.json) (the other social identifiers have sibling pages);
  - [`CNAuthorizationStatus.limited` availability](https://developer.apple.com/tutorials/data/documentation/contacts/cnauthorizationstatus/limited.json), [TN3152 calendar access levels](https://developer.apple.com/documentation/technotes/tn3152-migrating-to-the-latest-calendar-access-levels);
  - [`ASWebAuthenticationSession`](https://developer.apple.com/documentation/authenticationservices/aswebauthenticationsession), [Mozilla bug 1921535](https://bugzilla.mozilla.org/show_bug.cgi?id=1921535);
  - [managed preferences payload](https://github.com/apple/device-management/blob/release/mdm/profiles/com.apple.ManagedClient.preferences.yaml), [`ManagedAppConfigurationProvider` availability](https://developer.apple.com/tutorials/data/documentation/managedapp/managedappconfigurationprovider.json), [WWDC26 session 206](https://developer.apple.com/videos/play/wwdc2026/206/);
  - [App Review Guidelines](https://developer.apple.com/app-store/review/guidelines/), [TN3137 on Mac keychains](https://developer.apple.com/documentation/technotes/tn3137-on-mac-keychains).
- **Microsoft:**
  - [Enterprise SSO plug-in](https://learn.microsoft.com/en-us/entra/identity-platform/apple-sso-plugin), [Platform SSO](https://learn.microsoft.com/en-us/intune/device-configuration/settings-catalog/configure-platform-sso-macos);
  - [MSAL web views](https://learn.microsoft.com/en-us/entra/msal/objc/customize-webviews), [app consent policies](https://learn.microsoft.com/en-us/entra/identity/enterprise-apps/manage-app-consent-policies).
- **GNOME Online Accounts:**
  - [D-Bus interfaces (3.58.1)](https://raw.githubusercontent.com/GNOME/gnome-online-accounts/3.58.1/data/dbus-interfaces.xml), [build options (3.58.1)](https://raw.githubusercontent.com/GNOME/gnome-online-accounts/3.58.1/meson_options.txt);
  - [Google provider](https://raw.githubusercontent.com/GNOME/gnome-online-accounts/3.58.1/src/goabackend/goagoogleprovider.c), [Microsoft 365 provider](https://raw.githubusercontent.com/GNOME/gnome-online-accounts/3.58.1/src/goabackend/goamsgraphprovider.c), [NEWS](https://raw.githubusercontent.com/GNOME/gnome-online-accounts/master/NEWS);
  - [goals and third-party policy](https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/goals.html), [consumer overview and lifecycle](https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/overview.html);
  - [Geary's GOA mediator](https://raw.githubusercontent.com/GNOME/geary/main/src/client/application/goa-mediator.vala), [Geary Flatpak manifest](https://raw.githubusercontent.com/flathub/org.gnome.Geary/master/org.gnome.Geary.yaml);
  - [Account portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Account.html), [Snap `accounts-service`](https://snapcraft.io/docs/reference/interfaces/accounts-service-interface/).
- **KDE:**
  - [KAccounts Google provider](https://invent.kde.org/network/kaccounts-providers/-/blob/master/providers/google.provider.in), [kaccounts-integration](https://invent.kde.org/network/kaccounts-integration);
  - [signond AuthService](https://accounts-sso.gitlab.io/libsignon-glib/gdbus-com.google.code.AccountsSSO.SingleSignOn.AuthService.html), [KOnlineAccounts README](https://github.com/KDE/konlineaccounts);
  - [KDE bug 480779](https://bugs.kde.org/show_bug.cgi?id=480779), [discuss.kde.org thread](https://discuss.kde.org/t/google-blocks-access-from-dolphin-kio-gdrive/26007), [Nicolas Fella on a new online accounts system](https://nicolasfella.de/posts/a-new-online-accounts-system/).
