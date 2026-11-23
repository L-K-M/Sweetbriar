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
- **Proton accounts** (04-sync-proton): the Proton password, the mailbox
  password and 2FA codes are entered at sign-in only and are never
  stored. They live in zeroizing buffers (`secrecy`) and are dropped as
  soon as the SRP proofs and the key passphrase exist. The key
  passphrase is derived from the password that unlocks the keys (the
  Proton password in one-password mode, the mailbox password in two)
  with Proton's key salt: it can't sign in, but it unlocks the keys,
  so it is guarded like a password. The secret store holds one item per
  account: the session UID, the refresh token and the key passphrase.
  Access tokens stay in memory only. Besides its copy in the secret
  item, the key passphrase is also kept in memory, in a zeroizing
  buffer, for as long as the account's session lasts, so address, user
  and key events can unlock the keys again (04-sync-proton).
- **Proton private keys** are unlocked in memory from the key passphrase
  and are never written anywhere, decrypted or not; Proton's encrypted
  key blobs are fetched again on each start. Decrypted key material is
  zeroized on drop.
- **A Keychain error puts the account in `needs_attention`** (04-sync-imap,
  04-sync-proton). It
  never deletes or recreates
  anything. Logs redact anything that looks like a token or password; a
  CI test scans for this.

## The Proton API

- **TLS:** connections to the Proton API host use `rustls` with the
  platform verifier (the system trust store, hostname checked). Pinning
  Proton's keys is *later*: a stale pin would lock the owner out. A
  certificate error is never accepted.
- **SRP:** the modulus must carry a valid signature from the Proton
  modulus key embedded in `proton-srp`, and the server proof must match
  before any token is used (04-sync-proton).
- **Honest identity:** every request sends `x-pm-appversion: Other` and
  `User-Agent: Sweetbriar/<version> (macOS <version>)`. Sweetbriar never
  sends another client's identity, even when Proton refuses Sweetbriar's;
  the refusal is shown to the user (04-sync-proton). A test asserts both headers on
  every request the fake server sees.
- **Human verification** opens Proton's verification page in a
  non-persistent `WKWebView` (08) that loads only `verify.proton.me`
  and Proton's CAPTCHA providers' hosts, shares no cookies with
  anything else and is torn down afterwards. Sweetbriar reads only the
  returned token. A challenge met during sync is held in memory only,
  like a sign-in conversation, and its token rides one request
  (04-sync-proton "Human verification during sync").
- **Decryption:** bodies and attachments are decrypted with
  `proton-crypto` (01) in the core, never in the UI. Decrypted messages
  are stored like any other original (03), so the store holds
  plaintext. FileVault covers it at rest (Privacy, below), as with
  Bridge before.
- **Signatures:** a body's signature is verified against the sender's
  keys when Proton supplies them. The pilot records the result for
  diagnostics only; showing it, like `Authentication-Results`, needs a
  trust design first (*later*).
- **Never logged:** passwords, tokens, the key passphrase, key material,
  decrypted content, the human-verification token, the session UID.
  The log-redaction CI test (Credentials) covers the Proton transport.

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
  mismatch the account goes to `needs_attention` with a clear message,
  for example after a Bridge reinstall. Pins are never updated silently.
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
    `sweetbriar-cid:` and records and removes remote **resource** URLs
    (`img`, `srcset`, CSS, fonts, media; this count feeds
    `remote_content_blocked`), then `ammonia` sanitizes with an
    allow-list. Hyperlinks (`a href`) to `http`, `https` and `mailto` are
    kept as user-activated links;
  - no scripts, forms, iframes, objects, `<base>` or meta refresh;
  - CSS is restricted, with no `url()` except `sweetbriar-cid:` and no
    loading at-rules (`@import`, `@font-face`, `@namespace` with a URL).
    `style` elements are dropped. An inline `style` keeps, declaration
    by declaration, only these properties, parsed with `cssparser`:
    `color`, `background-color`, `font-family`, `font-size`,
    `font-weight`, `font-style`, `text-align`, `text-decoration`,
    `text-indent`, `line-height`, `vertical-align`, `white-space`,
    `list-style-type`, `margin` and `padding` with their sides,
    `border` with its sides and its `-width`, `-style` and `-color`
    parts, `border-collapse`, `border-spacing`, `width`, `min-width`,
    `max-width`, `height`, and `background-image` holding only a minted
    `sweetbriar-cid:` URL. Values are keywords, numbers, lengths and
    percentages, colors (`#hex`, names, `rgb()`, `rgba()`, `hsl()`,
    `hsla()`) and font names; any other function, `!important`, a
    custom property or `var()` drops the declaration, and the rest of
    the style stays;
  - `cid:` URLs resolve only within the body's own multipart/related
    part, never by path. The renderer mints
    `sweetbriar-cid://<lease token>/<resource id>`; a sender's own
    `sweetbriar-cid:` URLs are removed first. Only PNG, JPEG, GIF and
    WebP whose bytes match are served; other parts stay attachments;
  - `remote_content_blocked` counts each removed network-capable
    resource URL once (each `src`, `srcset` candidate, CSS URL and
    link hint), relative ones included, saturating with a warning.
    Hyperlinks don't count;
  - each link's destination is the anchor's title, and the manifest
    (07) records its warnings per destination: link text naming another
    domain (`text_domain`), a raw IP (`raw_ip`), user info
    (`user_info`); hosts are compared parsed, not as strings.
  - The `WKWebView` setup in 08 adds a second, independent layer.
- **Attachments:** the user opens them explicitly. Files are written to a
  per-lease cache directory with the quarantine attribute set, so
  Gatekeeper applies.
- **Trust display:** `Authentication-Results` is not shown for Proton
  accounts in the pilot, through Bridge or the API. Proton's own
  verification and the API's signature results need a trust design
  first.

## Privacy

- **No telemetry.** Nothing leaves the machine except mail protocol
  traffic to configured servers, and for Proton accounts the API
  traffic to Proton and, during human verification, the verification
  page's own traffic.
- **Diagnostics:** a diagnostics package is assembled on demand with
  redaction (tokens, passwords, addresses and subjects masked). The user
  previews it before saving (07 `diagnostics_package`, M2.7).
  - It is one UTF-8 JSON document: the app and core versions, the OS
    version, the store's schema version and integrity state, and per
    account its id, kind, state, reason, condition (07
    `account_health`), open breaker events with counts, parked and
    quarantined intent counts, coverage counts, and for a Proton
    account its last refusal (`diagnostics_json`, 02), listing state
    and refresh count. It holds no message content, no log lines, no
    file paths under the home folder and no display names.
  - Redaction happens when a value is recorded and again when the
    package is assembled: anything shaped like an address becomes
    `<address>`, and refusal messages, which may quote one, pass the
    same mask. Tokens, passwords, key material and session ids are
    never read into it.
  - A log-scan test seeds every secret, address and subject the
    fixtures know and finds none of them in the package.
- **Encryption at rest (pilot):** relies on FileVault, stated in
  Settings. App-level encryption is a later decision, made from a threat
  model (D24). The object-store trait and SQLite open path keep a codec
  seam.
- **Spotlight:** Sweetbriar doesn't index mail into Spotlight in the
  pilot.
- **External editor files (08, M4.4e4):** a draft body handed to an
  external editor is the only draft content written outside the store.
  It goes in a new directory under the cache root's `external-edit/`,
  `0700` and named by a random UUID; the `0600` file inside is named by
  the draft's type (`draft.md` or `draft.txt`), never by subject. The
  shell deletes it when the edit ends (not after an unreadable final
  read the user hasn't discarded, 08), and deletes whatever a crash left
  there at the next launch without reading it. The cache root is
  excluded from Time Machine (03).
- **Notifications:** show sender and subject only if the user enables
  previews (`notifications.previews`, 02). They name the account in
  text. Only messages that arrive in an account's Inbox through the
  event loop notify; a bootstrap, a refresh or a resumed account's
  resync never does (07 `subscribe_new_mail`, M2.7).

## Supply chain

- **Dependency checks:** `cargo-deny` checks licences and advisories on
  every PR (11), using the licence policy from
  [spec/01](01-architecture.md#licence-policy-for-dependencies).
- **Locking:** `Cargo.lock` and `Package.resolved` are committed.
  Dependency updates are their own PRs.
