<!-- provenance: helper=devin model=swe-2-max started=2026-10-12T05:25:24+00:00 exit=0 brief=research/briefs/ -->

VERDICT: CHANGES REQUESTED

# Review: PR #2 — system accounts and account colors

The two documents are largely accurate and consistent with settled
decisions (D1–D3, D22–D26): I verified the core claims against Apple,
GNOME/KDE, Microsoft, Mozilla and vendor primary sources (list below).
The architecture — credential sources as optional inputs, never the
account authority; color as recognition layered over exact-address and
frozen-route correctness — is sound. The remaining issues are a small
set of verifiable factual errors and unverified claims, mostly trivial
to fix, plus one design gap worth making explicit before implementation.

## Findings

| Severity | File:line | Problem | Exact fix |
|---|---|---|---|
| Medium | research/14-system-accounts.md:77-79, 334; research/notes/working-notes.md:1072-1074 | Stale Firefox claim. Bug 1921535 was fixed 2026-09-02 for the 157 branch; Firefox 157 **already shipped 2026-09-29** ([release calendar](https://whattrainisitnow.com/api/release/schedule/?version=157), [bug flags](https://bugzilla.mozilla.org/rest/bug/1921535): `cf_status_firefox157=fixed`). "The shipping release is unverified" is wrong — the code shipped 13 days ago; what is unverified is end-to-end behavior, confirmed only in 158 betas per [eduVPN #589](https://codeberg.org/eduVPN/apple/issues/589) (via devin.md:277). 158 ships 2026-10-13. | Replace with: "bug 1921535 fixed 2026-09-02, shipped in Firefox 157 (released 2026-09-29); eduVPN testers report it working only in 158 betas (158 ships 2026-10-13). Treat Firefox support as available but unproven until tested." |
| Nit | research/14-system-accounts.md:120; research/notes/working-notes.md:1038-1039 | GOA 3.58.1 date is 2026-04-11; the release archive timestamps the tarball [2026-04-12](https://download.gnome.org/sources/gnome-online-accounts/3.58/). | Change to 2026-04-12 in both places. |
| Nit | research/15-account-colors.md:212 | "libadwaita 1.6+ allows a per-widget accent override" — the mechanism and variables exist ([css-variables docs](https://gnome.pages.gitlab.gnome.org/libadwaita/doc/1-latest/css-variables.html)) but the 1.6 version attribution is unverified here. | Cite the version that introduced per-widget accent override, or say "current libadwaita". |
| Nit | research/15-account-colors.md:33-34 | Group claim "Spark, Airmail, Canary, Mailspring and Mimestream mark unified-inbox rows": I verified [Mimestream](https://mimestream.com/help/user-guide/unified-inbox) and [Mailspring (PR #2240)](https://github.com/Foundry376/Mailspring/pull/2240); Spark/Airmail/Canary not verified in this review. | Add per-client citations from the raw reports, or qualify "(e.g. Spark, Mimestream, Mailspring)". |
| Nit | research/15-account-colors.md:55-56 | Quoted string "never send an email from the wrong email address" not independently verified. The Aug-6 sneak-peek post exists and the beta/release posts use "the color shows in the top bar/header of new message and reply windows so you can easily tell which account is being used". | Verify the exact string on the 2026-08-06 post or replace the quote with the verified wording. |
| Non-blocking | research/15-account-colors.md:393-394 | Aliases, shared mailboxes, Send As and On Behalf Of are parked in Open. The strip "shows the identity but can't settle authorization" is right, but the document never states the inverse constraint: the From selector must only enumerate identities the backend has validated as sendable for that account. An implementer could read the identity model as purely presentational. | Add one line under "Compose behaviour": "The From list enumerates only identities the account backend has validated as sendable (including aliases, Send As and On Behalf Of grants); the strip displays that choice and never extends it." |

## Own view on the two open calls

**(a) GOA OAuth token reuse only with GNOME's agreement — endorse.**
The doc's gate is correct on three independent grounds, all verified:
GNOME's own [policy page](https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/goals.html)
asks third-party apps to contact maintainers and the design/release
teams and to keep their own brand identity; Google's restricted-scope
verification and Microsoft's per-app consent bind grants to the *client
that requested them* (GOA's tokens are issued to GNOME's
`44438659992-…` Google and `b155a604-…` Microsoft registrations, per
[provider source](https://gitlab.gnome.org/GNOME/gnome-online-accounts/-/blob/master/src/goabackend/goagoogleprovider.c)
and [build options](https://raw.githubusercontent.com/GNOME/gnome-online-accounts/3.58.1/meson_options.txt)),
so reusing them presents GNOME's identity, not Sweetbriar's; and the KDE
precedent shows a shared desktop client can be cut off wholesale
([KDE bug 480779](https://bugs.kde.org/show_bug.cgi?id=480779), "Google
blocked us from using this back in June because we weren't able justify
our API usage"). The strongest counter-argument: `GetAccessToken` is a
documented consumer API that Geary and Evolution already use, the token
is a bearer token that technically works, and "agreement" may never
materialize — GNOME is not obliged to answer. That is a real cost, but
the doc draws the boundary in the right place: settings import and
IMAP/SMTP password accounts (the user's own credentials through a
generic provider) need no agreement, own-OAuth remains the baseline, and
GOA OAuth is only a convenience path. Where the doc could be stronger:
it never says what "agreement" operationally means — worth spelling out
(written confirmation from GOA maintainers plus a user-visible
Sweetbriar identity at first use).

**(b) Opaque identity strip as the default cue — endorse.**
Three independent reasons: tinted glass composites over arbitrary window
content, so its contrast cannot be computed or guaranteed (the doc's
WCAG-2.2 targets — 4.5:1 strip text — are only reachable on a
deterministic fill; my own computation confirms the palette can't serve
both black and white text, e.g. white-on-orange 2.25:1, black-on-blue
4.05:1); Reduce Transparency and the Liquid Glass clear/tinted modes
change how a glass cue renders, and an opaque strip survives both; and
the two shipping precedents — eM Client 11's top bar ([release post](https://www.emclient.com/blogpost.aspx?id=842),
2026-10-07) and [Thunderbird 128's colored From field](https://www.thunderbird.net/en-US/thunderbird/128.0esr/releasenotes/)
(2024-07-11) — are bounded surfaces, not whole-window tints, matching
Apple's toolbar guidance against custom chrome backgrounds. The
strongest counter-argument is the owner's own request: a tinted window
is the most glanceable cue at distance and in Mission Control
thumbnails. The doc answers that correctly by keeping Strong mode as an
opt-in and putting the account name in the window title — the cue is not
lost, just not the default.

## What I checked

- **Apple:** Accounts framework metadata ([ACAccountStore](https://developer.apple.com/documentation/accounts/acaccountstore)
  deprecated macOS 12; [Twitter type](https://developer.apple.com/documentation/accounts/acaccounttypeidentifiertwitter)
  deprecated 10.13); [CNAuthorizationStatus.limited](https://developer.apple.com/documentation/contacts/cnauthorizationstatus/limited)
  (no macOS); [ASWebAuthenticationSession](https://developer.apple.com/documentation/authenticationservices/aswebauthenticationsession)
  ("default browser if it supports … or Safari"); [TN3152](https://developer.apple.com/documentation/technotes/tn3152-migrating-to-the-latest-calendar-access-levels)
  (EventKit macOS 14 levels); [ManagedApp](https://developer.apple.com/documentation/managedapp/managedappconfigurationprovider)
  (macOS 27); [credential exchange](https://developer.apple.com/documentation/authenticationservices/ascredentialexportmanager)
  (macOS 26); [ManagedClient.preferences](https://developer.apple.com/documentation/devicemanagement/managedpreferences);
  [NSGlassEffectView](https://developer.apple.com/documentation/appkit/nsglasseffectview),
  [NSToolbarItem.backgroundTintColor](https://developer.apple.com/documentation/appkit/nstoolbaritem/backgroundtintcolor),
  `.prominent` style (all macOS 26).
- **GNOME:** GOA [overview](https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/overview.html)
  (no-data-destruction rule), [D-Bus interfaces](https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/dbus-org.gnome.OnlineAccounts.OAuth2Based.html)
  (GetAccessToken signature, no scope arg), [Google](https://gitlab.gnome.org/GNOME/gnome-online-accounts/-/blob/master/src/goabackend/goagoogleprovider.c)
  and [ms_graph](https://gitlab.gnome.org/GNOME/gnome-online-accounts/-/blob/master/src/goabackend/goamsgraphprovider.c)
  provider sources, [goals/policy](https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/goals.html),
  [3.58 archive](https://download.gnome.org/sources/gnome-online-accounts/3.58/),
  [issue #335](https://gitlab.gnome.org/GNOME/gnome-online-accounts/-/issues/335),
  [Geary mediator](https://gitlab.gnome.org/GNOME/geary/-/blob/main/src/client/application/goa-mediator.vala)
  (correct path — the file exists, code matches doc), Geary tags (latest
  46.0 — no bearing on the doc, which cites no version), Geary/Evolution
  Flatpak manifests, [XDG Account portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Account.html),
  snapd [accounts-service](https://raw.githubusercontent.com/canonical/snapd/master/interfaces/builtin/accounts_service.go)
  (`deny-auto-connection`) and [online-accounts-service](https://raw.githubusercontent.com/canonical/snapd/master/interfaces/builtin/online_accounts_service.go)
  (different API, auto-connect), [signond docs](https://accounts-sso.gitlab.io/libsignon-glib/gdbus-com.google.code.AccountsSSO.SingleSignOn.AuthService.html)
  (unstable API), Ubuntu resolute packages (GOA 3.58.0-1,
  gnome-online-accounts-gtk 3.50.9-1 — XApp project, so the Cinnamon
  claim holds), libadwaita [CSS variables](https://gnome.pages.gitlab.gnome.org/libadwaita/doc/1-latest/css-variables.html)
  (headerbar bg/fg/backdrop all present),
  [gtk_style_context_add_provider](https://docs.gtk.org/gtk4/method.StyleContext.add_provider.html)
  (deprecated 4.10), [gtk_accessible_announce](https://docs.gtk.org/gtk4/method.Accessible.announce.html)
  (4.14), [GtkColorDialogButton](https://docs.gtk.org/gtk4/class.ColorDialogButton.html)
  (4.10).
- **KDE:** [kaccounts-providers](https://github.com/KDE/kaccounts-providers/tree/master/providers)
  (Google/Nextcloud/ownCloud only; [Google scopes](https://raw.githubusercontent.com/KDE/kaccounts-providers/master/providers/google.provider.in)
  — no Gmail; google-calendar/contacts in `services/disabled/`),
  kaccounts-integration/providers tags (v26.08.2), [KOnlineAccounts](https://github.com/KDE/konlineaccounts)
  ("prototype stage, not ready for production"), [Fella's post](https://nicolasfella.de/posts/a-new-online-accounts-system/)
  (2025-11-15), [bug 480779](https://bugs.kde.org/show_bug.cgi?id=480779).
- **Microsoft/Mozilla:** [Enterprise SSO plug-in](https://learn.microsoft.com/en-us/entra/identity-platform/apple-sso-plugin)
  (MSAL automatic, non-MSAL allow-list), [EWS retirement](https://learn.microsoft.com/en-us/exchange/clients-and-mobile-in-exchange-online/deprecation-of-ews-exchange-online)
  (blocking began Oct 2026 — the doc's "retiring" is right), [bug 1921535](https://bugzilla.mozilla.org/rest/bug/1921535)
  + [release calendar](https://whattrainisitnow.com/), bugs [355520](https://bugzilla.mozilla.org/rest/bug/355520)
  (2006 identity cue, fixed in 128), [704613](https://bugzilla.mozilla.org/rest/bug/704613)
  ("8 accounts", verbatim), [688147](https://bugzilla.mozilla.org/rest/bug/688147)
  (wrong-account detection).
- **Vendor/other:** eM Client [sneak peek](https://www.emclient.com/blog)
  (2026-08-06) and [release](https://www.emclient.com/blogpost.aspx?id=842)
  (2026-10-07), Postbox [Domain Fencing](https://www.postbox-inc.com/features)
  (Postbox 4, May 2015), [Slack themes](https://slack.com/help/articles/205166337-Change-your-Slack-theme),
  [Superhuman 12 colors](https://headwayapp.co/superhuman-mail-updates),
  [Graph 25 category presets](https://learn.microsoft.com/en-us/graph/api/resources/outlookcategory),
  [Mimestream](https://mimestream.com/help/user-guide/unified-inbox),
  [RFC 9979](https://www.rfc-editor.org/rfc/rfc9979.txt) (seven flag
  colors — correct), Gmail [IMAP capabilities](https://developers.google.com/gmail/imap/imap-extensions)
  (no METADATA), [palette](https://docs.rs/crate/palette/latest) (latest
  0.7.7; doc names no version), WCAG contrast math recomputed locally.
- **Consistency:** D30/D31 rows and 00-summary sections match the docs
  and settled decisions; no contradiction with debate/04-resolution
  (no-delete-on-account-removal, same-principal validation, no auto-retry
  of unknown sends all respected).
