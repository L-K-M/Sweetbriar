# R1 — Proton Mail: authentication and sessions

Research date: 2026-11-15. Sources: read-only clones (go-proton-api @ 79fbe06, hydroxide @ a08bcca, proton-bridge @ 36d7b6a, proton-crypto-rs and go-srp at their 2026-11-15 heads) (cited as `GPA`=go-proton-api, `GSRP`=go-srp, `PS`=proton-crypto-rs/proton-srp, `PC`=proton-crypto-rs/proton-crypto, `PCA`=proton-crypto-rs/proton-crypto-account, `BR`=proton-bridge, `HYD`=hydroxide), plus live `curl` probes of `https://mail.proton.me/api` and public web/sources. `BR` is GPL-3.0 and was read only for protocol facts. Live probes are cited as **Live** with the endpoint and date. Third-party repos are cited by URL.

## 1. API base URLs and required headers

- `https://mail.proton.me/api` — GPA default (`GPA/manager_builder.go:13`), HYD default (`HYD/cmd/hydroxide/main.go:33`), live OK [Live: `GET /api/tests/ping`, 2026-11-15].
- `https://mail-api.proton.me` — Bridge's API host, no `/api` suffix (`BR/internal/constants/host_default.go:23`); live OK [Live: `GET /auth/v4/modulus`, 2026-11-15].
- `https://api.protonmail.ch` (legacy) and `https://api.protonmail.ch/api` — live OK [Live: `GET /auth/v4/modulus`, 2026-11-15]; Python client lists it as `api` (`https://raw.githubusercontent.com/opulentfox-29/protonmail-api-client/master/src/protonmail/constants.py:24`).
- Account API: `https://account.proton.me/api`, `https://account-api.proton.me` (same Python `constants.py:26-27`); VPN API `https://vpn-api.proton.me` (documented in `https://pkg.go.dev/github.com/Arsylk/proton-auth`).
- Reachability test: `GET /tests/ping` (`GPA/manager_ping.go:8`; Bridge uses `/tests/ping`, `BR/internal/dialer/dialer_proxy_provider.go:184-195`). Live `{"Code":1000}` [Live: `GET /api/tests/ping`, 2026-11-15]. Response header `access: application/vnd.protonmail.api+json;apiversion=4` [Live: `GET /api/auth/v4/modulus`, 2026-11-15].
- Required on authenticated routes: `x-pm-uid: <UID>` and `Authorization: Bearer <AccessToken>` (`GPA/client.go:163-169`; `PS`-based clients do the same, e.g. `https://raw.githubusercontent.com/photuris/proton-mail-api/main/src/http.rs:399-411`). `Content-Type: application/json` for bodies.
- `x-pm-apiversion: 4` is sent by HYD (`HYD/protonmail/protonmail.go:93`) and the Python client (`constants.py:19`); live API answers without it [Live: `POST /api/auth/v4/modulus` with only `x-pm-appversion`, 2026-11-15]. Inference: optional.
- Server sets `Session-Id` (Domain `proton.me`, HttpOnly, Secure, SameSite=None, Max-Age 7776000 = 90 days) and `Tag=default` [Live: any `/api` response, 2026-11-15]; the GPA test server mirrors the 90-day `Session-Id` (`GPA/server/router.go:188-205`). Bridge persists cookies (`BR/internal/vault/cookies.go:20-28`).
- HV retry headers: `x-pm-human-verification-token`, `x-pm-human-verification-token-type` (`GPA/manager_auth.go:140-141`, `GPA/hv.go:20-27`).

## 2. `x-pm-appversion`: format and enforcement

Format is `platform-product@semver`, all lowercase; Bridge renders `{os}-{app}@{version}` (`BR/internal/constants/version_default.go:25-27`, `AppName="bridge"` at `constants.go:59`). The old unofficial spec documents the special value `Other` (`https://raw.githubusercontent.com/secure-mail-documentation-project/protonmail-api/main/protonmail-api.yaml:12038-12043`). Live results [Live: `GET /api/auth/v4/modulus`, 2026-11-15]:

| header | result |
|---|---|
| missing | HTTP 400 `{"Code":5002,"Error":"Missing x-pm-appversion header"}` |
| `Other`, `other` | HTTP 200 |
| `macos-bridge@3.31.0`, `macos-bridge@999.0.0`, `macos-bridge@3.31.0-beta` | HTTP 200 |
| `macos-bridge@0.0.1` | HTTP 422 `{"Code":5003,"Error":"This version of the app is no longer supported..."}` |
| `macos-mail@3.16.0`, `web-account@5.0.423.0` | HTTP 401 `{"Code":401,"Error":"Invalid access token"}` (registered product; endpoint refuses, reason not documented) |
| `macos-sweetbriar@1.0.0` | HTTP 400 `{"Code":2064,"Error":"Product 'sweetbriar' is not valid"}` |
| `macos-notes@1.0.0` | HTTP 400 Code 2064 (product invalid) |
| `go-proton-api` | HTTP 400 `{"Code":2064,"Error":"Platform 'go' is not valid"}` |
| `macos_mail@3.16.0` | HTTP 400 Code 2064 (platform/product must be separated by `-`) |
| `macos-bridge@notaversion`, `macos-bridge@3.31` | HTTP 400 Code 2064 (invalid/missing version) |
| `MACOS-BRIDGE@3.31.0` | HTTP 400 Code 2064 (must be lowercase) |

So Proton refuses unknown products/platforms (2064) and outdated versions (5003); missing/structurally invalid values give 5002. GPA's constants differ from production: `AppVersionMissingCode=5001`, `AppVersionBadCode=5003` (`GPA/response.go:25-26`), and its test server returns HTTP 400 and expects `name_version` (`GPA/server/router.go:170-186,318-339`). Bridge treats 5003 as "force update" (`BR/internal/bridge/bridge.go:389-393`). Inference: Sweetbriar cannot use `macos-sweetbriar@…`; use `Other` (accepted live, used by HYD `HYD/cmd/hydroxide/main.go:34` and Python `PM_APP_VERSION_DEV='Other'`, `constants.py:5`). A registered product string such as `macos-bridge@<semver>` is accepted but is impersonation, ruled out below. Unverified: whether Proton would register a new product for Sweetbriar. Note: sending a registered product string such as `macos-bridge@…` would misreport Sweetbriar as another Proton client (impersonation) and breaks whenever Proton raises that product's minimum version; Sweetbriar's spec rules it out (spec/10).

## 3. SRP login flow (reference: `PS`, `GSRP`, `GPA`)

1. `POST /auth/v4/info` with `{"Username":...}` → `{Code:1000, Modulus, ServerEphemeral, Version:4, Salt, SRPSession, Username, ClaimedAddresses}` (`GPA/manager_auth.go:75-85`, `GPA/manager_auth_types.go:7-14`; live confirmed [Live: `POST /api/auth/v4/info`, 2026-11-15]). Live: `SRPSession` is 32 hex chars, `Salt` base64 of 10 bytes, `Modulus` is a 2048-bit clear-signed value; a non-existent user still gets Code 1000 with a fixed salt (Inference: dummy) [Live, 2026-11-15].
2. Verify the modulus signature, then compute proofs:
   - `Modulus` is an OpenPGP cleartext-signed message (`-----BEGIN PGP SIGNED MESSAGE-----`); verify with Proton's embedded public key and reject trailing data (`PS/src/pgp_modulus.rs:32-77`, `GSRP/srp.go:39-43,72-92`). The key is embedded at `PS/resources/server_public_key.asc` (`PS/src/srp/client.rs:313-317`) and as `modulusPubkey` in `GSRP/srp.go:67`; UID `proton@srp.modulus`, fingerprint `248097092B458509C508DAC0350585C4E9518F26` (verified: `gpg --verify` on a live `/auth/v4/modulus` body, 2026-11-15). Live `/auth/v4/info` returns a stable modulus; `/auth/v4/modulus` returns a fresh signed modulus per request [Live, 2026-11-15].
   - Group: 2048-bit safe prime `N`, `N ≡ 3 mod 8`, generator `g=2`, 256-byte little-endian encoding (`PS/src/srp/core.rs:60-67,226-251`); reject `B ≡ 0` and `u=0` (`core.rs:118-123,170-178`).
   - Password hash v4: `x = expand_hash(bcrypt(password, salt ∥ "proton") ∥ N)` where `salt` is the 10-byte SRP salt, bcrypt cost 10, variant `$2y$` (`PS/src/pmhash.rs:18-27,108-119,178-210`; `GSRP/hash.go:50-58,110-118`). `expand_hash(d)=SHA512(d∥0)∥…∥SHA512(d∥3)` (256 bytes) (`PS/src/pmhash.rs:108-119`).
   - Proofs: `k=H(g∥N) mod N`, `u=H(A∥B) mod (N-1)`, `K=(B−k·g^x)^(a+u·x)`, client proof `H(A∥B∥K)`, expected server proof `H(A∥clientProof∥K)` (`PS/src/srp/core.rs:141-206,260-280`; diagram `core.rs:26-40`).
3. `POST /auth/v4` with `{"Username","ClientEphemeral","ClientProof","SRPSession"}`; `AuthReq` also embeds `Auth2FAReq`, so TOTP/FIDO2 can be sent in the same call (`GPA/manager_auth.go:51-56`, `GPA/manager_auth_types.go:36-43`). Live errors: HTTP 422 Code 2001 for a bad session length, HTTP 422 Code 9001 for CAPTCHA [Live: `POST /api/auth/v4`, 2026-11-15].
4. Verify `ServerProof`: base64-decode and constant-time compare with the expected proof; abort on mismatch (`GPA/manager_auth.go:61-70`, `ErrInvalidProof` at `:14`; `PS/src/srp/client.rs:36-45`; `PC/src/srp.rs:1-30`). GPA can skip this only when `WithSkipVerifyProofs` is set (`GPA/option.go:57-68`); Bridge does not set it (`BR/internal/bridge/api.go:38-45`).
5. Success returns `Auth` (`GPA/manager_auth_types.go:45-56`): `UID, AccessToken, RefreshToken, ServerProof, Scope, 2FA, PasswordMode, UserID`. Live Code 1000 was not reachable without credentials.

## 4. Two-factor (TOTP, FIDO2) and `/auth/v4/2fa`

- The `/auth/v4` response carries `2FA.Enabled`: 1=TOTP, 2=FIDO2, 3=both (`GPA/manager_auth_types.go:69-80`); Bridge tests it as a bitmask (`BR/internal/bridge/user.go:222`).
- `POST /auth/v4/2fa` (`GPA/auth.go:9-13`) with `{"TwoFactorCode":"123456"}` or `{"FIDO2":{...}}` (`GPA/manager_auth_types.go:28-34,89-92`). It requires the pre-2FA access token: live without a token returns HTTP 401 `{"Code":401,"Error":"Invalid access token"}` [Live: `POST /api/auth/v4/2fa`, 2026-11-15].
- Wrong TOTP is reported as Code 8002 `PasswordWrong`, which Bridge treats as retryable (`BR/internal/frontend/grpc/service_methods.go:556-564`; `GPA/response.go:28`).
- Response contains `Scope` (`HYD/protonmail/auth.go:148-169`; `https://raw.githubusercontent.com/photuris/proton-mail-api/main/src/session.rs:328-336`). Tokens are not re-read by Bridge after 2FA (`BR/internal/frontend/grpc/service_methods.go:569-574`); Inference: 2FA upgrades scope, it does not rotate tokens.
- FIDO2: `2FA.FIDO2.AuthenticationOptions` is a WebAuthn `publicKey` object with `rpId`, `challenge` (array of ints) and `allowCredentials` (`BR/internal/fido/utils.go:40-77`). Client builds `clientDataJSON` `{type:"webauthn.get", challenge: base64url(challenge), origin:"https://"+rpId}`, then sends `AuthenticationOptions` (echo), `ClientData`, `AuthenticatorData`, `Signature` (standard base64) and `CredentialID` (array of ints) (`BR/internal/fido/utils.go:92-108`). Registered keys are `{AttestationFormat, CredentialID, Name}` (`GPA/manager_auth_types.go:58-67`).

## 5. Human verification / CAPTCHA

- Trigger: Code 9001 `HumanVerificationRequired` with `Details` = `{HumanVerificationMethods:[…], HumanVerificationToken, Direct, Description, Title, WebUrl, ExpiresAt}` (`GPA/hv.go:11-15,57-59`). Live on `/auth/v4`: HTTP 422, methods `["captcha"]`, `WebUrl: https://verify.proton.me/?methods=captcha&token=…`, `ExpiresAt` ≈ 30 min after issue [Live: `POST /api/auth/v4`, 2026-11-15].
- Retry the same request with `x-pm-human-verification-token: <token>` and `x-pm-human-verification-token-type: <methods comma-joined>` (`GPA/hv.go:20-27`, `GPA/manager_auth.go:140-141`). Live: retrying with an unsolved token returns HTTP 422 `{"Code":12087,"Error":"CAPTCHA validation failed"}` [Live: `POST /api/auth/v4`, 2026-11-15].
- Client handling: Bridge extracts the details (`BR/internal/hv/hv.go:33-45`), stores them, and opens `hv.FormatHvURL` = `https://verify.proton.me/?methods=…&token=…` in the browser/CLI (`BR/internal/hv/hv_default.go:29-33`, `BR/internal/frontend/grpc/service.go:648-664`). GPA exposes the legacy image endpoint `GET /core/v4/captcha?Token=…&ForceWebMessaging=1` (`GPA/manager_user.go:7-14`). A third-party Python client solves the CAPTCHA programmatically via `core/v4/captcha`, `captcha/v1/api/init|bg|validate`, sends a `pcaptcha` header, and submits a composite `{token}:{sub_token}{proved}` value (`https://raw.githubusercontent.com/opulentfox-29/protonmail-api-client/master/src/protonmail/client.py:1129-1151`).
- HV can also be returned by non-login calls (Bridge handles it after login/user fetch, `BR/internal/frontend/grpc/service.go:464-476`). Third-party docs state the verification token is bound to the issuing IP (`https://pkg.go.dev/github.com/Arsylk/proton-auth`). Unverified: exact CAPTCHA provider/API stability.

## 6. Access/refresh tokens, UID, scopes, refresh, logout

- `UID` is the session id sent as `x-pm-uid`; `AccessToken` is the bearer token (`GPA/client.go:163-169`). `Scope` is a space-separated string; old examples are `"full"` and `"reset"` (`protonmail-api.yaml:1083,1255`; `https://raw.githubusercontent.com/photuris/proton-mail-api/main/src/session.rs:253-255`).
- Refresh: `POST /auth/v4/refresh`, body `{UID, RefreshToken, ResponseType:"token", GrantType:"refresh_token", RedirectURI:"https://protonmail.ch", State}` (`GPA/manager_auth.go:109-138`, `GPA/manager_auth_types.go:94-102`). Response is a new `Auth` (new access + refresh tokens; `UID` may be omitted and the old one kept, `https://raw.githubusercontent.com/photuris/proton-mail-api/main/src/http.rs:334-377`). Live with a bogus token: HTTP 422 `{"Code":10013,"Error":"Invalid refresh token"}` [Live: `POST /api/auth/v4/refresh`, 2026-11-15].
- Refresh failure policy: HTTP 400/422 → de-auth; 409/429/500/503 → transient (`GPA/client.go:183-202`). Bridge clears stored auth on Code 10013 (`BR/internal/bridge/user.go:458-470`).
- Lifetime: the old spec shows `ExpiresIn: 360000` (`protonmail-api.yaml:1081,1253`) and HYD treats it as seconds (`HYD/protonmail/auth.go:101-108`). Third-party measurements report 30 min–24 h per session (`https://pkg.go.dev/github.com/Arsylk/proton-auth`). GPA and the Rust third-party client ignore `ExpiresIn` and refresh on 401 (`GPA/client.go:174-213`; `photuris …/src/http.rs:415-455`). Unverified: current exact access-token lifetime and refresh-token lifetime; refresh token is long-lived and revocable (Bridge reloads it across launches, `BR/internal/bridge/user.go:458-474`).
- Logout: `DELETE /auth/v4`; list `GET /auth/v4/sessions`; revoke one `DELETE /auth/v4/sessions/{uid}`; revoke others `DELETE /auth/v4/sessions` (`GPA/auth.go:15-45`). Session list fields: `UID, CreateTime, ClientID, MemberID, Revocable, LocalizedClientName` (`GPA/manager_auth_types.go:104-113`).

## 7. Mailbox password, salts, key passphrase

- `PasswordMode`: 1 = one-password (login password also unlocks keys), 2 = two-password (separate mailbox password) (`GPA/manager_auth_types.go:82-87`). Bridge uses the login password in mode 1 and prompts for a mailbox password in mode 2 (`BR/internal/bridge/user.go:235-248`).
- `GET /core/v4/keys/salts` (authenticated) → `{Code:1000, KeySalts:[{ID, KeySalt}]}` (`GPA/salt.go:7-20`, `GPA/salt_types.go:11-15`; live unauthenticated → HTTP 401 `{"Code":401,"Error":"Invalid access token"}` [Live: `GET /api/core/v4/keys/salts`, 2026-11-15]). `KeySalt` is base64 of 16 bytes and can be null/empty for old keys (`PCA/src/salts.rs:71-97`; `https://raw.githubusercontent.com/photuris/proton-mail-api/main/src/keys.rs:24,119-126`).
- Derivation: `bcrypt(mailbox_password, key_salt)` cost 10, `$2y$`, then drop the 29-char `$2y$10$<22-char-salt>` prefix, leaving the 31-char hash, used as the OpenPGP passphrase (`PS/src/pmhash.rs:24-59`; `PCA/src/salts.rs:99-112`; `GSRP/hash.go:50-83,76-83`; `HYD/protonmail/password.go:11-14,55-63`). If a key has no salt, the mailbox password itself is used (`https://raw.githubusercontent.com/photuris/proton-mail-api/main/src/keys.rs:362-373`). Address keys may carry an encrypted `Token` + detached `Signature`, decrypted with the user keys (`GPA/unlock.go:11-35`; `photuris …/src/keys.rs:376-400`).
- Important scope quirk: salts are granted only briefly after login; a resumed session gets Code 9101 / HTTP 403 from `/core/v4/keys/salts` hours later (`https://raw.githubusercontent.com/photuris/proton-mail-api/main/AGENTS.md:68-69`; `…/src/keys.rs:240-248`). Inference: persist the derived key password (and optionally salts) at login to avoid this.

## 8. TLS pinning and alternative routing

- GPA does **not** pin: its default transport is `http.DefaultTransport` (`GPA/manager_builder.go:35`) and no pinning code exists in the repo. Bridge pins the leaf certificate SPKI SHA-256 for `api.protonmail.ch`, `protonmail.com`, `proton.me` and for proxies (same pins for all proxies), and reports mismatches to `https://reports.proton.me/reports/tls` (`BR/internal/dialer/dialer_pinning.go:26-53`; fingerprint computation `dialer_pinning_checker.go:30-40`; leaf check `dialer_pinning_checker_default.go:26-60`). Bridge builds a pinning dialer and wraps it in a proxy dialer (`BR/internal/app/bridge.go:76-85`).
- Live verification: leaf SPKI of `mail.proton.me`/`mail-api.proton.me` = `CT56BhOTmj5ZIPgb/xD5mH8rY3BLo/MlhP7oPyJUEDo=` and of `api.protonmail.ch` = `drtmcR2kFkM8qJClsuWgUzxgBkePfRCkRpqUesyDmeE=`, matching Bridge's "current" pins; `mail.proton.me` also sends `public-key-pins-report-only` with the same proton.me pins [Live: `openssl s_client` + `GET /api/auth/v4/modulus`, 2026-11-15].
- Fallback/alternative routing: if the direct TLS dial fails, Bridge switches to a reachable server for 24 h (`BR/internal/dialer/dialer_proxy.go:35,86-107`). Candidates come from DoH TXT lookup of `dMFYGSLTQOJXXI33ONVQWS3BOMNUA.protonpro.xyz` via Quad9 (`dns11.quad9.net`, port 5053) and Google (`dns.google`) (`BR/internal/dialer/dialer_proxy_provider.go:34-51,205-266`), and reachability is checked with `GET /tests/ping` (`:184-195`). Inference: Sweetbriar should pin at least the proton.me SPKI set and implement a similar proxy fallback if target users may be blocked.

## 9. Rate limiting, 429/503, Retry-After, "jail"

- 429 and 503 are retryable; parse `Retry-After`, else default 10 s, add jitter; GPA: 3 retries, max wait 60 s (`GPA/response.go:145-176`, `GPA/manager_builder.go:87-93`). Bridge: exponential 20/40/80/160/300/600 s + ≤10 s jitter on 429/5xx (`BR/internal/network/proton.go:28-60,72-105`). Rust third-party: 3 retries, 10 s default, 2 s jitter, 60 s cap (`photuris …/src/http.rs:24-28,490-535`).
- Anti-abuse: Code 2028 is Proton's anti-abuse block ("Too many recent logins"); stop retrying and wait (`https://raw.githubusercontent.com/photuris/proton-mail-api/main/README.md:39`; `…/src/http.rs:543-572`; Python client `client.py:1201-1206`). Bridge's changelog records "Avoid API jail" (`BR/Changelog.md:1853`), but no literal "jail" code exists in any reference repo. Unverified: HTTP status that carries 2028 and whether "jail" maps to 2028/9001/429.
- Other codes: 9002 device verification (`photuris …/src/http.rs:568`; unverified live), 9101 HTTP 403 insufficient scope for salts (`photuris AGENTS.md:68-69`), 10013 invalid refresh token (live), 2064/5002/5003 app-version errors (live), 12087 invalid CAPTCHA token (live).

## 10. What to persist, and what is secret

- Bridge stores `AuthUID`, `AuthRef` (refresh token) and `KeyPass` (derived salted key password) per user in an AES-GCM vault (`BR/internal/vault/types_user.go:30-45`, `BR/internal/vault/user.go:124-155`, `BR/internal/vault/vault.go:40-52,59-77`); the vault key comes from the OS keychain (`BR/internal/app/vault.go:44-115`). Cookies are persisted too (`BR/internal/vault/cookies.go:20-28`). On launch it calls `NewClientWithRefresh(uid, ref)` and stores the new refresh token on every auth update (`BR/internal/bridge/user.go:458-474`; `BR/internal/user/user.go:300-306`).
- GPA: `NewClientWithRefresh(ctx, uid, ref)` refreshes at startup (`GPA/manager_auth.go:20-29`); clients must keep `uid` + refresh token and update both after refresh (`GPA/client.go:205-210`).
- Rust third-party `PersistedSession` = `{uid, access_token, refresh_token, password_mode, scope, key_password}` (`https://raw.githubusercontent.com/photuris/proton-mail-api/main/src/session.rs:413-421`).
- Secrets: refresh token, access token, derived key password, mailbox/login password (if stored), HV token (short-lived). Non-secret but session/account data: `UID`, user id, salts, SRP salt/modulus, addresses/keys, cookies. Inference: Sweetbriar should keep secrets in macOS Keychain, persist `UID` + refresh token + key password (+ salts if desired), and refresh on 401/at launch.

## Open questions

- Exact current access-token lifetime (`ExpiresIn` unit/values) and refresh-token lifetime/rotation semantics; whether refresh tokens survive password changes.
- Exact scope strings granted for a mail client and whether 2FA response rotates tokens.
- Whether Code 2028's HTTP status is 200/429/other, and what Proton internally calls "jail".
- Live behavior of Code 8002 (wrong password), 9002 (device verification), 9101 (salts on resume), 9001 on authenticated endpoints; all need a real account.
- Whether `Other` is accepted for all authenticated endpoints (only unauthenticated endpoints were probed) and whether Proton would register a Sweetbriar product name.
- Whether the HV token is truly IP-bound and whether the `verify.proton.me` flow sets cookies required later.
- Whether `Session-Id`/`Tag` cookies are required for any mail endpoint, and whether `PersistentCookies:1` is needed.
- Current minimum accepted versions per product (5003 threshold) and whether Bridge's pin set is complete for Sweetbriar's chosen host.
