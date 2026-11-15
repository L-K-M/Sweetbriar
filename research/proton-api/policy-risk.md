# R4 — Policy, terms and risk of a third-party Proton API client

References (read-only clones under `src/`): `go-proton-api` @ 79fbe06 (MIT), `hydroxide` @ a08bcca (MIT), `proton-bridge` @ 36d7b6a (GPL-3.0; protocol facts only, no code copied). Web pages accessed 2026-11-15. Repo paths below are relative to `src/`. API base hosts in use: `https://mail.proton.me/api` (`go-proton-api/manager_builder.go:13`, `hydroxide/cmd/hydroxide/main.go:33`) and `https://mail-api.proton.me` (`proton-bridge/internal/constants/host_default.go:23`).

## 1. Proton ToS and acceptable use

Proton's Terms of Service (https://proton.me/legal/terms, "Last modified: November 05, 2026") contain no API-specific terms: the string "API" does not occur in the page (checked 2026-11-15). The general rules that matter:

- §1: "Accounts registered by “bots” or automated methods are not authorized and will be terminated."
- §2, clause 2.10: "Accessing the Services through automated means (including but not limited to bots, scripts, or similar technologies) in a manner that is distinguishable from the standard client behavior of human users, that deviates significantly from normal usage patterns, that exhibits characteristics of abuse, or that attempts to circumvent the Company's security controls;"
- §2: "Notwithstanding clause 2.10 above, automated access to the Services is permitted provided that the resulting traffic remains indistinguishable from the standard client behavior of human users. The Company reserves the right, in its sole discretion, to block or restrict access in response to any activity that deviates significantly from normal usage patterns, exhibits characteristics of abuse, or attempts to bypass its security controls. Users engaging in such activity shall not be eligible for support or any recourse in relation to such access restrictions."
- §2: "Any Account found to be committing any of the listed unauthorized activities will be immediately suspended or restricted accordingly (which can include features and capabilities restrictions)."
- §2 also prohibits "Sharing your Account or login credentials with any other person or entity, except where such sharing is expressly permitted under your subscription plan or has been otherwise authorized in writing by the Company" and "Reselling, sublicensing, or otherwise making the Services available to any third party, except where expressly permitted under your subscription plan or authorized in writing by the Company."

No separate acceptable-use policy was found; the legal pages are terms, privacy, transparency, DPA, BAA and referral terms (https://proton.me/legal/terms; proton.me sitemap grep 2026-11-15). Inference: a direct-API mail client is "automated access" and is tolerated only while its traffic is indistinguishable from a human using a standard Proton client; Proton can block it without support or recourse.

## 2. Client identity: an app-version string, not a registered client ID

- Authentication is SRP with username/password (plus 2FA); the request carries no client ID or secret (`go-proton-api/manager_auth_types.go:36-43`, `manager_auth.go:31-73`; `hydroxide/protonmail/auth.go:50-112`). The only client identity sent is the `x-pm-appversion` header (`go-proton-api/manager_builder.go:76`; `hydroxide/protonmail/protonmail.go:92`).
- Proton's own test server treats the header as mandatory and validates it: missing → code 5001, bad → 5003 "This version of the app is no longer supported, please update to continue using the app" (`go-proton-api/server/router.go:170-186`; codes at `response.go:25-26`). The validator splits on `_` and expects a semver second part (`server/router.go:318-336`), i.e. the `Other_<SemVer>` shape.
- Real-server behaviour matches: default `go-proton-api` is rejected with HTTP 400 "Platform `go` is not valid" (https://github.com/ProtonMail/go-proton-api/issues/180); a stale/unknown string returns 5003 "This web page is out of date, please refresh the page to continue using it" (https://codeberg.org/emersion/hydroxide/issues/252).
- Known string shapes: third-party `Other_<SemVer>` (https://github.com/ProtonMail/go-proton-api/issues/227); web `web-mail@5.0.33.6` (https://github.com/ProtonMail/go-proton-api/issues/139) and `web-mail@5.0.124.7` (https://codeberg.org/emersion/hydroxide/pulls/354); Bridge `<os>-bridge@<version>` (`proton-bridge/internal/constants/version_default.go:24-27`, `constants.go:59,75-88`).
- No public registration path exists. Issue #227 asks for a named third-party identity and states that `Other` sessions are shown as "Unknown application" in sign-in alerts; it is still open with no Proton reply (https://github.com/ProtonMail/go-proton-api/issues/227). A developer reports: "I have submitted requests in through ProtonAG's support and business channels to get an approved AppString, but I have not received a reply" (comment on https://github.com/ProtonMail/go-proton-api/issues/139, 2026-05-17).
- The app string is not cosmetic: "How the client identifies itself seems to also contribute to the API calls a client is allowed to make ... even `Other` was limited on what I could and couldn't do" (comment on https://github.com/ProtonMail/go-proton-api/issues/227, 2026-07-29); "The servers themselves seem to scope all permissions to specific API endpoints based on the AppString" (https://github.com/ProtonMail/go-proton-api/issues/139, 2026-05-17). Community reports — not verified against server documentation.
- Server-side sessions carry a `ClientID` and `LocalizedClientName` (`go-proton-api/manager_auth_types.go:104-112`); the QR/session-fork flow uses a `ChildClientID` drawn from a Proton `ForkableApps` allowlist (https://github.com/ProtonMail/go-proton-api/issues/226). Inference: Proton has internal client-ID allowlists but no public registration.
- Note: `ClientID` in `go-proton-api/client.go:15-16,47` is only a local atomic counter for request/context bookkeeping; it is not sent to Proton.

## 3. Unofficial clients: observed blocks and incidents

- Human verification (CAPTCHA), API code 9001: "For security reasons, please complete CAPTCHA. If you can't pass it, please try updating your app or contact us here: https://proton.me/support/appeal-abuse" (https://codeberg.org/emersion/hydroxide/issues/328; code at `go-proton-api/response.go:29`). Reported repeatedly 2022-2026 (issues https://codeberg.org/emersion/hydroxide/issues/268, /235, /230, /252, /328).
- App string directly affects anti-bot: "`x-pm-appversion` being "Other" (and absence) triggers anti-bot. Solution: Use the exact ID from the webapp. Login works now." The reporter used `-app-version 'web-mail@5.0.124.7'` (https://codeberg.org/emersion/hydroxide/pulls/354, closed unmerged; current master still defaults to `Other`, `hydroxide/cmd/hydroxide/main.go:34`).
- IP/connection-based 9001: "Login temporarily not permitted from your connection for security reasons, please try again later. Using a VPN may cause this error" (https://codeberg.org/emersion/hydroxide/issues/194, /179).
- Account suspension: a user reported both accounts suspended; Proton support attributed it to "bulk registration", not hydroxide (https://codeberg.org/emersion/hydroxide/issues/276, 2024-08).
- Session/token instability: `[401] Invalid access token` on `/api/keys/salts` immediately after successful TOTP 2FA (https://codeberg.org/emersion/hydroxide/issues/345); `[10013] Invalid refresh token` (https://codeberg.org/emersion/hydroxide/issues/341).
- Mitigation shipped by hydroxide v0.2.33 (2026-10-25): commit 02b3b6c "cmd/hydroxide: ask user to complete human verification challenge" (https://codeberg.org/emersion/hydroxide/commit/02b3b6c8aff726d2404b6ed5f6ba1b7937e1f30e); a browser-CAPTCHA PR is still open (https://codeberg.org/emersion/hydroxide/pulls/358).

## 4. Human-verification mechanics (protocol facts)

Error 9001 details are JSON `{HumanVerificationMethods, HumanVerificationToken}` (`go-proton-api/hv.go:12-14`); the retry sends `x-pm-human-verification-token` and `x-pm-human-verification-token-type` (`manager_auth.go:140-141`, `hv.go:17-22`). Methods seen in tests: `captcha`, `ownership-email` (`proton-bridge/internal/hv/hv_test.go:31-34`). Bridge renders `https://verify.proton.me/?methods=...&token=...` (`proton-bridge/internal/hv/hv_default.go:28-31`). A bad HV token is code 12087 (`go-proton-api/response.go:32`).

## 5. go-proton-api: intended use and stability

- README: "a Go library implementing a client and development server for (a subset of) the Proton REST API"; "The library is maintained by Proton AG, and is not actively looking for contributors." (`go-proton-api/README.md:8,22`). GitHub description: "Proton API library used by Go-based clients and tools" (https://api.github.com/repos/ProtonMail/go-proton-api).
- `DefaultAppVersion = "go-proton-api"` with the comment "This must be changed (using the WithAppVersion option) for production use." (`manager_builder.go:15-17`).
- No API stability guarantee is stated. The MIT licence disclaims warranty (`go-proton-api/LICENSE`). Latest tagged module version is v0.4.0 (2023-02-09, https://proxy.golang.org/github.com/!proton!mail/go-proton-api/@v/v0.4.0.info); Proton Bridge consumes a 2026 pseudo-version `v0.4.1-0.20260901104215-8a62023d3e38` (`proton-bridge/go.mod:12`). The GitHub tags API currently returns only v0.1.4 (https://api.github.com/repos/ProtonMail/go-proton-api/tags). Inference: the listings disagree because upstream tags were removed after release while the Go module proxy, which is immutable, keeps v0.4.0; so a CI lane should pin an audited commit, not a tag. The library is also pre-1.0, changed heavily after 2023, and is used in production by Bridge without a stable release.
- It is the library Bridge uses (`proton-bridge/go.mod:12`), so it is "official" code, but the README does not promise third-party support.

## 6. How hydroxide identifies itself

- Default app version is the literal `Other` (`hydroxide/cmd/hydroxide/main.go:34`), sent verbatim as `X-Pm-Appversion` (`protonmail/protonmail.go:92`); overridable with `-app-version` (`main.go:227`). It also sends `X-Pm-Apiversion: 3` (`protonmail.go:19,21,93`) and a Firefox User-Agent (`protonmail.go:122`).
- The default does not use an official product name, so it does not impersonate an official client by name; the Firefox User-Agent does impersonate a browser. The known CAPTCHA workaround is to set `web-mail@5.0.124.7`, which does impersonate the official web client (https://codeberg.org/emersion/hydroxide/pulls/354).
- README self-describes as "A third-party, open-source ProtonMail bridge" (`hydroxide/README.md:3`).

## 7. Official statement about a public API

- No Proton support article about a Proton Mail API exists in the support sitemap (21,107 URLs; only "SCIM provisioning API", https://proton.me/support/scim-provisioning-api, for business user provisioning). Proton's open-source page lists apps and crypto libraries but not `go-proton-api` (https://proton.me/community/open-source).
- The only official signals found: the `go-proton-api` repository itself is maintained by Proton AG (README:22), and Proton publishes SDKs for other services — Proton Drive SDK is "not ready for production use or third-party apps yet" (https://proton.me/blog/proton-drive-sdk-preview). Unverified: any Proton statement that the Mail REST API is public, supported, or intended for third-party clients. The Proton community forum (community.proton.me) does not resolve from this environment, so forum statements could not be checked.

## 8. Plans: free vs paid

- Official Bridge: "Proton Mail Bridge is available only with a paid plan that includes Proton Mail." (https://proton.me/mail/bridge). Bridge maps API code 10004 to a "free user" login error (`proton-bridge/internal/frontend/grpc/service_methods.go:478-479`; code at `go-proton-api/response.go:30`).
- Direct API: a hydroxide user reports successful live-API use with a free account (https://codeberg.org/emersion/hydroxide/pulls/348, 2026-07-13). Inference: the paid-plan gate is at least partly client-identity dependent (Bridge's `<os>-bridge@...` string is paid-gated; `Other` is not), but this is not documented.
- 2FA is available on free and paid plans (https://proton.me/support/two-factor-authentication-2fa).

## 9. 2FA and FIDO2-only accounts

- TOTP works from third-party clients: `POST /auth/v4/2fa` with `TwoFactorCode` (`go-proton-api/auth.go:9-12`, `manager_auth_types.go:89-92`); hydroxide `POST /auth/2fa` (`hydroxide/protonmail/auth.go:152-165`).
- FIDO2 is a supported account state: `HasTOTP=1`, `HasFIDO2=2`, `HasFIDO2AndTOTP=3` (`manager_auth_types.go:74-80`); `AuthInfo` returns `TwoFA.FIDO2` with `AuthenticationOptions` and `RegisteredKeys` (`manager_auth_types.go:7-14,64-72`). Inference: Proton's docs imply FIDO2-only is allowed: if you lose your key and have no authenticator app, you use a recovery code (https://proton.me/support/2fa-security-key).
- go-proton-api only defines FIDO2 request types (`FIDO2Req`, `manager_auth_types.go:28-33`); it has no WebAuthn/FIDO2 dependency (`go-proton-api/go.mod`, no fido/webauthn entry). Bridge implements the ceremony with `go-libfido2` (`proton-bridge/go.mod:41,161`; `internal/frontend/grpc/fido.go:28`). Hydroxide has `U2F interface{} // TODO` (`hydroxide/protonmail/auth.go:93`) and an open feature request (https://codeberg.org/emersion/hydroxide/issues/321). `proton-crypto-rs` has `proton-srp` 0.9.0 but no FIDO2/WebAuthn code (`proton-crypto-rs/proton-srp/Cargo.toml:2-3`; grep finds no fido/webauthn). Inference: Sweetbriar must implement WebAuthn itself to support FIDO2-only accounts; no reference implementation exists in the Rust sources.

## 10. Risk summary for the owner

Facts:
- Proton's ToS permits automated access only if indistinguishable from human standard-client behaviour, and allows blocking/restriction with no support or recourse; violations can suspend the account (https://proton.me/legal/terms §1, §2).
- There is no registered-client programme; the only identifier is `x-pm-appversion`, which Proton validates and which affects anti-bot and API permissions (issues #180, #227, #354; `server/router.go:170-186`).
- Unofficial clients have hit CAPTCHA/9001, IP blocks, 400/5003 app-version rejections, token/refresh failures, and at least one account suspension (§3).
- Bridge (the supported path) is paid-only; direct-API use with free accounts is reported but undocumented (§8).
- TOTP works; FIDO2-only is possible but unimplemented in all third-party references (§9).
- `proton-bridge` is GPL-3.0 (`proton-bridge/LICENSE`); copying it would conflict with Sweetbriar's Unlicense/0BSD. `go-proton-api` and `hydroxide` are MIT.

Inferences:
- A third-party client will be judged largely on its app-version string and traffic pattern. Using `Other` is the honest identity but is reported to trigger anti-bot and permission limits; using an official string (e.g. `web-mail@...`) avoids CAPTCHA but is impersonation and can be blocked server-side at any time.
- Expect forced-update behaviour (5003) and HV loops; design for interactive CAPTCHA and token refresh.
- Account suspension is the worst case and is discretionary: with no recourse it can mean losing access to the whole mailbox, which is one reason Sweetbriar keeps complete local copies; the no-recourse clause means Proton support may refuse help.
- Direct API access is unlikely to be a supported, stable platform; treat it as best-effort.

- Credential custody: a third-party client holds session tokens and the derived key passphrase on the user's device (Sweetbriar never stores the password; spec/10). §1's clause against sharing login credentials with "any other person or entity" is not obviously about the user's own client, but it is untested.

## 11. Open questions

- What traffic pattern counts as "standard client behavior" (event polling cadence, retries and backoff, concurrency, header set)? Only Proton's own clients show it; Sweetbriar's pacing (spec/04-sync-proton) is an estimate.

- Is there an official Proton statement (blog, support article, forum post) that the Mail REST API is public or that third-party clients are permitted? community.proton.me is unreachable from this environment.
- Does Proton run a client-registration/allowlist process for app-version strings? Issue #227 is unanswered; ask Proton support/business channels.
- Which exact API endpoints or scopes are gated by app string, and is `Other` permanently limited? Only community reports exist.
- Is the paid-plan gate (10004) applied per app string or per account? Bridge sees it at login; hydroxide free-account use suggests `Other` is exempt — needs live testing.
- What are the valid `x-pm-appversion` grammar and minimum versions for third-party clients? The dev server requires `X_Y` semver (`server/router.go:318-336`), but real-server rules are undocumented.
- Does the real server require `X-Pm-Apiversion`? hydroxide sends 3 (`protonmail/protonmail.go:93`); go-proton-api does not.
- FIDO2 ceremony details (RP ID, allowlists, platform vs roaming keys) and whether FIDO2-only accounts can complete SRP login from a non-official client.
- What triggers "Unknown application" alerts and whether a named third-party identity is planned.
- Licence/distribution: confirm with counsel that clean-room reimplementation from protocol facts (not GPL Bridge code) is acceptable, and that storing user SRP credentials/refresh tokens is consistent with the ToS credential clause.
