# Topic: Using system-level accounts (macOS Internet Accounts, GNOME Online Accounts, KDE Accounts)

The project owner asks: "macOS and I think also Linux allow logging into
accounts on system level. Can we use these accounts?"

A snapshot of the project's research is in `./research/` (start with
`research/00-summary.md`). Relevant settled decisions: own OAuth app
registrations (D26), Microsoft's consent policy and Google's restricted-scope
verification (`research/06-account-backends.md`), a contacts source (D22),
the Rust core owning accounts and sync (`research/01-architecture.md`).
Answer on the merits; if using system accounts would change a settled
decision, say so with evidence.

## Questions

1. **macOS Internet Accounts** (System Settings → Internet Accounts):
   - What it is technically on macOS 26/27 (accountsd, Accounts.framework,
     `~/Library/Accounts`), and which account types it holds (iCloud,
     Google, Microsoft Exchange/365, Yahoo, AOL, "Other": IMAP/POP mail,
     CalDAV, CardDAV, LDAP).
   - Is there any **public, supported** API that lets a third-party app
     (sandboxed or Developer ID) enumerate these accounts, get their server
     settings, or obtain OAuth tokens or passwords? Status of
     Accounts.framework (`ACAccountStore`, remaining account types,
     deprecations) as of macOS 27. Cite Apple documentation.
   - What third-party apps *can* reach indirectly: Contacts.framework and
     EventKit expose data from CardDAV/CalDAV/Exchange accounts configured
     there. What exactly, and with which permissions?
   - Keychain: could an app read credentials Apple Mail or accountsd
     stored (legacy file keychain prompts, data-protection keychain, access
     groups)? Is that possible, and is it acceptable?
   - Private APIs and protected files (`~/Library/Accounts`,
     `~/Library/Mail`): what breaks, what TCC/MAC protections apply on
     macOS 27, notarization and App Store consequences.
   - **Microsoft Enterprise SSO plug-in / Platform SSO**: can a
     third-party Mac app using MSAL get single sign-on tokens for Microsoft
     365 from the system (Company Portal, Platform SSO, Extensible
     Enterprise SSO)? Requirements (MDM, managed device, app allow-lists),
     and how that would work with a Rust core (token acquired in Swift via
     MSAL, handed to Rust).
   - `ASWebAuthenticationSession`: does it reuse the user's browser session
     (Safari or the default browser) so OAuth becomes one click when the
     user is already signed in to Google or Microsoft? Current macOS
     behaviour with non-Safari default browsers.
   - Managed configuration: can MDM pre-configure Sweetbriar's accounts
     (managed preferences for the app's domain, Managed App Configuration,
     Declarative Device Management account configurations, which may be
     Apple-apps only)?
   - Any other macOS 26/27 mechanism worth knowing (AutoFill password
     providers, credential exchange, App Intents, Apple Mail's MDM mail
     payloads).
2. **GNOME Online Accounts (GOA)**:
   - Current version (GNOME 49/50 era), the D-Bus API
     (`org.gnome.OnlineAccounts`: Account, OAuth2Based `GetAccessToken`,
     PasswordBased, Mail with IMAP/SMTP settings, Exchange, Calendar,
     Contacts), and the provider list (Google, Microsoft personal,
     Microsoft 365, Exchange, IMAP/SMTP, Nextcloud, WebDAV, Kerberos ...).
     Cite the GOA source or docs.
   - Which mail clients use it (Evolution, Geary, others) and how they
     handle token refresh, `AttentionNeeded`, `EnsureCredentials`, account
     removal.
   - **Whose OAuth client is it?** GOA's Google and Microsoft providers use
     GNOME's client IDs. What does that mean for Sweetbriar: Google
     verification and restricted scopes (does GNOME's client have the
     Gmail scope today?), Google and Microsoft terms on one app using
     another app's tokens, Microsoft 365 tenant consent for GNOME's app,
     the option to configure a custom client ID.
   - Flatpak: access to the GOA D-Bus name (`--talk-name`), Flathub review
     policy, whether any portal covers online accounts (and what the
     Account portal actually does). What Geary's and Evolution's Flatpaks do.
   - Snap and .deb differences.
3. **KDE**: KAccounts (libaccounts, signond), its D-Bus API and providers,
   whether KMail/Akonadi or other mail apps use it in 2026, and its
   maintenance status. Other desktops (Cinnamon, XFCE, COSMIC).
4. **Precedents**: how Thunderbird, Spark, Airmail, Canary, Mimestream,
   Outlook for Mac, eM Client, Mailspring, Geary, Evolution and KMail
   handle system accounts. Does any third-party Mac mail client read
   Internet Accounts? (If none can, say why.)
5. **Recommendation for Sweetbriar**, per platform:
   - Which uses are possible and worth it: (a) a token source, (b) an
     import/autofill source for server settings and identities, (c) an SSO
     broker, (d) an indirect source for contacts and calendars.
   - How it fits the architecture: a credential/token-source abstraction
     in the Rust core (own OAuth, GOA, MSAL broker, app password),
     ownership of refresh, what happens when the system account is
     removed or needs attention (pause, never delete local history), how
     an account's identity and namespace stay stable if the token source
     changes.
   - UX: what "Use an account from System Settings" can honestly mean on
     each platform.
   - The strongest counter-argument, and what would change the
     recommendation.
