<!-- provenance: helper=codex model=gpt-6.1-sol started=2026-10-12T04:23:52+00:00 exit=0 brief=research/briefs/ -->

# Sweetbriar: using system-level accounts

Research date: **2026-10-12**. Capability, version and policy findings are as of that date unless explicitly historical or marked **(unverified)**. Recommendations are design judgments. This is documentation/source research, not a runtime qualification of macOS or authenticated provider accounts.

## 1. TL;DR

- **macOS Internet Accounts is not a supported mail-token source for Sweetbriar.** Apple's public Accounts framework is deprecated and its documented account-type identifiers concern former social integrations. Use provider authorization for mail. [Apple Accounts](https://developer.apple.com/documentation/accounts), [account-type identifiers](https://developer.apple.com/documentation/accounts/acaccounttypeidentifiertencentweibo).
- **macOS can contribute contacts and calendars with permission.** These APIs expose data, rather than the underlying account credentials. [Contacts](https://developer.apple.com/documentation/contacts), [EventKit](https://developer.apple.com/documentation/eventkit/accessing-the-event-store).
- **Browser SSO can simplify Sweetbriar's own OAuth.** On macOS, `ASWebAuthenticationSession` uses a capable default browser, falling back to Safari. An existing login helps, but does not remove consent or MFA requirements. [Apple authentication session](https://developer.apple.com/documentation/authenticationservices/aswebauthenticationsession).
- **Settings import is a separate feature.** Apple Mail scripting is an avenue for a consented metadata importer, subject to a macOS 26/27 compatibility probe. It is not a credentials API. [Apple scripting guide](https://developer.apple.com/library/archive/documentation/LanguagesUtilities/Conceptual/MacAutomationScriptingGuide/NavigateaScriptingDictionary.html).
- **Managed Macs offer a real Microsoft SSO broker.** Company Portal's Enterprise SSO plug-in can serve MSAL applications after MDM configuration. Sweetbriar still uses its own Entra registration and consent. [Microsoft plug-in](https://learn.microsoft.com/en-us/entra/identity-platform/apple-sso-plugin).
- **GOA really can expose mail settings, passwords and access tokens on Linux.** Its current verified release is 3.58.1; this is a supported D-Bus consumer interface, with provider-specific limits. [Release index](https://download.gnome.org/sources/gnome-online-accounts/3.58/), [GOA overview](https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/overview.html).
- **GOA's shared OAuth identity is a product-policy issue.** GNOME asks third-party applications to contact its maintainers and preserve their own brands. Keep D26; do not present GOA as a verification or enterprise-consent workaround. [GOA policy](https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/goals.html).
- **KDE is in transition.** Legacy KAccounts has no comparable shipped mail-service contract; KOnlineAccounts is a replacement prototype with KDE PIM integration listed in master. Defer a KDE account adapter. [KOnlineAccounts](https://github.com/KDE/konlineaccounts).
- **System-account removal must disconnect authentication, not erase Sweetbriar's archive or drafts.** This is also explicitly required by GOA's consumer guidance, because daemon restarts temporarily remove account objects. [GOA lifecycle guidance](https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/overview.html).

## 2. Findings

### macOS Internet Accounts

#### What Internet Accounts contains

Apple confirms macOS 27 Golden Gate's release on **2026-09-14** and Apple-silicon support; this report therefore does not substitute older OS assumptions for the assignment's premise. [Apple compatibility record](https://support.apple.com/en-ca/127255).

Internet Accounts is the system configuration surface used by Apple's Mail, Contacts, Calendar and related apps. The provider families include iCloud, Google, Microsoft Exchange, Yahoo and AOL; “Other” covers ordinary mail and standards-based contact/calendar services. Microsoft 365 is a service accessed through the Exchange account setup, rather than a promise of a public credential-sharing API. Apple's current documentation covers IMAP/SMTP, LDAPv3, CalDAV and CardDAV, and Mail also presents POP account settings. [Internet Accounts guide](https://support.apple.com/en-gb/guide/mac-help/mh35565/26/mac/26), [Mail providers](https://support.apple.com/kb/ht5361), [standards-based services, published 2026-09-17](https://support.apple.com/en-lamr/guide/deployment/depbd78ea978/web), [Mail server settings](https://support.apple.com/en-au/guide/mail/cpmlprefacctadv/mac).

The distinction is architectural: an account configuration can enable several Apple services, each of which exposes its own public data API where one exists. It is not a general third-party email-account registry.

Technically, the implementation centers on the system-managed Accounts database and the **`accountsd`** daemon associated with Accounts.framework. First-hand historical diagnostic records show it running as a per-user `launchd` process. Private account metadata has been stored under **`~/Library/Accounts`**, including **`Accounts4.sqlite`**; contemporary third-party tools explicitly parse that file to map Apple Mail account names. That proves metadata extraction exists in practice, not a supported credentials API. The exact macOS 26/27 schema and access behavior remain **(unverified)** here. [Historical process diagnostic](https://discussions.apple.com/thread/6449667), [current reader's implementation documentation](https://github.com/like-a-freedom/rusty_apple_mail_mcp).

#### Public Accounts APIs: the misleading name

**Verified 2026-10-12:** Accounts.framework remains documented but is deprecated. Apple's documentation metadata marks `ACAccountStore`, `ACAccount`, `ACAccountType` and `ACAccountCredential` deprecated from **macOS 12.0**, directing developers to the relevant provider SDK. The public store includes enumeration/access-request methods, but those methods do not establish access to all account types used internally by macOS. [Framework documentation](https://developer.apple.com/documentation/accounts), [ACAccountStore documentation metadata](https://developer.apple.com/tutorials/data/documentation/accounts/acaccountstore.json).

Its documented legacy identifiers are Facebook, LinkedIn, Sina Weibo, Tencent Weibo and Twitter, all deprecated. The existence of an internal Google, Exchange or IMAP account type does not make an invented account-type string a supported API. No public, supported mail-account enumeration/server-settings/token contract for those Internet Accounts was found in the current Apple documentation. This conclusion applies to both sandboxed apps and ordinary Developer ID apps: distribution outside the App Store does not create missing API authority. [Apple's documented type identifiers](https://developer.apple.com/documentation/accounts/acaccounttypeidentifiertencentweibo).

This is a conclusion about the documented public surface, not a claim that a legacy method can never return an object on any system. Sweetbriar should not depend on undocumented results.

#### Useful indirect data access

With authorization, `CNContactStore` gives access to contact records, groups and containers. System-synchronized CardDAV/Exchange contacts can therefore supply recipient completion without Sweetbriar implementing those account logins. `CNContainer` exposes container identity/name/type, not mail-server settings or OAuth secrets. Request contact access with `NSContactsUsageDescription`; include `com.apple.security.personal-information.addressbook` where required by sandbox/resource-access configuration. Contact notes have a separate restricted entitlement and should not be part of the initial adapter. [Contacts framework](https://developer.apple.com/documentation/contacts), [containers](https://developer.apple.com/documentation/contacts/cncontainer), [authorization and notes](https://developer.apple.com/documentation/contacts/accessing-the-contact-store), [Address Book entitlement](https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.security.personal-information.addressbook).

A platform trap: Apple's generic Contacts guide discusses limited access, but the current `CNAuthorizationStatus.limited` availability metadata lists iOS/iPadOS/Mac Catalyst/visionOS/watchOS, **not native macOS**. Do not promise the iOS limited-contact flow for an AppKit client. A user-selected contact picker is worth evaluating separately. [Apple symbol availability, checked 2026-10-12](https://developer.apple.com/tutorials/data/documentation/contacts/cnauthorizationstatus/limited.json).

`EKEventStore` exposes calendar sources, calendars, events and reminders synchronized by the system, subject to account capabilities and permissions. Since macOS 14, `requestFullAccessToEvents` and `requestWriteOnlyAccessToEvents` express different permissions; full access is needed to read existing events, while write-only cannot read even the events the app created. Use `NSCalendarsFullAccessUsageDescription` or `NSCalendarsWriteOnlyAccessUsageDescription`, and the sandbox calendar entitlement. Reminders require their own full-access request and purpose string. These permissions do not reveal CalDAV or Exchange credentials. [EventKit migration technote](https://developer.apple.com/documentation/technotes/tn3152-migrating-to-the-latest-calendar-access-levels), [event-store access](https://developer.apple.com/documentation/eventkit/accessing-the-event-store).

**Recommendation:** treat system contacts as an optional D22 source for completion. Respect permission revocation and account disappearance; system container IDs are external references, not Sweetbriar account IDs. Calendar access should wait for a defined calendar feature.

#### Keychain access and protected files

An absolute “third-party apps cannot read any existing keychain password” would be wrong. macOS has two keychain implementations. A legacy file-based keychain uses per-item access-control lists; a permitted query can result in an authorization prompt, and a user may authorize access to a particular password. Data-protection keychain items instead use entitlement-authorized access groups. A normal Sweetbriar signature does not gain Apple's groups, and Full Disk Access does not change keychain access groups. [Apple TN3137](https://developer.apple.com/documentation/technotes/tn3137-on-mac-keychains), [keychain sharing](https://developer.apple.com/documentation/security/sharing-access-to-keychain-items-among-a-collection-of-apps).

Accordingly, a deliberately consented legacy-password migration may be technically possible for a particular item. Which current Mail/accountsd credentials remain accessible that way on macOS 27 is **(unverified)**. It is unsuitable as the normal account connection flow: storage location, ACL and credential type vary, and possession of an Apple-client OAuth token would not establish authorization for Sweetbriar's client registration. Prefer a fresh OAuth grant or a user-provided app password.

Reading `~/Library/Accounts` or `~/Library/Mail` is a different operation from using public frameworks. Mandatory access controls apply even outside App Sandbox and even to root; Full Disk Access and sandbox permission are distinct gates. Apple says the exact protected-path list is undocumented and changes over time. Therefore this report does not invent a macOS-27 guarantee that one permission unlocks both directories. Direct account-database schemas and token layouts are not documented public interfaces; precise current protections and layouts require a real OS probe **(unverified)**. [Apple platform security](https://support.apple.com/guide/security/controlling-app-access-to-files-secddd1d86a6/web), [Apple DTS discussion of filesystem permissions](https://developer.apple.com/forums/topics/app-and-system-services/app-and-system-services-core-os).

Private-account APIs create compatibility and distribution liabilities. App Store guideline 2.5.1 requires public APIs. Developer ID notarization is an automated malware/code-signing check, not App Review and not a grant to use private account services. A notarized build still encounters runtime access controls. Do not claim private APIs always cause notarization rejection; the supported conclusion is that notarization does not make them supported or reliable. [App Review Guidelines](https://developer.apple.com/app-store/review/guidelines/), [macOS notarization](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution).

#### A narrower settings-import route: Mail scripting

Apple documents that Mail contains scriptable accounts/mailboxes and continues to document Mail automation in the macOS 27 guide. Its scripting dictionary is an `.sdef` in the application bundle. That provides a legitimate avenue to investigate a one-time importer for non-secret account metadata; it is separate from enumerating Internet Accounts through Accounts.framework. Exact readable server/identity properties on macOS 26/27 are **(unverified)** until tested against those shipped dictionaries. [Apple scripting-object guide](https://developer.apple.com/library/archive/documentation/LanguagesUtilities/Conceptual/MacAutomationScriptingGuide/NavigateaScriptingDictionary.html), [Mail automation on macOS 27](https://support.apple.com/en-ie/guide/mail/mlhlp1120/16.0/mac/27).

The importer would need Automation consent and `NSAppleEventsUsageDescription`, the appropriate hardened-runtime entitlement, and sandbox scripting authorization if sandboxed. Apple's example Mail access group is for composing; it does not establish account-settings read permission. An additional temporary exception may need App Store justification. It should present proposed settings for review, then authorize Sweetbriar independently. [Apple Events entitlement](https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.security.automation.apple-events), [sandbox Apple Events rules](https://developer.apple.com/library/archive/documentation/Miscellaneous/Reference/EntitlementKeyReference/Chapters/AppSandboxTemporaryExceptionEntitlements.html).

#### Browser-session reuse

**Current documented behavior, checked 2026-10-12:** `ASWebAuthenticationSession` routes through the default macOS browser if that browser supports authentication sessions; otherwise it uses Safari. Non-Safari defaults therefore do not automatically imply Safari. The browser must advertise and implement Apple's authentication-session capability. [Authentication session](https://developer.apple.com/documentation/authenticationservices/aswebauthenticationsession), [browser implementation contract](https://developer.apple.com/documentation/authenticationservices/supporting-single-sign-on-in-a-web-browser-app).

`prefersEphemeralWebBrowserSession` defaults to false; requesting true avoids normal browser-session data in Safari, while another browser may not honor the request. With the normal session, a prior Google/Microsoft browser login can reduce password entry. Account selection, consent, MFA and enterprise policy may still interrupt, so “one click” is a possibility rather than a product guarantee. A Safari fallback cannot reuse cookies that exist only in an unsupported default browser. [Apple session-privacy property](https://developer.apple.com/documentation/authenticationservices/aswebauthenticationsession/prefersephemeralwebbrowsersession).

#### Other mechanisms

Credential exchange arrived in **macOS 26**: `ASCredentialExportManager`/`ASCredentialImportManager` support user-driven transfers between participating credential-provider apps, with extension capabilities and exchange-format versions declared. This is a password-manager migration facility, not permission to retrieve accountsd's OAuth tokens. Password AutoFill can assist a user-selected login; native credential association must meet AuthenticationServices requirements. [Credential exchange](https://developer.apple.com/documentation/authenticationservices/ascredentialexportmanager), [WWDC25 passkeys session](https://developer.apple.com/videos/play/wwdc2025/279/), [AuthenticationServices](https://developer.apple.com/documentation/authenticationservices).

Third-party password providers can offer AutoFill suggestions and release a password when the user selects it. Apple's saved-password sharing APIs require website/app association and user consent; they are not a general password enumerator. Sweetbriar cannot claim association with a provider's website merely by listing its domain in an entitlement. Prefer AutoFill in the provider's browser authorization flow, or deliberate user entry of an app password. [Credential-provider security](https://support.apple.com/guide/security/credential-provider-extensions-sec6319ac7b9/web), [saved-password sharing requirements](https://support.apple.com/guide/security/app-access-to-saved-passwords-sec8762eb992/web).

MailKit extends Apple Mail's compose/message/security/content-blocking behavior; App Intents exposes app actions to system experiences. Neither documents a general Internet Accounts mail-token API. Sign in with Apple likewise supplies an identity for an app, not IMAP authorization for the user's iCloud mailbox. [MailKit](https://developer.apple.com/documentation/mailkit), [App Intents](https://developer.apple.com/documentation/appintents), [Sign in with Apple](https://developer.apple.com/documentation/signinwithapple/authenticating-users-with-sign-in-with-apple).

#### Enterprise SSO and managed configuration

**Yes, a third-party app can obtain its own Microsoft 365 tokens through Microsoft's supported macOS broker.** This is independent of Internet Accounts. Apple's Extensible Enterprise SSO hosts an identity-provider extension; Microsoft's implementation is included in **Intune Company Portal**. It needs an MDM-enrolled device and an MDM-pushed SSO configuration; installing Company Portal alone is insufficient. Intune is one supported MDM, not the only one. [Microsoft Enterprise SSO requirements, checked 2026-10-12](https://learn.microsoft.com/en-us/entra/identity-platform/apple-sso-plugin).

Platform SSO adds integration with the Mac login and device identity. Microsoft's deployment documentation describes the SSO extension as the authentication/Conditional Access broker and a hardware-bound workplace-join certificate accessible to that plug-in. The ordinary Enterprise SSO extension and Platform SSO are related, but Sweetbriar does not need to require Platform SSO for every user merely to offer OAuth. [Microsoft Platform SSO deployment](https://learn.microsoft.com/en-us/intune/device-configuration/settings-catalog/configure-platform-sso-macos).

For IT's configuration, Microsoft's macOS extension is `com.microsoft.CompanyPortalMac.ssoextension`, team `UBF8T346G9`; Apple uses the `com.apple.extensiblesso` payload and forbids manual installation of that payload. The currently documented Platform SSO minimums are macOS 13 and Company Portal 5.2404.0. [Apple payload schema](https://raw.githubusercontent.com/apple/device-management/release/mdm/profiles/com.apple.extensiblesso.yaml), [Microsoft deployment prerequisites](https://learn.microsoft.com/en-us/intune/device-configuration/settings-catalog/configure-platform-sso-macos), [extension identity](https://learn.microsoft.com/en-us/entra/identity-platform/apple-sso-plugin).

For MSAL apps, the plug-in participates by default; the administrator's non-MSAL application allow-list is not required. Non-MSAL interception depends on Apple's networking/web-view stack and settings such as `AppAllowList`, `AppPrefixAllowList` and `Enable_SSO_On_All_ManagedApps`. A Rust `reqwest` request does not automatically become an Apple SSO-intercepted request. A browser flow can still benefit from browser SSO; do not equate that with direct broker integration. [Microsoft integration and allow-list rules](https://learn.microsoft.com/en-us/entra/identity-platform/apple-sso-plugin).

**Proposed implementation:** Swift creates `MSALPublicClientApplication` with Sweetbriar's own client ID/authority, tries silent acquisition, and presents interactive MSAL authorization when needed. Register `msauth.<bundle-id>://auth` and its URL scheme according to Microsoft's Apple-app rules. Configure MSAL's own authorized keychain cache; the documented macOS cache group is `com.microsoft.identity.universalstorage`, with signing/provisioning requirements. That cache setup does not permit reading Outlook's or Apple's credentials. Swift hands the Rust engine an access token, expiry and account handle. MSAL/broker owns refresh material. [Redirect URI requirements](https://learn.microsoft.com/en-us/entra/msal/objc/redirect-uris-ios), [MSAL cache-sharing setup](https://learn.microsoft.com/en-us/entra/msal/objc/single-sign-on-macos-ios).

The broker authenticates the user and can satisfy device-aware checks; it does not approve Sweetbriar's requested scopes. The Microsoft-managed default consent policy currently excludes Graph mail-reading permissions and Exchange IMAP/POP permissions from ordinary user consent, with an exception for listed clients. The current exception list does not include Sweetbriar or GNOME. IT can approve an application under its tenant policy; existing approval of Apple Internet Accounts or GNOME is not approval of Sweetbriar. [Current consent policy, checked 2026-10-12](https://learn.microsoft.com/en-us/entra/identity/enterprise-apps/manage-app-consent-policies).

A documentation conflict needs recording: Microsoft's older SSO page says macOS supports only `WKWebView`; its browser-customization page explicitly documents `ASWebAuthenticationSession` on macOS 10.15+ and defaults to it there. Apple's API contract and Mimestream's shipping integration support the latter behavior. Verify the selected MSAL build rather than carrying forward that stale limitation. [MSAL browser documentation](https://learn.microsoft.com/en-us/entra/msal/objc/customize-webviews), [conflicting SSO text](https://learn.microsoft.com/en-us/entra/msal/objc/single-sign-on-macos-ios), [Mimestream implementation](https://mimestream.com/help/user-guide/adding-accounts).

**MDM can preconfigure Sweetbriar if Sweetbriar implements a configuration schema.** These channels must be distinguished:

| Mechanism | What it can mean for Sweetbriar |
|---|---|
| Managed preferences in Sweetbriar's defaults domain, using `com.apple.ManagedClient.preferences` | Tenant, email/identity hints, server presets and locked policy keys, read through native preferences APIs. [Apple payload schema](https://github.com/apple/device-management/blob/release/mdm/profiles/com.apple.ManagedClient.preferences.yaml). |
| Declarative managed app configuration and `ManagedApp` | Secure app-specific configuration/secrets for an app adopting the framework. Apple explicitly adds macOS support in **27**; the configuration-provider symbol confirms 27.0. [Deployment guide](https://support.apple.com/guide/deployment/distribute-managed-apps-dep575bfed86/web), [symbol metadata](https://developer.apple.com/tutorials/data/documentation/managedapp/managedappconfigurationprovider.json). |
| DDM mail/Exchange/CalDAV/CardDAV account declarations and legacy `com.apple.mail.managed` | Configure Apple's corresponding account services. They do not define a Sweetbriar account or export its credentials. [Mail declaration](https://github.com/apple/device-management/blob/release/declarative/declarations/configurations/account.mail.yaml), [Mail profile](https://github.com/apple/device-management/blob/release/mdm/profiles/com.apple.mail.managed.yaml). |

Declarative app installation on macOS 26 is distinct from ManagedApp configuration support on 27. Apple's **WWDC26** session explicitly announces the latter, including secure credential provisioning and hardware-bound keys. `ManagedAppDistribution` concerns managed application distribution; it is not an Internet Accounts reader. [WWDC26 device management](https://developer.apple.com/videos/play/wwdc2026/206/), [Apple deployment guide](https://support.apple.com/guide/deployment/distribute-managed-apps-dep575bfed86/web).

**Recommendation:** define a small versioned preset schema and managed-policy overlay. Use managed preferences for compatible non-secret defaults; evaluate ManagedApp for managed deployments on 27. Secrets delivered intentionally to Sweetbriar are legitimate provisioning, not extraction from Apple Mail. OAuth authorization still follows the provider's flow and policy.

### GNOME Online Accounts

#### Version and provider reality

**Verified 2026-10-12:** the release archive identifies **GOA 3.58.1**, published **2026-04-12**; 3.58.0 was published **2026-03-14**. The client API remains **1.0**. Package versions differ by distribution; neither the desktop's major number nor a bundled Flatpak client library establishes the host daemon's version. [Release archive](https://download.gnome.org/sources/gnome-online-accounts/3.58/), [current client documentation](https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/class.Client.html).

The inspected release enables Google, Microsoft 365 (`ms_graph`), Exchange, generic IMAP/SMTP, Nextcloud/ownCloud, WebDAV and Kerberos providers; Fedora's provider is an optional build feature. Google and IMAP/SMTP are useful mail sources; Microsoft 365 signals Graph capability; the Exchange provider is a separate legacy path. Nextcloud/WebDAV supply contacts/calendars/files rather than a generic email mailbox. [3.58.1 build options](https://raw.githubusercontent.com/GNOME/gnome-online-accounts/3.58.1/meson_options.txt), [Graph provider](https://raw.githubusercontent.com/GNOME/gnome-online-accounts/3.58.1/src/goabackend/goamsgraphprovider.c).

Keep transport separate from authentication: EDS maps GOA's `exchange` provider to EWS and `ms_graph` to Microsoft 365/Graph. Microsoft's current timeline starts disabling Exchange Online EWS in **October 2026**, with full disablement in **April 2027**. A system-account adapter therefore does not reopen the project's Graph decision. Graph mail scopes also differ from Exchange IMAP's `https://outlook.office.com/IMAP.AccessAsUser.All`; a Graph token is not a substitute for an IMAP token. [EDS mapping](https://raw.githubusercontent.com/GNOME/evolution-data-server/master/src/modules/gnome-online-accounts/module-gnome-online-accounts.c), [EWS timeline, checked 2026-10-12](https://learn.microsoft.com/en-us/exchange/clients-and-mobile-in-exchange-online/deprecation-of-ews-exchange-online), [Microsoft protocol scopes](https://learn.microsoft.com/en-us/exchange/client-developer/legacy-protocols/how-to-authenticate-an-imap-pop-smtp-application-by-using-oauth).

Two stale-documentation traps: the services table still lists a standalone Microsoft personal provider and Google Files. The release NEWS records removal of the Windows Live option in 3.55.0, and 3.58 disables Google Files by default. These do not remove Google's Gmail scope. Microsoft personal-account usability through the remaining Microsoft 365 provider must be checked against its current registration/account policy **(unverified)**, rather than promising a removed provider. [Services table](https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/services.html), [release history](https://raw.githubusercontent.com/GNOME/gnome-online-accounts/master/NEWS), [current build options](https://raw.githubusercontent.com/GNOME/gnome-online-accounts/3.58.1/meson_options.txt).

#### D-Bus contract and lifecycle

GOA is a per-user session service, **`org.gnome.OnlineAccounts`**, with account objects under `/org/gnome/OnlineAccounts`. Consumers enumerate via the object-manager/client API and inspect each object's interfaces. [GOA client API](https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/class.Client.html), [D-Bus XML](https://raw.githubusercontent.com/GNOME/gnome-online-accounts/3.58.1/data/dbus-interfaces.xml).

| Interface | Relevant contract |
|---|---|
| `Account` | `Id`, provider/identity/display fields, `AttentionNeeded`, service-disabled flags; `EnsureCredentials()` and `Remove()`. |
| `OAuth2Based` | `GetAccessToken()` returns token and remaining seconds; zero means unknown. Exposes client identity; no per-call arbitrary scope argument. |
| `PasswordBased` | `GetPassword(id)`; generic mail uses identifiers such as `imap-password` and `smtp-password`. |
| `Mail` | Email/name, IMAP/SMTP support flags, host/user/TLS/authentication settings. Host strings can include a port; there are no separate generic port properties. |
| `Exchange` | Host and TLS-error configuration; not a universal Microsoft Graph adapter. |
| `Calendar`, `Contacts` | Service URI and TLS-error metadata when supplied; consumers still implement the provider/data protocol. |

The interface details above come from the [3.58.1 introspection XML](https://raw.githubusercontent.com/GNOME/gnome-online-accounts/3.58.1/data/dbus-interfaces.xml); password identifiers are demonstrated by [Geary's mediator](https://raw.githubusercontent.com/GNOME/geary/main/src/client/application/goa-mediator.vala).

GOA owns the OAuth refresh credentials and the renewal operation. The consumer obtains access tokens, handles authorization failures by asking GOA to ensure credentials again, and observes `AttentionNeeded` for recovery. Calls can perform network work and must remain off the UI thread. Interface availability can change when a user disables Mail or an implementation changes. [Attention property](https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/property.Account.attention-needed.html), [OAuth provider refresh implementation](https://raw.githubusercontent.com/GNOME/gnome-online-accounts/3.58.1/src/goabackend/goaoauth2provider.c).

**Actual consumers:** Geary's mediator calls `EnsureCredentials`, then `GetAccessToken` or the mail password method; it also translates GOA mail settings into Geary's transport settings. Evolution's Evolution Data Server module maps system accounts into sources, obtains credentials through GOA, and handles account additions/removals. Source removal is not a suitable model for Sweetbriar's archive retention. GNOME's own guide expressly forbids destroying data merely because account objects disappear: a daemon restart produces that event too. [Geary source](https://raw.githubusercontent.com/GNOME/geary/main/src/client/application/goa-mediator.vala), [EDS source](https://raw.githubusercontent.com/GNOME/evolution-data-server/master/src/modules/gnome-online-accounts/module-gnome-online-accounts.c), [GOA lifecycle rule](https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/overview.html).

#### Whose OAuth application?

The inspected upstream defaults are Google's client **`44438659992-7kgjeitenc16ssihbtdjbgguch7ju55s.apps.googleusercontent.com`** and Microsoft's **`b155a604-3c31-4079-93b0-6bb6aa9d5464`**. These are GOA/GNOME registrations, not Sweetbriar's. Distributions can replace build defaults. [3.58.1 options](https://raw.githubusercontent.com/GNOME/gnome-online-accounts/3.58.1/meson_options.txt), [GNOME provider administration](https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/services.html).

**Yes, GOA Google requests Gmail access today:** its scope string includes **`https://mail.google.com/`**, and it exports `imap.gmail.com`/`smtp.gmail.com` with OAuth-capable settings. That is the full-mail scope used for Gmail IMAP/SMTP. Source proves the requested scope; it does not prove Google's current private verification decision for every downstream consumer. [Google provider](https://raw.githubusercontent.com/GNOME/gnome-online-accounts/3.58.1/src/goabackend/goagoogleprovider.c), [Google IMAP OAuth requirements](https://developers.google.com/workspace/gmail/imap/xoauth2-protocol).

A bearer token returned by GOA can be technically useful to another local app. That is not equivalent to converting the grant into a Sweetbriar-branded OAuth grant. GNOME's policy explicitly asks third-party developers to discuss integration with its maintainers/design/release teams, and says applications with their own identities should retain those identities rather than masquerade as GNOME. [GOA account policy](https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/goals.html).

Google requires appropriate client registration, accurate app identity, minimum scopes and verification for production use of sensitive/restricted scopes. Its User Data Policy also governs disclosure, permitted use and data transfers. Restricted-scope security assessment depends on the application's data flows, not simply on which process refreshed its token. Using GOA is not evidence that Sweetbriar inherits a verification exemption. [OAuth policy, modified 2026-08-05](https://developers.google.com/identity/protocols/oauth2/policies), [User Data Policy](https://developers.google.com/terms/api-services-user-data-policy), [restricted-scope verification](https://developers.google.com/identity/protocols/oauth2/production-readiness/restricted-scope-verification).

Microsoft likewise requires accurate registration and treats application access credentials as non-transferable; the identity-platform terms restrict sharing credentials/application identity. These general clauses do not resolve every approved operating-system-broker arrangement. **Interpretation:** a deliberately authorized GOA consumer integration needs agreement and accurate disclosure; do not copy GNOME's registration into Sweetbriar's own authorization code or use it to sidestep tenant consent. [Microsoft API terms, October 2025](https://learn.microsoft.com/en-us/legal/microsoft-apis/terms-of-use), [identity-platform terms](https://learn.microsoft.com/en-us/legal/microsoft-identity-platform/terms-of-use).

GOA's Graph provider requests broad permissions, including mail read/write/send, shared mail, calendars, contacts and files. A tenant must authorize the effective GOA client under its policy; signing into Microsoft elsewhere does not supply that consent. GNOME's default client is not in Microsoft's current popular-mail-client exception list. [Graph scopes](https://raw.githubusercontent.com/GNOME/gnome-online-accounts/3.58.1/src/goabackend/goamsgraphprovider.c), [current consent policy](https://learn.microsoft.com/en-us/entra/identity/enterprise-apps/manage-app-consent-policies).

**Custom IDs:** Google has a build-time `google_client_id` override; this is not a per-app `GetAccessToken` parameter. Microsoft's setup offers an organization flow with client ID/tenant ID, deriving a redirect such as `goa-oauth2://localhost/<client-id>`. A tenant-owned or Sweetbriar-owned registration can therefore be used in a separately configured GOA account when its redirect/scopes are supported. Changing an existing GOA account's client is not an automatic migration of its grants and can affect other consumers. [GOA build options](https://raw.githubusercontent.com/GNOME/gnome-online-accounts/3.58.1/meson_options.txt), [organization-account implementation](https://raw.githubusercontent.com/GNOME/gnome-online-accounts/3.58.1/src/goabackend/goamsgraphprovider.c).

Google's override applies to the host provider build, not merely a client library bundled inside Sweetbriar's Flatpak. Microsoft's custom client ID also leaves the provider's hardcoded broad scope list unchanged. Neither is a substitute for designing an appropriately scoped Sweetbriar grant.

**Recommendation:** use GOA metadata/password accounts where useful; keep shared Google/Microsoft OAuth optional and gated on a documented supported integration arrangement. D26 remains the production baseline.

#### Flatpak, Snap and native packages

A Flatpak needs **`--talk-name=org.gnome.OnlineAccounts`** to call the host service. Both current Flathub manifests do this: Geary also permits Control Center/EDS/Secret Service interactions; Evolution declares GOA and Secret Service. Both bundle a GOA client build with its backend disabled, rather than installing a second host account daemon. [Geary manifest](https://raw.githubusercontent.com/flathub/org.gnome.Geary/master/org.gnome.Geary.yaml), [Evolution manifest](https://raw.githubusercontent.com/flathub/org.gnome.Evolution/master/org.gnome.Evolution.json).

This is a static bus-service permission, not a per-account consent portal. Once accessible, the GOA interface exposes credential methods; Sweetbriar should still make selection and use explicit. Flatpak recommends narrow permissions and portals where available. Flathub reviews permissions, requests linter exceptions when necessary, and assesses submissions individually; existing mail-client manifests are precedent, not automatic approval for Sweetbriar. [Flatpak permissions](https://docs.flatpak.org/en/latest/sandbox-permissions.html), [Flathub review](https://docs.flathub.org/docs/for-team-members/review), [current requirements](https://docs.flathub.org/docs/for-app-authors/requirements).

There is no equivalent online-mail-account/token portal in the consulted portal registry. **`org.freedesktop.portal.Account.GetUserInformation` returns the local user's ID, name and avatar.** It does not enumerate Google/Microsoft logins. The Email portal delegates composing/sending to an email application; it is not mailbox authentication. [Account portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Account.html), [Email portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Email.html).

For a strictly confined Snap, the relevant GOA permission is **`accounts-service`**, not an invented `gnome-online-accounts` interface. It is **not auto-connected by default**; the snapd policy explicitly permits GOA ObjectManager/property/method calls when connected. The similarly named `online-accounts-service` targets **`com.ubuntu.OnlineAccounts.Manager`**, a different stack. A normal `.deb` application avoids these packaging-specific bus filters, but still needs the session service and its credentials available; other host security policy can constrain it. [Snap accounts-service](https://snapcraft.io/docs/reference/interfaces/accounts-service-interface/), [snapd GOA policy](https://raw.githubusercontent.com/canonical/snapd/master/interfaces/builtin/accounts_service.go), [other online-accounts policy](https://raw.githubusercontent.com/canonical/snapd/master/interfaces/builtin/online_accounts_service.go).

### KDE and other Linux desktops

#### Legacy KAccounts

KAccounts is a KDE library/QML integration over **Accounts-SSO/libaccounts** for account metadata and **signond** for authentication. Provider/service files describe what each account offers; OAuth authentication can use a signon plug-in. This is a different service and storage model from GOA, not another implementation of `org.gnome.OnlineAccounts`. [KAccounts integration](https://github.com/KDE/kaccounts-integration), [Accounts-SSO project](https://accounts-sso.gitlab.io/).

The signond session-bus namespace is `com.google.code.AccountsSSO.SingleSignOn`. `AuthService.getAuthSessionObjectPath` creates an authentication-session object; `AuthSession.process` passes mechanism/session data to the authentication plug-in. **Its own API documentation says the raw D-Bus interfaces are unstable and clients should use `libsignon-glib` or `libsignon-qt`.** A direct Rust `zbus` implementation should not be assumed as safe as GOA's documented consumer contract. [AuthService](https://accounts-sso.gitlab.io/libsignon-glib/gdbus-com.google.code.AccountsSSO.SingleSignOn.AuthService.html), [AuthSession](https://accounts-sso.gitlab.io/libsignon-glib/gdbus-com.google.code.AccountsSSO.SingleSignOn.AuthSession.html).

The inspected KAccounts provider tree includes Google, Nextcloud and ownCloud, with older integrations disabled. Google's provider requests profile/calendar/tasks/contacts/YouTube permissions, **not Gmail's full-mail scope**. The inspected service tree contains no general mail service. A Google entry in Plasma's Online Accounts therefore does not establish that Sweetbriar can authenticate Gmail mail through it. [Provider/service source tree](https://github.com/KDE/kaccounts-providers), [Google scope file](https://github.com/KDE/kaccounts-providers/blob/master/providers/google.provider.in).

KMail's supported setup is its own account wizard and Akonadi resources. There is historical optional KAccounts-to-Akonadi integration: a **2019-09-11** change creates contacts/calendar resources for a KAccounts service and configures them via `org.kde.Akonadi.Accounts`. That is evidence against saying Akonadi has never integrated at all; it is not proof of a currently supported mail-token importer. [KMail setup](https://docs.kde.org/trunk_kf6/en/kmail/kmail2/getting-started.html), [Akonadi integration commit](https://phabricator.kde.org/R165:b2b0dd482b213ba774d582e427c37b400dc4b997).

#### Maintenance and replacement

In his **2025-11-15** design explanation, KDE developer Nicolas Fella describes low development activity in the underlying stack, difficulty landing Qt6 changes and inadequate sandbox/account isolation. This is evidence of maintenance risk, not a declaration that every KAccounts component is abandoned. [Developer's assessment](https://nicolasfella.de/posts/a-new-online-accounts-system/).

**Checked 2026-10-12:** **KOnlineAccounts** is designed to replace KAccounts with a daemon, D-Bus account/functionality interfaces and per-app account access. Its README lists Nextcloud, Mastodon and Google providers, plus Tokodon, Purpose and **KDE PIM in master** as consumers. It still explicitly labels itself a prototype unsuitable for production. Distinguish this current development from the legacy stack and from an established released mail API. [Current KOnlineAccounts README](https://github.com/KDE/konlineaccounts).

**Recommendation:** defer a legacy KAccounts adapter. Re-evaluate KOnlineAccounts when a release, supported mail functionality, authentication policy and Flatpak permissions are documented. Do not make KDE users install GOA merely to use Sweetbriar; offer the app's ordinary account setup everywhere.

#### Cinnamon, XFCE and COSMIC

Cinnamon/Mint has a GTK frontend for **GOA**, so the same GOA consumer adapter can work when that daemon is present. It is not a separate Cinnamon token protocol. [XApp GOA frontend](https://github.com/xapp-project/gnome-online-accounts-gtk).

No documented stable XFCE-specific or COSMIC-specific mail-account/token broker was established in the consulted desktop material. COSMIC has a community accounts library project; its existence is not evidence of a supported OS-wide mail broker. Treat a universal negative here as **(unverified)** and detect capabilities at runtime. A session with GOA available can use the GOA adapter irrespective of its desktop name. [XFCE settings documentation](https://docs.xfce.org/xfce/xfce4-settings/start), [COSMIC settings source](https://github.com/pop-os/cosmic-settings), [community accounts project](https://github.com/cosmic-utils/accounts).

### Mail-client precedents

The following is a documentation/source survey checked **2026-10-12**, not a binary audit of every release. “No documented Internet Accounts credential use” means the cited setup flow authorizes the client itself; it does not rule out archive importing or native Contacts integration.

| Client | Documented account approach and relevant system integration |
|---|---|
| **Thunderbird** | Own OAuth/provider setup. The GOA enhancement remains **NEW**, rather than implemented. [OAuth developer docs](https://source-docs.thunderbird.net/en/latest/backend/oauth.html), [GOA issue](https://bugzilla.mozilla.org/show_bug.cgi?id=847989). |
| **Spark** | In-app address entry and provider authorization; its account/settings sync is Spark's own facility. No documented Internet Accounts credential reuse. [Spark setup](https://support.readdle.com/spark/spark-onboarding/connect-to-your-email-account-in-spark). |
| **Airmail** | Own account/provider setup. “Local Accounts” means imported local archives, including Apple Mailbox/EMLX/EML/mbox, not System Settings accounts. Its cited help page is old; current private implementation is **(unverified)**. [Airmail setup](https://help.airmailapp.com/en-us/article/add-new-account-airmail-for-macos-nnue2q/). |
| **Canary** | Opens Google's authentication page from its Gmail setup; no documented Internet Accounts token reuse. Guide updated **2026-03-26**. [Canary setup](https://canarymail.io/help/adding-a-gmail-account-in-canary-macos). |
| **Mimestream** | Own Google authorization using AuthenticationServices/default browser, with a browser fallback; its refresh token is delivered to the device and stored in its Keychain. [Setup](https://mimestream.com/help/user-guide/adding-accounts), [security model](https://mimestream.com/trust/security-and-privacy). |
| **Outlook for Mac** | Own account authorization. Microsoft documents cloud sync for non-Microsoft accounts, with a direct-sync option for supported providers. This is distinct from Apple Internet Accounts. [Microsoft setup](https://support.microsoft.com/en-us/outlook/add-email-accounts-in-outlook-for-mac-using-microsoft-cloud). |
| **eM Client** | Own automatic setup/provider OAuth; Apple Mail archive import is separate and does not prove credential sharing. [Account setup](https://www.emclient.com/blog/how-do-i-create-or-add-a-new-email-account-in-em-client-606), [import discussion](https://forum.emclient.com/t/importing-from-apple-mail/72164). |
| **Mailspring** | Own accounts configured in Preferences; Mailspring ID is a separate product identity. No documented GOA/Internet Accounts reuse. [Quick-start guide](https://www.getmailspring.com/docs/quick-start-guide). |
| **Geary** | Direct GOA integration for settings and credentials; consumer code calls GOA's renewal/token/password methods. This does not automatically make Geary a Graph client whenever GOA exposes a Microsoft 365 Mail interface. [GOA mediator](https://raw.githubusercontent.com/GNOME/geary/main/src/client/application/goa-mediator.vala). |
| **Evolution** | Automatically picks up GOA accounts, with some settings/removal managed in Online Accounts; EDS maps provider types to its transports, including Microsoft's Graph backend. [User guide](https://help.gnome.org/evolution/intro-first-run.html), [EDS integration](https://raw.githubusercontent.com/GNOME/evolution-data-server/master/src/modules/gnome-online-accounts/module-gnome-online-accounts.c). |
| **KMail** | Own account wizard/Akonadi mail resources; legacy optional contacts/calendar integration and newer KOnlineAccounts/PIM work must be distinguished from mail-token sharing. [KMail guide](https://docs.kde.org/trunk_kf6/en/kmail/kmail2/getting-started.html), [KOnlineAccounts status](https://github.com/KDE/konlineaccounts). |

**Did any surveyed third-party Mac client demonstrate supported reuse of Internet Accounts mail credentials? No.** The evidence is consistent with Apple's public API boundary: former social-account APIs, separate Contacts/EventKit data APIs, and independent provider OAuth for mail. It would be unjustified to claim that no application can ever inspect settings or import Apple Mail data. Mail scripting, explicit file imports and legacy Keychain authorization are distinct possibilities, each with limits. [Apple Accounts boundary](https://developer.apple.com/documentation/accounts), [Mail scripting model](https://developer.apple.com/library/archive/documentation/LanguagesUtilities/Conceptual/MacAutomationScriptingGuide/NavigateaScriptingDictionary.html).

## 3. Options compared

The capability judgments below follow the evidence above; the priority column is this report's recommendation.

| Platform/source | (a) Mail token/credential source | (b) Settings/identity source | (c) SSO broker | (d) Contacts/calendars | Priority |
|---|---|---|---|---|---|
| **[macOS Internet Accounts](https://developer.apple.com/documentation/accounts)** | No supported public mail-token contract found | No general public registry; Mail scripting importer needs qualification | Separate enterprise/browser mechanisms | Yes, indirectly through authorized native frameworks | Contacts first; no private credential adapter |
| **[macOS MSAL + Enterprise SSO](https://learn.microsoft.com/en-us/entra/identity-platform/apple-sso-plugin)** | Yes, tokens for Sweetbriar's app | Broker identity hints, not arbitrary IMAP settings | Yes, on appropriately managed devices | Separate Graph scopes/native APIs | Support for enterprise cohort |
| **[macOS ManagedApp/preferences](https://support.apple.com/guide/deployment/distribute-managed-apps-dep575bfed86/web)** | Intentional provisioned secrets where supported | Yes, with a Sweetbriar schema | Can configure policy; not itself an OAuth broker | App-specific configuration | Small enterprise preset feature |
| **[GNOME/Cinnamon GOA](https://raw.githubusercontent.com/GNOME/gnome-online-accounts/3.58.1/data/dbus-interfaces.xml)** | Yes; shared OAuth use needs policy agreement | Yes, inspect actual service interfaces | Shared credentials; not the same managed-device broker as MSAL | Provides service configuration; use protocol or EDS for data | Optional Linux adapter |
| **[Legacy KDE KAccounts](https://github.com/KDE/kaccounts-integration)** | Generic auth possible; current provider scopes lack a mail contract | Generic metadata; little useful mail configuration | signond authentication stack | Some services/integrations | Defer |
| **[KOnlineAccounts](https://github.com/KDE/konlineaccounts)** | Prototype; release/mail support unqualified | Prototype | Prototype | PIM work exists in master | Monitor |
| **[Own OAuth](https://www.rfc-editor.org/rfc/rfc8252.html)/app password** | Yes, provider-supported access and consent required | Provider discovery/presets or user entry | Browser and supported broker can simplify sign-in | Own provider access or separate native source | Universal baseline |

| Product strategy | Benefit | Main cost | Assessment |
|---|---|---|---|
| System accounts everywhere | A compelling “sign in once” story | No uniform cross-platform mail API; coupled provider identity and daemon lifecycle | Cannot meet the stated platform goals alone |
| Own account setup only | Consistent identity, scope control and support | Repeated account selection/consent; misses native convenience | Sound baseline, incomplete native experience |
| **Own accounts plus optional native sources** | Durable ownership with contacts, enterprise SSO and selected Linux convenience | Extra adapters and lifecycle/policy qualification | **Recommended** |

## 4. Recommendations and trade-offs

### Platform priorities and settled decisions

**macOS:** keep D26. Use Sweetbriar's OAuth registrations, `ASWebAuthenticationSession` for ordinary browser authorization and MSAL's Swift-accessible Apple SDK for Microsoft accounts where broker/device-policy integration matters. Add optional `CNContactStore` recipient completion under D22. Define enterprise presets. Qualify Mail scripting only when pilot users demonstrate value from importing settings; calendar functionality needs its own scope decision.

**Linux:** keep independent OAuth/password setup available on every desktop. Add GOA discovery and explicit metadata import; support GOA password accounts without pretending they are OAuth grants. A shared-GNOME OAuth token source is a deliberate optional extension to D26, requiring the policy arrangement discussed above. A separately configured GOA account using an approved Sweetbriar/tenant client could preserve registration ownership, but is not a transparent reuse of the user's existing GNOME grant. Defer KDE-specific work until its replacement has a production contract.

**D22 is reinforced; Rust account ownership is unchanged.** Native services supply data, presets or credential leases. They do not become the authoritative store for Sweetbriar identities, drafts, settings or history. See the project's [architecture baseline](../../01-architecture.md) and [authorization/backend decisions](../../06-account-backends.md).

### Credential-source architecture

The following is a proposed design, not an existing API. Keep three records separate:

1. **Mailbox account:** Sweetbriar UUID, provider, validated principal/mailbox, durable history and settings.
2. **Transport namespace:** backend-specific remote object identifiers and sync cursors.
3. **Credential binding:** source kind, external handle, authority/tenant, effective client identity, capability information and binding generation.

Expose an asynchronous **`CredentialSource`** contract through the platform/resource boundary. A request specifies account, resource/audience, scopes or protocol, claims challenge when applicable, and whether interactive work is allowed. A result is a short-lived credential lease, expiry/unknown-expiry status and source generation, or a typed failure. Possible sources are `OwnedOAuth`, `Goa`, `Msal`, `AppPassword` and deliberately managed credentials. Account and transport records never use the GOA object path or MSAL cache key as their primary identity.

Backends receive credentials from the engine through the existing adjacent layers; they do not reach directly into Swift, GOA or the store. Swift presents authorization UI and services MSAL requests; Rust retains scheduling, identity validation and durable state. Use the existing bridge's asynchronous request/completion pattern, including cancellation and generation checks.

Useful concrete dependencies are **`zbus`** for Linux D-Bus, **`oauth2`** for owned authorization-code/PKCE and refresh flows, and **`secrecy`** for deliberate secret handling. The consulted documentation identifies `zbus` **5.19.0** and `oauth2` **5.0.0** on **2026-10-12**; pin and audit the selected implementations, rather than adopting floating latest versions. Store owned refresh secrets through the platform's secret store; use MSAL's cache for MSAL accounts. [zbus](https://docs.rs/zbus/latest/zbus/), [oauth2](https://docs.rs/oauth2/latest/oauth2/), [secrecy](https://docs.rs/secrecy/latest/secrecy/).

The authorization-code design should follow native-app OAuth guidance and PKCE; a desktop-bundled client secret does not establish confidentiality. [RFC 8252](https://www.rfc-editor.org/rfc/rfc8252.html), [RFC 7636](https://www.rfc-editor.org/rfc/rfc7636.html).

| Source | Refresh owner | Sweetbriar responsibility |
|---|---|---|
| Owned OAuth | Sweetbriar's source adapter | Serialize refresh, persist replacement credentials safely, respect consent/revocation |
| GOA OAuth | Host GOA daemon/provider | Ask GOA for a new token; track service availability and attention state; retain no copied refresh token |
| MSAL | MSAL/cache/broker | Invoke silent acquisition, forward relevant claims challenges, request UI on interaction-required |
| App password | User/provider, or explicitly managed provisioner | Secure storage, invalidation and replacement; no OAuth refresh assumption |

A GOA source cannot satisfy arbitrary new scopes simply because the generic source trait accepts them: its D-Bus method has no scope argument. Return `CapabilityMissing` or `ConsentRequired` rather than silently substituting a token intended for a different resource.

### Removal, attention and stable identity

**Proposed invariant:** external-account removal, daemon loss, keychain lock or consent failure can change account health; none authorizes deleting local mail, drafts, settings or queued intent. Pause network activity, preserve offline reading/search/compose, and display a reconnect action. Drop cached credential leases and stop retry storms. A send with unknown outcome remains unknown and must not be resent automatically; this preserves the project's existing safety model.

Distinguish `SourceUnavailable`, `SourceRemoved`, `NeedsUserInteraction`, `ConsentRequired`, `CredentialsLocked`, `CapabilityMissing` and transient network failure. Subscribe to service-owner/interface/property changes; re-enumerate after restart. Increment a binding generation on disconnect/rebind, so a late successful token callback cannot restart a removed account. Do not revoke a shared GOA grant globally when the user merely disconnects it from Sweetbriar.

When switching GOA to owned OAuth, or browser OAuth to MSAL, validate that the new credential reaches the **same provider principal and target mailbox** before atomically replacing the binding. Use trustworthy provider identity responses; do not treat an email alias, display name or locally decoded opaque access token as sufficient proof. The local account UUID and existing same-transport namespace/cursors remain stable.

Changing **transport** is a separate operation: Gmail IMAP to Gmail API, for example, changes remote locator semantics. Reconcile through the project's occurrence/provider model rather than resetting history or merging on Message-ID. If identity cannot be established, offer an explicit new account or a reviewed reconnection; never silently attach different mail to an old archive.

### Honest onboarding language

| Context | Honest label/behavior |
|---|---|
| Ordinary macOS | **“Sign in with Google”**, **“Sign in with Microsoft”**; browser login may already exist, but Sweetbriar obtains its own grant |
| Managed Microsoft Mac | **“Use your work sign-in”** when broker capability is detected; explain missing organization consent separately |
| macOS contacts | **“Use contacts from this Mac”**, followed by native permission or a qualified user-selection flow |
| Qualified Mail importer | **“Import settings from Apple Mail”**; preview settings, then authorize independently |
| GOA-capable Linux | **“Use an account from Online Accounts”**; show eligible accounts/services and the selected source's limitations |
| Desktop without a qualified broker | Normal provider/manual setup; the optional system-source choice appears when a supported service exists |

“Use an account from System Settings” on macOS would currently imply credential access Sweetbriar cannot promise. Reserve that wording for a supported provider-specific broker or explicitly described data source.

### Strongest counter-argument

**The strongest argument against this recommendation is that it accepts duplicated setup and consent precisely where a native app should remove friction.** GOA already provides usable Gmail credentials, and Evolution/Geary demonstrate the intended experience. A carefully agreed GOA integration could spare users another grant, avoid duplicate refresh stores and be more native. Concentrating on own registrations may delay Sweetbriar's Linux usefulness, particularly when verification and tenant approval dominate onboarding.

That argument is persuasive **if** GNOME and the providers explicitly support Sweetbriar's use of the shared client, the needed scopes/transport are available, and lifecycle/sandbox tests pass. Then promote GOA OAuth from an optional integration to a recommended Linux setup path. It would be an evidence-backed revision of D26 for that path, not a replacement of Rust-owned accounts.

For macOS, a new documented mail-account broker or provider partnership would change the conclusion. Private database parsing alone would not. For KDE, a production KOnlineAccounts release with demonstrated mail consumers would justify a new adapter. Until then, the cost of repeated authorization is smaller than depending on a credential path Sweetbriar cannot support reliably.

## 5. Risks and open questions

1. **Policy authorization for shared desktop OAuth.** A source-visible Gmail scope is not proof that Google approves every downstream commercial consumer. Obtain a clear GNOME/provider position for Sweetbriar's disclosed behavior; keep D26's own verification/tenant-consent work active. The current policy basis is [GNOME's account policy](https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/goals.html), [Google's OAuth policy](https://developers.google.com/identity/protocols/oauth2/policies) and [Microsoft's registration terms](https://learn.microsoft.com/en-us/legal/microsoft-apis/terms-of-use).
2. **Real macOS 26/27 qualification remains outstanding.** This research environment is Linux. Test signed sandboxed and Developer ID builds for Contacts, browser callbacks, Keychain/resource access and Mail scripting. The exact Accounts database schema, current Mail settings exposed to scripting and legacy-password accessibility are **(unverified)**; no runtime experiment here establishes them.
3. **Browser behavior is capability-dependent.** Test Safari, Chrome, Firefox and the cohort's other default browsers for existing-session reuse, profiles, ephemeral requests, cancellation and callback handling. A web login can require MFA/consent even with valid cookies. [Apple browser contract](https://developer.apple.com/documentation/authenticationservices/supporting-single-sign-on-in-a-web-browser-app).
4. **Microsoft enterprise matrix.** Qualify Company Portal/MDM configurations, Platform SSO, device compliance, admin consent and interaction-required behavior, including silent acquisition when only a future background agent runs. Test the actual MSAL release; public Microsoft pages currently conflict about older macOS web-view behavior. [Enterprise plug-in](https://learn.microsoft.com/en-us/entra/identity-platform/apple-sso-plugin), [MSAL web views](https://learn.microsoft.com/en-us/entra/msal/objc/customize-webviews).
5. **GOA distribution/provider drift.** Probe the host service, interfaces and effective client ID rather than assume the docs' provider table is current. Test service disablement, restart, account re-creation, locked secrets, unknown expiry and unsupported resource/scopes. The inspected release disables Google Files and removes the old Windows Live option while some docs retain them. [Release history](https://raw.githubusercontent.com/GNOME/gnome-online-accounts/master/NEWS).
6. **Imported TLS and policy choices.** GOA exposes flags permitting TLS certificate errors. Recommendation: do not silently adopt them; surface an explicit provider/certificate decision under Sweetbriar's security policy. Managed preferences/configuration should be validated and separated from user-owned settings. [GOA interface flags](https://raw.githubusercontent.com/GNOME/gnome-online-accounts/3.58.1/data/dbus-interfaces.xml).
7. **Retention versus enterprise management.** Sweetbriar must define how explicit organizational retention/offboarding requirements relate to its archive promise. Removing a source is not an implicit purge command. A managed distribution may intentionally remove app files/configuration; qualify backup/recovery behavior separately. [Apple managed-app deployment](https://support.apple.com/guide/deployment/distribute-managed-apps-dep575bfed86/web).
8. **Scope of contacts/calendars.** D22 justifies optional recipient completion; it does not by itself authorize building a calendar client, copying all contacts into persistent storage, or uploading them. Define the data contract and revocation behavior before expanding that integration.

Suggested first evidence gates are a signed macOS authorization/contacts probe, an MSAL test in an independently administered tenant, a GOA adapter probe against the cohort's Ubuntu/Flatpak versions, and a source-switch/removal test demonstrating that drafts and history remain intact. These are bounded validation work, not a reason to replace the project's core architecture.

## 6. Sources

Consulted **2026-10-12**. This index lists the sources cited above; API pages, source files, release records and vendor documentation were fetched or consulted directly. Apple documentation metadata was used where the rendered page hid availability information. Source trees were read over HTTPS; no git checkout or software installation was used. Historical documents and untested runtime behavior are identified in the findings.

**Apple APIs, security and deployment**

- [Apple Accounts](https://developer.apple.com/documentation/accounts)
- [account-type identifiers](https://developer.apple.com/documentation/accounts/acaccounttypeidentifiertencentweibo)
- [Contacts](https://developer.apple.com/documentation/contacts)
- [EventKit](https://developer.apple.com/documentation/eventkit/accessing-the-event-store)
- [Apple authentication session](https://developer.apple.com/documentation/authenticationservices/aswebauthenticationsession)
- [Apple scripting guide](https://developer.apple.com/library/archive/documentation/LanguagesUtilities/Conceptual/MacAutomationScriptingGuide/NavigateaScriptingDictionary.html)
- [Apple compatibility record](https://support.apple.com/en-ca/127255)
- [Internet Accounts guide](https://support.apple.com/en-gb/guide/mac-help/mh35565/26/mac/26)
- [Mail providers](https://support.apple.com/kb/ht5361)
- [standards-based services, published 2026-09-17](https://support.apple.com/en-lamr/guide/deployment/depbd78ea978/web)
- [Mail server settings](https://support.apple.com/en-au/guide/mail/cpmlprefacctadv/mac)
- [Historical process diagnostic](https://discussions.apple.com/thread/6449667)
- [ACAccountStore documentation metadata](https://developer.apple.com/tutorials/data/documentation/accounts/acaccountstore.json)
- [containers](https://developer.apple.com/documentation/contacts/cncontainer)
- [authorization and notes](https://developer.apple.com/documentation/contacts/accessing-the-contact-store)
- [Address Book entitlement](https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.security.personal-information.addressbook)
- [Apple symbol availability, checked 2026-10-12](https://developer.apple.com/tutorials/data/documentation/contacts/cnauthorizationstatus/limited.json)
- [EventKit migration technote](https://developer.apple.com/documentation/technotes/tn3152-migrating-to-the-latest-calendar-access-levels)
- [Apple TN3137](https://developer.apple.com/documentation/technotes/tn3137-on-mac-keychains)
- [keychain sharing](https://developer.apple.com/documentation/security/sharing-access-to-keychain-items-among-a-collection-of-apps)
- [Apple platform security](https://support.apple.com/guide/security/controlling-app-access-to-files-secddd1d86a6/web)
- [Apple DTS discussion of filesystem permissions](https://developer.apple.com/forums/topics/app-and-system-services/app-and-system-services-core-os)
- [App Review Guidelines](https://developer.apple.com/app-store/review/guidelines/)
- [macOS notarization](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution)
- [Mail automation on macOS 27](https://support.apple.com/en-ie/guide/mail/mlhlp1120/16.0/mac/27)
- [Apple Events entitlement](https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.security.automation.apple-events)
- [sandbox Apple Events rules](https://developer.apple.com/library/archive/documentation/Miscellaneous/Reference/EntitlementKeyReference/Chapters/AppSandboxTemporaryExceptionEntitlements.html)
- [browser implementation contract](https://developer.apple.com/documentation/authenticationservices/supporting-single-sign-on-in-a-web-browser-app)
- [Apple session-privacy property](https://developer.apple.com/documentation/authenticationservices/aswebauthenticationsession/prefersephemeralwebbrowsersession)
- [Credential exchange](https://developer.apple.com/documentation/authenticationservices/ascredentialexportmanager)
- [WWDC25 passkeys session](https://developer.apple.com/videos/play/wwdc2025/279/)
- [AuthenticationServices](https://developer.apple.com/documentation/authenticationservices)
- [Credential-provider security](https://support.apple.com/guide/security/credential-provider-extensions-sec6319ac7b9/web)
- [saved-password sharing requirements](https://support.apple.com/guide/security/app-access-to-saved-passwords-sec8762eb992/web)
- [MailKit](https://developer.apple.com/documentation/mailkit)
- [App Intents](https://developer.apple.com/documentation/appintents)
- [Sign in with Apple](https://developer.apple.com/documentation/signinwithapple/authenticating-users-with-sign-in-with-apple)
- [Apple payload schema](https://raw.githubusercontent.com/apple/device-management/release/mdm/profiles/com.apple.extensiblesso.yaml)
- [Apple payload schema](https://github.com/apple/device-management/blob/release/mdm/profiles/com.apple.ManagedClient.preferences.yaml)
- [Deployment guide](https://support.apple.com/guide/deployment/distribute-managed-apps-dep575bfed86/web)
- [symbol metadata](https://developer.apple.com/tutorials/data/documentation/managedapp/managedappconfigurationprovider.json)
- [Mail declaration](https://github.com/apple/device-management/blob/release/declarative/declarations/configurations/account.mail.yaml)
- [Mail profile](https://github.com/apple/device-management/blob/release/mdm/profiles/com.apple.mail.managed.yaml)
- [WWDC26 device management](https://developer.apple.com/videos/play/wwdc2026/206/)

**Microsoft identity, consent and Exchange**

- [Microsoft plug-in](https://learn.microsoft.com/en-us/entra/identity-platform/apple-sso-plugin)
- [Microsoft Platform SSO deployment](https://learn.microsoft.com/en-us/intune/device-configuration/settings-catalog/configure-platform-sso-macos)
- [Redirect URI requirements](https://learn.microsoft.com/en-us/entra/msal/objc/redirect-uris-ios)
- [MSAL cache-sharing setup](https://learn.microsoft.com/en-us/entra/msal/objc/single-sign-on-macos-ios)
- [Current consent policy, checked 2026-10-12](https://learn.microsoft.com/en-us/entra/identity/enterprise-apps/manage-app-consent-policies)
- [MSAL browser documentation](https://learn.microsoft.com/en-us/entra/msal/objc/customize-webviews)
- [EWS timeline, checked 2026-10-12](https://learn.microsoft.com/en-us/exchange/clients-and-mobile-in-exchange-online/deprecation-of-ews-exchange-online)
- [Microsoft protocol scopes](https://learn.microsoft.com/en-us/exchange/client-developer/legacy-protocols/how-to-authenticate-an-imap-pop-smtp-application-by-using-oauth)
- [Microsoft API terms, October 2025](https://learn.microsoft.com/en-us/legal/microsoft-apis/terms-of-use)
- [identity-platform terms](https://learn.microsoft.com/en-us/legal/microsoft-identity-platform/terms-of-use)

**GNOME APIs, implementation and policy**

- [Release index](https://download.gnome.org/sources/gnome-online-accounts/3.58/)
- [GOA overview](https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/overview.html)
- [GOA policy](https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/goals.html)
- [current client documentation](https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/class.Client.html)
- [3.58.1 build options](https://raw.githubusercontent.com/GNOME/gnome-online-accounts/3.58.1/meson_options.txt)
- [Graph provider](https://raw.githubusercontent.com/GNOME/gnome-online-accounts/3.58.1/src/goabackend/goamsgraphprovider.c)
- [EDS mapping](https://raw.githubusercontent.com/GNOME/evolution-data-server/master/src/modules/gnome-online-accounts/module-gnome-online-accounts.c)
- [Services table](https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/services.html)
- [release history](https://raw.githubusercontent.com/GNOME/gnome-online-accounts/master/NEWS)
- [D-Bus XML](https://raw.githubusercontent.com/GNOME/gnome-online-accounts/3.58.1/data/dbus-interfaces.xml)
- [Geary's mediator](https://raw.githubusercontent.com/GNOME/geary/main/src/client/application/goa-mediator.vala)
- [attention property](https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/property.Account.attention-needed.html)
- [OAuth provider refresh implementation](https://raw.githubusercontent.com/GNOME/gnome-online-accounts/3.58.1/src/goabackend/goaoauth2provider.c)
- [Google provider](https://raw.githubusercontent.com/GNOME/gnome-online-accounts/3.58.1/src/goabackend/goagoogleprovider.c)
- [Google IMAP OAuth requirements](https://developers.google.com/workspace/gmail/imap/xoauth2-protocol)
- [OAuth policy, modified 2026-08-05](https://developers.google.com/identity/protocols/oauth2/policies)
- [User Data Policy](https://developers.google.com/terms/api-services-user-data-policy)
- [restricted-scope verification](https://developers.google.com/identity/protocols/oauth2/production-readiness/restricted-scope-verification)

**Linux packaging and portals**

- [Geary manifest](https://raw.githubusercontent.com/flathub/org.gnome.Geary/master/org.gnome.Geary.yaml)
- [Evolution manifest](https://raw.githubusercontent.com/flathub/org.gnome.Evolution/master/org.gnome.Evolution.json)
- [Flatpak permissions](https://docs.flatpak.org/en/latest/sandbox-permissions.html)
- [Flathub review](https://docs.flathub.org/docs/for-team-members/review)
- [current requirements](https://docs.flathub.org/docs/for-app-authors/requirements)
- [Account portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Account.html)
- [Email portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Email.html)
- [Snap accounts-service](https://snapcraft.io/docs/reference/interfaces/accounts-service-interface/)
- [snapd GOA policy](https://raw.githubusercontent.com/canonical/snapd/master/interfaces/builtin/accounts_service.go)
- [other online-accounts policy](https://raw.githubusercontent.com/canonical/snapd/master/interfaces/builtin/online_accounts_service.go)

**KDE and other desktops**

- [KOnlineAccounts](https://github.com/KDE/konlineaccounts)
- [KAccounts integration](https://github.com/KDE/kaccounts-integration)
- [Accounts-SSO project](https://accounts-sso.gitlab.io/)
- [AuthService](https://accounts-sso.gitlab.io/libsignon-glib/gdbus-com.google.code.AccountsSSO.SingleSignOn.AuthService.html)
- [AuthSession](https://accounts-sso.gitlab.io/libsignon-glib/gdbus-com.google.code.AccountsSSO.SingleSignOn.AuthSession.html)
- [Provider/service source tree](https://github.com/KDE/kaccounts-providers)
- [Google scope file](https://github.com/KDE/kaccounts-providers/blob/master/providers/google.provider.in)
- [KMail setup](https://docs.kde.org/trunk_kf6/en/kmail/kmail2/getting-started.html)
- [Akonadi integration commit](https://phabricator.kde.org/R165:b2b0dd482b213ba774d582e427c37b400dc4b997)
- [Developer's assessment](https://nicolasfella.de/posts/a-new-online-accounts-system/)
- [XApp GOA frontend](https://github.com/xapp-project/gnome-online-accounts-gtk)
- [XFCE settings documentation](https://docs.xfce.org/xfce/xfce4-settings/start)
- [COSMIC settings source](https://github.com/pop-os/cosmic-settings)
- [community accounts project](https://github.com/cosmic-utils/accounts)

**Mail-client precedents and metadata readers**

- [current reader's implementation documentation](https://github.com/like-a-freedom/rusty_apple_mail_mcp)
- [Mimestream implementation](https://mimestream.com/help/user-guide/adding-accounts)
- [OAuth developer docs](https://source-docs.thunderbird.net/en/latest/backend/oauth.html)
- [GOA issue](https://bugzilla.mozilla.org/show_bug.cgi?id=847989)
- [Spark setup](https://support.readdle.com/spark/spark-onboarding/connect-to-your-email-account-in-spark)
- [Airmail setup](https://help.airmailapp.com/en-us/article/add-new-account-airmail-for-macos-nnue2q/)
- [Canary setup](https://canarymail.io/help/adding-a-gmail-account-in-canary-macos)
- [security model](https://mimestream.com/trust/security-and-privacy)
- [Microsoft setup](https://support.microsoft.com/en-us/outlook/add-email-accounts-in-outlook-for-mac-using-microsoft-cloud)
- [Account setup](https://www.emclient.com/blog/how-do-i-create-or-add-a-new-email-account-in-em-client-606)
- [import discussion](https://forum.emclient.com/t/importing-from-apple-mail/72164)
- [Quick-start guide](https://www.getmailspring.com/docs/quick-start-guide)
- [User guide](https://help.gnome.org/evolution/intro-first-run.html)

**Rust libraries and standards**

- [Own OAuth](https://www.rfc-editor.org/rfc/rfc8252.html)
- [zbus](https://docs.rs/zbus/latest/zbus/)
- [oauth2](https://docs.rs/oauth2/latest/oauth2/)
- [secrecy](https://docs.rs/secrecy/latest/secrecy/)
- [RFC 7636](https://www.rfc-editor.org/rfc/rfc7636.html)