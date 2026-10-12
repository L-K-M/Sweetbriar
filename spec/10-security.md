# 10 — Security and privacy

Rests on research [04-data-safety](../research/04-data-safety.md)
(credentials and hostile content), [07-standards](../research/07-standards.md)
and D24, D29.

## Credentials

- **Where secrets live:**
  - On macOS: the data-protection Keychain, with
    `kSecUseDataProtectionKeychain=true` on every query,
    `kSecAttrAccessibleWhenUnlockedThisDeviceOnly` and
    `kSecAttrSynchronizable=false`.
  - **Pilot caveat:** the data-protection Keychain needs an
    entitlement-signed app. Until the owner has a signing identity (plan
    O8), ad-hoc builds may fall back to a file-based Keychain item. The
    fallback is flagged in Settings and removed in plan M6.5.
  - On Linux: the Secret Service.
- **The database stores only a `credential_ref`** (02). Secrets are never
  written to files, logs, crash reports or exports.
- **The Bridge password** (Bridge generates it; it is not the Proton
  password) is entered once at account setup and stored as above.
- **A Keychain error pauses the account.** It never deletes or recreates
  anything. Logs redact anything that looks like a token or password; a
  CI test scans for this.

## TLS and the Bridge certificate

- **Connections** to Bridge use STARTTLS or TLS as Bridge is configured,
  through `rustls`.
- **Bridge's self-signed certificate is pinned at setup.**
  - **Preferred:** the user exports it from Bridge (Settings → Advanced →
    Export TLS certificates, which writes `cert.pem` and `key.pem`) and
    picks `cert.pem`. Sweetbriar never reads `key.pem`.
  - **Fallback:** Sweetbriar shows the presented certificate's SHA-256
    fingerprint for the user to confirm.
  - The DER SHA-256 is stored in `imap_endpoints.trusted_cert_sha256`.
    Trust is scoped to that account's endpoints and never added to the
    system trust store.
  - Connections always use the literal `127.0.0.1`: the certificate's
    IP SAN is `127.0.0.1`, with no `localhost` name.
- **Later connections** require an exact match with that pin. On a
  mismatch the account pauses with a clear message, for example after a
  Bridge reinstall. Pins are never updated silently.
- **Loopback only:** Bridge connections go to `127.0.0.1` only. `::1`
  isn't used: Bridge's certificate names only the IPv4 address.
- **Generic IMAP** (*later*) uses the platform verifier. Certificate
  errors are never accepted silently.

## Hostile content

- **Parsing:** MIME parsing runs with limits:
  - nesting depth 50;
  - 100 MB per message and 25 MB per decoded part;
  - 10,000 parts.
  - Exceeding a limit yields a `partial` parse with a visible warning.
- **Panics:** release builds use `panic = "unwind"`, with `catch_unwind`
  at each FFI export and around the parser. A parser panic marks that
  message `failed` and never crashes the app.
- **Fuzzing:** `cargo-fuzz` targets cover the MIME parser entry points,
  the mbox splitter and the query parser. They run nightly in CI.
- **Rendering:**
  - sanitized HTML: `lol_html` first rewrites `cid:` URLs to
    `sweetbriar-cid:` and records and removes remote URLs (this count
    feeds `remote_content_blocked`), then `ammonia` sanitizes with an
    allow-list;
  - no scripts, forms, iframes, objects, `<base>` or meta refresh;
  - CSS is restricted, with no `url()` except `sweetbriar-cid:`.
  - The `WKWebView` setup in 08 adds a second, independent layer.
- **Attachments:** the user opens them explicitly. Files are written to a
  per-lease cache directory with the quarantine attribute set, so
  Gatekeeper applies.
- **Trust display:** `Authentication-Results` is not shown for Bridge
  accounts in the pilot. Proton's own verification isn't exposed through
  Bridge in a form Sweetbriar can trust.

## Privacy

- **No telemetry.** Nothing leaves the machine except mail protocol
  traffic to configured servers.
- **Diagnostics:** a diagnostics package is assembled on demand with
  redaction (tokens, passwords, addresses and subjects masked). The user
  previews it before saving.
- **Encryption at rest (pilot):** relies on FileVault, stated in
  Settings. App-level encryption is a later decision, made from a threat
  model (D24). The object-store trait and SQLite open path keep a codec
  seam.
- **Spotlight:** Sweetbriar doesn't index mail into Spotlight in the
  pilot.
- **Notifications:** show sender and subject only if the user enables
  previews. They name the account in text.

## Supply chain

- **Dependency checks:** `cargo-deny` checks licences and advisories on
  every PR (11), using the licence policy from
  [plan/README](../plan/README.md).
- **Locking:** `Cargo.lock` and `Package.resolved` are committed.
  Dependency updates are their own PRs.
