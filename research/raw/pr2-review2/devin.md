<!-- provenance: helper=devin model=swe-2-max started=2026-10-12T05:56:07+00:00 exit=0 brief=research/briefs/ -->

# PR #2 adversarial review, round 2 — system accounts and account colors

VERDICT: APPROVE

All round-1 findings are fixed and verified against primary sources. Three
non-blocking issues remain; none rises to the blocking bar.

## What was checked

I re-verified the round-1 fixes in place: `MailDisabled` lifecycle and
feature-interface churn ([14:234-242](../../14-system-accounts.md)),
`GetAccessToken` returning 0 for unknown expiry (14:243-246, confirmed in
[dbus-interfaces.xml](https://raw.githubusercontent.com/GNOME/gnome-online-accounts/3.58.1/data/dbus-interfaces.xml)),
reply restoring the recorded sending identity incl. aliases (15:106-109),
`fullScreenMinHeight` for full-screen strips (15:172-175), sendable-only
From identities (15:126-129), monogram-collision fallback (15:26-28),
hedged Apple Mail/Outlook absence (15:42-43), the corrected GOA policy
reading ("The policy makes no exception for settings import or password
accounts", 14:190-193), and the rewritten Firefox status (14:79-83 —
[bug 1921535](https://bugzilla.mozilla.org/show_bug.cgi?id=1921535) is
RESOLVED FIXED, milestone 157).

New claims spot-checked against primaries, all confirmed: GNOME's
"contact the maintainers" and "not falsely advertised as 'GNOME'"
quotes ([GOA goals](https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/goals.html));
GOA client ids, scopes (`https://mail.google.com/`, Graph
`mail.readwrite`/`mail.send`), imap.gmail.com/smtp.gmail.com XOAUTH2
export, Windows-Live removal in 3.55, MS365-client fix #335
([provider sources](https://raw.githubusercontent.com/GNOME/gnome-online-accounts/3.58.1/src/goabackend/goagoogleprovider.c),
[NEWS](https://raw.githubusercontent.com/GNOME/gnome-online-accounts/master/NEWS));
GOA's no-destroy-on-removal guidance ([overview](https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/overview.html));
`ACAccountStore` deprecation; ManagedApp coming to macOS 27 (WWDC26
session 206 transcript); `CNAuthorizationStatus.limited` iOS-only;
accounts-service snap interface (auto-connect: no, per snapcraft docs);
KAccounts Google scopes lacking Gmail and the disabled
calendar/contacts service files; the Google-block quote in
[KDE bug 480779](https://bugs.kde.org/show_bug.cgi?id=480779);
kaccounts 26.08.2 tags; [KOnlineAccounts README](https://github.com/KDE/konlineaccounts)
prototype status; Geary + Evolution Flatpak
`--talk-name=org.gnome.OnlineAccounts`; eM Client's top-bar quote
(2026-08-06 post); Thunderbird 128 From-field coloring (2024-07-11);
`gtk_style_context_add_provider` deprecation in 4.10 while
`add_provider_for_display` is not; `--headerbar-backdrop-color`;
libadwaita 1.6 accent vars; `gtk_accessible_announce` 4.14+;
`NSToolbarItem.Style.prominent`/`backgroundTintColor`;
`NSWindowTab.accessoryView` (10.13+); the four WCAG contrast ratios
(recomputed: 2.25, 1.32, ~5.17, ~4.06); RFC 9979's seven flag colors;
and raw-report fidelity (devin's 3–4 pt `NSGlassEffectView` strip and
per-widget GTK provider are accurately attributed in both conflict
tables; his 2026-10-07 eM Client date was corrected to 2026-08-06).

D30–D31 match the documents and D26/D2/D3/D23. All cited links resolve.

## Findings

| Severity | File:line | Problem | Exact fix |
|---|---|---|---|
| Minor | 00-summary.md:322 | Map still says "decision-agenda.md: D1–D29", but the agenda now contains D30–D31 | Change to "D1–D31" |
| Minor | 14-system-accounts.md:288 | Binding uses "GOA object path" as the external handle; GOA's own consumer docs name `Account:Id` (type `s`) as the key for stored data — object paths encode it but are a session surface | Say the external handle is `Account:Id`; keep the object path only as a per-session D-Bus handle |
| Minor | 14-system-accounts.md:225-251 | Lifecycle rules don't cover `Account:IsTemporary` (silently discarded after logout) or `IsLocked` (admin-deployed, removal discouraged). Binding to a temporary account would mysteriously revert at logout | Add a line: skip or visibly label `IsTemporary` accounts on import/bind; treat `IsLocked` as an enterprise-managed source |

## View on the two open calls

### (a) GOA OAuth token reuse only with a documented GNOME agreement — agree

Keep the gate. The facts support it: GOA tokens are issued to GNOME's
client registration (`44438659992-…`, `b155a604-…` in meson_options),
there is no scope argument on `GetAccessToken`, and the grant carries
GNOME's provider verification, which doesn't transfer. GNOME's own
policy asks third parties to contact maintainers *before* integrating
and to keep their own identity. The shared-client risk is concrete —
Google cut KDE's client off Drive in June 2024 (Nate Graham's commit
message in bug 480779). Sweetbriar branding itself as "GNOME" at
consent time is exactly what the goals page forbids; a written
agreement covering consent attribution, permitted consumers and scopes
is the only way to know which identity the user actually saw.

The strongest counter-argument: `GetAccessToken` is a documented,
supported API, Geary and Evolution use it today without a bespoke
agreement, and requiring written sign-off may delay the nicest Linux
onboarding path indefinitely. That argument is weaker than it looks:
Geary and Evolution are the core-GNOME apps GOA exists for — the
policy's letter is aimed at branded third parties like Sweetbriar. The
doc also correctly unbundles the paths (settings import and password
accounts proceed after the policy-mandated contact; only OAuth reuse
waits for the agreement), so the gate costs little. As a product
decision this is the right strictness.

### (b) Opaque identity strip as the default — agree

The opaque strip is the better default on the evidence. Its contrast is
computable, so the 4.5:1/3:1 targets are testable claims rather than
hopes; tinted glass mixes the account color with arbitrary window
content and Liquid Glass's tinted/clear modes, so the same hue can fail
contrast on one draft and pass on another. Opaque also survives Reduce
Transparency and future glass changes — a real maintenance point on a
two-OS-generation floor. It matches the closest shipping precedent
(eM Client 11's top bar) and leaves the toolbar native, which Apple's
HIG prefers. Strong mode already serves users who want the
Terminal-profile full-window look, so the owner's "window color" ask is
still reachable as an option.

The strongest counter-argument: a strip is easier to tune out than a
whole-window tint — exactly the inattention the feature exists to fix —
and eM Client's choice could be fashion, not evidence. Fair, but the
safety does not ride on the strip: the full From address, the
sendable-only selector, the reply resolver, the cross-account check and
the frozen send route carry it (per the doc and D31). A louder
background doesn't fix wrong-identity sends; the deterministic checks
do. Default strip + opt-in Strong is the right split.

## Residual risks not in the findings table

- Apple Mail's scripting dictionary remains "per Devin, unverified" —
  correctly hedged (14:106-114); it gates only an optional importer.
- Firefox `ASWebAuthenticationSession` support is dated but unproven
  end-to-end (14:79-83) — hedged; browser-callback probe is already an
  open item.
- "No surveyed third-party Mac mail client reuses Internet Accounts
  credentials" (14:58-60) is a survey claim, not exhaustive — wording
  already qualifies it.
- The KDE/Google provider and GOA claims were verified at 3.58.1 /
  kaccounts 26.08.2; both trees are live, so the "checked 2026-10-12"
  stamp matters and is present.
