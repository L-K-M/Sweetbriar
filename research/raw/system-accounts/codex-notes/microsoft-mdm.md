# Evidence memo — Microsoft SSO / MDM on macOS for Sweetbriar

Date researched: 2026-10-12. Scope: whether Sweetbriar (native macOS app,
Rust core) can ride Microsoft's macOS SSO machinery, and what MDM can push.
Tags: **[S]** source-proven, **[I]** inference from sources, **[U]** unverified.

## Premise check

- macOS 27 "Golden Gate" is real: announced WWDC 2026-06-08, released
  2026-09-14, Apple silicon only; latest stable 27.0.1 (2026-09-28)
  **[S]** ([Wikipedia](https://en.wikipedia.org/wiki/MacOS_Golden_Gate),
  [Apple](https://www.apple.com/os/macos/),
  [release notes](https://developer.apple.com/documentation/macos-release-notes/macos-27-release-notes)).

## Terminology and architecture (source-proven)

Three distinct things get conflated:

1. **Apple Extensible SSO framework.** `com.apple.extensiblesso` MDM payload
   (macOS 10.15+, device and user channel, requires user-approved MDM, cannot
   be manually installed) registers an IdP's app extension for URL prefixes.
   Type `Redirect` covers OAuth/OIDC/SAML; `Credential` covers Kerberos.
   WebKit and CFNetwork provide the interception layer for native and WebKit
   apps; apps can also call the AuthenticationServices API directly. Redirect
   extensions prove IdP approval via Associated Domains
   **[S]** ([payload yaml](https://github.com/apple/device-management/blob/release/mdm/profiles/com.apple.extensiblesso.yaml),
   [Apple Platform Security](https://support.apple.com/guide/security/single-sign-on-security-sec0c87ccc6d/web)).
2. **Microsoft Enterprise SSO plug-in** = Microsoft's IdP-side implementation,
   shipped inside **Intune Company Portal** on macOS (extension ID
   `com.microsoft.CompanyPortalMac.ssoextension`, Team ID `UBF8T346G9`), as a
   "Redirect" extension for `login.microsoftonline.com`, `login.microsoft.com`,
   `sts.windows.net` and sovereign equivalents **[S]**
   ([apple-sso-plugin](https://learn.microsoft.com/en-us/entra/identity-platform/apple-sso-plugin)).
3. **Platform SSO** = macOS 13+ extension of the same plug-in to FileVault /
   Login Window / System Settings sign-in (password sync, Secure Enclave key,
   smart card; macOS 27 adds "Require Touch ID", web-based and QR-code auth,
   Authenticated Guest Mode + FileVault) **[S]**
   ([Apple](https://support.apple.com/guide/deployment/platform-sso-for-macos-dep7bbb05313/web),
   [Intune](https://learn.microsoft.com/en-us/intune/intune-service/configuration/platform-sso-macos)).

Requirements for Sweetbriar's users to get brokered SSO **[S]**
([apple-sso-plugin](https://learn.microsoft.com/en-us/entra/identity-platform/apple-sso-plugin)):

- Device **enrolled in MDM** (any MDM; Intune not required) **and** a pushed
  Extensible-SSO configuration — Apple requires both; there is no user- or
  app-installable path.
- Company Portal installed (manual download, PKG, or script;
  version ≥5.2404.0 for Platform SSO) **[S]**
  ([add-company-portal](https://learn.microsoft.com/en-us/intune/intune-service/apps/apps-company-portal-macos)).
- On Platform-SSO devices the SSO extension "acts as the broker for Microsoft
  Entra ID authentication and Conditional Access"; the WPJ (workplace-join)
  certificate is hardware-bound and only accessible by the plug-in **[S]**
  ([platform-sso-macos](https://learn.microsoft.com/en-us/intune/intune-service/configuration/platform-sso-macos)).

Implication **[I]**: on an unmanaged Mac (no MDM, no Company Portal) none of
this exists; MSAL falls back to web auth, and MSAL's own docs state
Safari-cookie SSO via ASWebAuthenticationSession "is currently not available
on macOS" (MSAL macOS supports WKWebView only) **[S]**
([single-sign-on-macos-ios](https://learn.microsoft.com/en-us/entra/msal/objc/single-sign-on-macos-ios)).

## MSAL brokered token flow on macOS

- MSAL for Apple devices ≥1.1.0 natively supports the plug-in. On devices with
  the SSO plug-in, MSAL **automatically invokes it for all interactive and
  silent token requests, account enumeration, and account removal** — a native
  in-process protocol (custom operations through AuthenticationServices), not
  a URL-scheme hop like iOS's Authenticator flow **[S]**
  ([apple-sso-plugin](https://learn.microsoft.com/en-us/entra/identity-platform/apple-sso-plugin)).
- Redirect URI: must be registered on *your* app registration as
  `msauth.<bundle-id>://auth` and the `msauth.<bundle-id>` scheme declared in
  `CFBundleURLSchemes` **[S]**
  ([redirect-uris-ios](https://learn.microsoft.com/en-us/entra/msal/objc/redirect-uris-ios)).
  Unsigned tools/scripts can instead use `msauth.com.msauth.unsignedapp://auth`
  **[S]** ([MSAL Python broker](https://learn.microsoft.com/en-us/entra/msal/python/advanced/macos-broker),
  [MSAL Node](https://learn.microsoft.com/en-us/entra/msal/javascript/node/brokering-macos)).
- Keychain: shared-SSO across same-team apps uses
  `keychain-access-groups` with group `com.microsoft.identity.universalstorage`
  on macOS **[S]**
  ([single-sign-on-macos-ios](https://learn.microsoft.com/en-us/entra/msal/objc/single-sign-on-macos-ios)).
- Other MSALs (MSAL.NET ≥4.73.1, MSAL Python ≥1.31, MSAL Node via
  `@azure/msal-node-extensions`) can also use the macOS broker with the same
  Company Portal SSO extension **[S]** (Python/.NET/Node broker docs, URLs below).
- Device state can be read via MSAL's `getWPJMetaDataDeviceWithParameters`
  **[S]** ([apple-sso-plugin](https://learn.microsoft.com/en-us/entra/identity-platform/apple-sso-plugin)).

## Direct MSAL vs non-MSAL Apple-networking integration — the fork that matters

- **MSAL path**: no admin allow-list needed; "apps that use a Microsoft
  Authentication Library participate in SSO by default" **[S]**
  ([apple-sso-plugin](https://learn.microsoft.com/en-us/entra/identity-platform/apple-sso-plugin)).
- **Non-MSAL path**: an app *not* using MSAL gets SSO only if its requests to
  Entra URLs flow through Apple's networking stack — URLSession, WKWebView,
  ASWebAuthenticationSession — *and* the admin allow-lists it via
  `AppAllowList`, `AppPrefixAllowList` (macOS defaults cover `com.apple.` and
  `com.microsoft.` prefixes only), or `Enable_SSO_On_All_ManagedApps`.
  Explicitly: "It doesn't support applications that ship their own network
  layer implementation" **[S]** (same doc).
- **Consequence for Sweetbriar [I]**: if the Rust core performs Entra HTTP via
  reqwest/hyper, CFNetwork never sees the request, so URL interception cannot
  attach SSO artifacts — the non-MSAL route is dead by construction. Even the
  token-endpoint POST would be invisible. The viable designs are (a) token
  acquisition in Swift via MSAL ObjC (recommended — native UX, no allow-list),
  or (b) performing the Entra leg of OAuth through URLSession /
  ASWebAuthenticationSession while Rust keeps sync traffic (still requires an
  admin allow-listing Sweetbriar's bundle ID).

## Consent and Conditional Access — the broker grants nothing

- Allow-lists control *which apps may use SSO*, never *which scopes an app
  gets*. Every app still authenticates under its own client ID and registered
  redirect URI, so scope consent is evaluated per app registration exactly as
  in the non-brokered case **[S for mechanism; I for the "never grants"
  phrasing]** ([redirect-uris-ios](https://learn.microsoft.com/en-us/entra/msal/objc/redirect-uris-ios),
  [apple-sso-plugin](https://learn.microsoft.com/en-us/entra/identity-platform/apple-sso-plugin)).
  So D26 stands: Sweetbriar's own registration still needs user/admin consent
  for Graph mail scopes (cf. `research/06-account-backends.md`, managed
  consent policy). Brokered SSO only removes *credential* re-entry, not the
  consent gate.
- Conditional Access is *eased*, not bypassed: the plug-in handles MFA,
  sign-in-frequency and token-revocation prompts once per device (its own UI),
  and "if the device is known to Microsoft Entra ID, the SSO plug-in passes
  the device certificate to satisfy the device-based Conditional Access
  check" **[S]** ([apple-sso-plugin](https://learn.microsoft.com/en-us/entra/identity-platform/apple-sso-plugin)).
- Device identity keys moved from Keychain to Secure Enclave for new
  registrations starting Aug 2025; apps/MDM that read registration keys from
  Keychain must move to MSAL. CA error `530003` appears when an app can't see
  the managed-device state — a real-world failure mode for non-MSAL apps on
  newly enrolled devices **[S]** (same doc).

## Refresh ownership across the Rust/Swift boundary

- Platform SSO itself refreshes its SSO tokens proactively when missing,
  expired, or >4h old (JWT signed with the device signing key) **[S]**
  ([Apple security doc](https://support.apple.com/guide/security/single-sign-on-security-sec0c87ccc6d/web)).
- MSAL owns the token cache in the keychain (`universalstorage` group) and the
  plug-in holds the PRT-class credentials; silent `acquireToken` is the
  refresh path **[I, standard MSAL contract consistent with cited docs]**.
- Recommendation **[I]**: the credential/token-source abstraction in the Rust
  core should treat "macOS brokered account" as a *token source*, not a token
  *store*. Swift calls `acquireTokenSilent`/`acquireToken` per need and hands
  Rust short-lived access tokens; Rust never persists refresh material and
  never does its own refresh for brokered accounts. On `interactionRequired`,
  Rust pauses sync and asks Swift to re-prompt (never delete local history —
  consistent with data-safety goals). If brokered acquisition isn't possible,
  the same abstraction wraps Sweetbriar's own OAuth (D26) where Rust can own
  refresh via Keychain storage — keep both behind one trait so Linux gets the
  same shape.

## MDM-managed configuration channels (what IT can push to Sweetbriar)

| Mechanism | macOS availability | Channel | What it delivers | Reaches Sweetbriar? |
|---|---|---|---|---|
| Managed preferences `com.apple.ManagedClient.preferences` | 10.7+ | device + user (not User Enrollment) | Forced/Set-Once keys in any defaults domain via CFPreferences | **[S]** yes — classic MCX; app reads via UserDefaults/CFPreferences ([yaml](https://github.com/apple/device-management/blob/release/mdm/profiles/com.apple.ManagedClient.preferences.yaml)) |
| `com.apple.configuration.app.managed` (DDM) | 26.0+, system+user scope | User/ADE | Declarative app install + management; feeds ManagedApp config+secrets | **[S]** yes on macOS 26+ ([yaml](https://github.com/apple/device-management/blob/release/declarative/declarations/configurations/app.managed.yaml)) |
| ManagedApp framework (app-side API) | **macOS 27 (new)** | — | App reads admin config + secrets (passwords, certs, identities, hardware-bound keys) via `ManagedAppConfigurationProvider` etc. | **[S]** yes — adopt for enterprise deploys; legacy apps get config via `com.apple.asset.data` + `LegacyAppConfigAssetReference` ([WWDC26 updates](https://support.apple.com/guide/deployment/app-management-updates-depd567c9ffa/web), [dev docs](https://developer.apple.com/documentation/managedapp)) |
| `ManagedAppDistribution` framework | macOS 26 | — | Lets an **MDM vendor's own app** vend optional managed installs | **[S]** no — that's for MDM products, not us ([distribute-managed-apps](https://support.apple.com/guide/deployment/distribute-managed-apps-dep575bfed86/web)) |
| DDM account declarations `account.mail`, `account.exchange` (also Google, LDAP, CalDAV/CardDAV, subscribed calendars) | 13.0+, user scope | User/ADE | Configure accounts for **Apple's** Mail/Calendar/Contacts; Exchange picks EAS or EWS, OAuth optional | **[S]** Apple apps only — no public API hands those credentials to third parties ([account.exchange yaml](https://raw.githubusercontent.com/apple/device-management/release/declarative/declarations/configurations/account.exchange.yaml), [mail DDM](https://support.apple.com/en-gb/guide/deployment/dep2dae6c8eb/web)) **[I]** that these land in the same accountd store as Internet Accounts |
| Legacy `com.apple.mail.managed` | 10.7+, user channel | manual/DEVICE/ADE | IMAP/POP account for **Apple Mail** | **[S]** Apple Mail only ([yaml](https://github.com/apple/device-management/blob/release/mdm/profiles/com.apple.mail.managed.yaml)); `com.apple.eas.account` is iOS/visionOS-only — no macOS version ([yaml](https://github.com/apple/device-management/blob/release/mdm/profiles/com.apple.eas.account.yaml)) |

Net: MDM can *configure* Sweetbriar (managed prefs today, ManagedApp on
macOS 27 — incl. secrets) and can *provision Apple apps' accounts*, but there
is still no supported path for MDM to inject a mail account into a
third-party mail app or export its credentials **[I]**.

## Private API / notarization distinctions

- Using the SSO machinery is public API — AuthenticationServices + MSAL —
  fine for App Store and Developer ID **[S/I]**.
- Notarization is an automated malware/code-signing scan, "not App Review";
  private-API use risks App Store rejection, while Developer-ID notarized
  builds mainly risk runtime breakage and TCC denials — neither legitimizes
  reading `~/Library/Accounts` **[S for notarization scope]**
  ([notarizing doc](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution)).
- Writing our *own* redirect SSO extension is technically public API but only
  makes sense for an IdP (Associated Domains must prove the IdP endorses it)
  — not viable/useful for Sweetbriar **[I]**.

## What this means for Sweetbriar

1. Enterprise SSO is real and worth supporting, but opt-in by the tenant's
   MDM — treat brokered MSAL as one token source behind the Rust trait; keep
   own-OAuth (D26) as the universal path. No allow-list needed because we
   ship MSAL.
2. Put Entra auth in Swift (MSAL ObjC), never in Rust's HTTP stack — Rust
   networking is invisible to the SSO interception layer.
3. Managed-device features (Platform SSO, WPJ cert, CA device checks) arrive
   free via the plug-in once IT configures it; document `AppPrefixAllowList`
   needs only for hypothetical non-MSAL flows.
4. For IT-managed rollouts, plan a ManagedApp config spec (macOS 27) with
   managed-prefs fallback (tenant ID, client ID, broker-required flag) —
   never expect MDM to provision the mailbox itself.

## Consulted URLs

- https://learn.microsoft.com/en-us/entra/identity-platform/apple-sso-plugin
- https://learn.microsoft.com/en-us/entra/msal/objc/single-sign-on-macos-ios
- https://learn.microsoft.com/en-us/entra/msal/objc/redirect-uris-ios
- https://learn.microsoft.com/en-us/entra/msal/python/advanced/macos-broker
- https://learn.microsoft.com/en-us/entra/msal/javascript/node/brokering-macos
- https://learn.microsoft.com/en-us/entra/msal/dotnet/acquiring-tokens/desktop-mobile/macos-broker-dotnet-sdk
- https://learn.microsoft.com/en-us/entra/identity/devices/macos-psso
- https://learn.microsoft.com/en-us/intune/intune-service/configuration/platform-sso-macos
- https://learn.microsoft.com/en-us/intune/intune-service/apps/apps-company-portal-macos
- https://learn.microsoft.com/en-us/intune/intune-service/configuration/use-enterprise-sso-plug-in-macos-with-intune
- https://support.apple.com/guide/deployment/platform-sso-for-macos-dep7bbb05313/web
- https://support.apple.com/guide/security/single-sign-on-security-sec0c87ccc6d/web
- https://support.apple.com/guide/deployment/distribute-managed-apps-dep575bfed86/web
- https://support.apple.com/guide/deployment/app-management-updates-depd567c9ffa/web
- https://support.apple.com/guide/deployment/declarative-app-configuration-dep80b8121d3/web
- https://support.apple.com/en-gb/guide/deployment/dep2dae6c8eb/web
- https://github.com/apple/device-management/blob/release/mdm/profiles/com.apple.extensiblesso.yaml
- https://github.com/apple/device-management/blob/release/mdm/profiles/com.apple.ManagedClient.preferences.yaml
- https://github.com/apple/device-management/blob/release/mdm/profiles/com.apple.mail.managed.yaml
- https://github.com/apple/device-management/blob/release/mdm/profiles/com.apple.eas.account.yaml
- https://github.com/apple/device-management/blob/release/declarative/declarations/configurations/app.managed.yaml
- https://raw.githubusercontent.com/apple/device-management/release/declarative/declarations/configurations/account.exchange.yaml
- https://github.com/apple/device-management/blob/release/declarative/declarations/configurations/account.mail.yaml
- https://developer.apple.com/documentation/managedapp
- https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution
- https://en.wikipedia.org/wiki/MacOS_Golden_Gate
- https://www.apple.com/os/macos/
- https://developer.apple.com/documentation/macos-release-notes/macos-27-release-notes
