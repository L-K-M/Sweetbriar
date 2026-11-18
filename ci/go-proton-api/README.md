# The `go-proton-api` lane (M2.17)

A CI-only driver that runs Proton's development API server (`github.com/ProtonMail/go-proton-api/server`), seeds one synthetic account, and prints one JSON line the Rust lane test reads (`crates/sb-service/src/accounts/proton_tests/lane.rs`). Nothing here is linked into the app, and nothing contacts the live Proton API (spec/11).

## Pins

- `go-proton-api` commit [`79fbe0626d9b737a5a2b447e1f66e4bee4717763`](https://github.com/ProtonMail/go-proton-api/commit/79fbe0626d9b737a5a2b447e1f66e4bee4717763) (2026-11-02), pseudo-version `v0.0.0-20261102101905-79fbe0626d9b`.
- Go `go1.26.10`, pinned by `go.mod` (the server asks for `go 1.26.1` / `toolchain go1.26.10`); `go.sum` is committed.

## Licence

The driver is Sweetbriar's own code, `Unlicense OR 0BSD`. It depends on `go-proton-api` (MIT); the `replace` of `go-resty/resty/v2` with Proton's fork is the server's own, carried here because a dependency's `replace` applies only to the main module.

## Running locally

```sh
bash ci/go-proton-api/build.sh -o /tmp/gpa-driver
/tmp/gpa-driver -request-log /tmp/gpa-requests.log > /tmp/gpa-line.json &
SB_GPA_LANE=/tmp/gpa-line.json SB_GPA_LANE_SUMMARY=/tmp/gpa-summary.json \
  cargo test -p sb-service --lib lane -- --nocapture
```

`build.sh` downloads the pinned module, applies `patches/srp-salt-10.patch`
to a temporary copy, and builds the driver against that copy through a
temporary `-modfile`; the committed `go.mod`/`go.sum` keep pinning the
upstream commit.

The JSON line carries the base URL, the lane certificate as PEM, the synthetic username and password, the modulus public key, the SRP salt length, the per-label counts, and the seeded messages.

## Known differences from production

- **TLS**: the server's httptest certificate is a self-signed `CA:TRUE` certificate, which rustls refuses as an end entity (`CaUsedAsEndEntity`); the driver terminates the lane's TLS with an ephemeral `CA:FALSE` certificate for `127.0.0.1` and proxies to the server's own TLS.
- **SRP salt length**: the server hard-codes a 16-byte SRP salt; production's is 10 (`proton-srp`'s `SALT_LEN_BYTES`), and Sweetbriar's client refuses any other length. `build.sh` applies `patches/srp-salt-10.patch`, which keeps the 16-byte key salt bcrypt needs and generates a separate 10-byte SRP salt.
- **`Code`**: the server omits `Code` from success responses; production puts `1000` on every success, which Sweetbriar's client requires. The lane proxy adds it to 2xx JSON object responses that lack one; raw bodies such as attachment packets pass through untouched.
- **Key `Version`**: `/core/v4/users` and `/core/v4/addresses` key objects carry no `Version`; production includes it, and `proton-crypto-account`'s `LockedKey` requires the field. The lane proxy adds production's value (`"Version": 3`) to key objects that lack it.
- **Null address lists**: the server marshals absent `ToList`/`CCList`/`BCCList`/`ReplyTos` as `null`; production sends empty arrays, and Sweetbriar's metadata deserializer expects a sequence. The lane proxy rewrites the nulls in `/mail/v4/messages`.
- **Per-label counts**: the server's `/mail/v4/messages/count` counts only system labels, so user-label totals are missing; production counts every label. The server patch counts the user labels too.
- **`Address.Send`/`Receive`**: fixed on the Sweetbriar side (commit `2446c9eb`): `Address` now decodes Proton's `0`/`1` (`APIBool`).
- **Modulus key**: the server signs its SRP modulus with Proton's real modulus key (`proton@srp.modulus`, fingerprint `248097092B458509C508DAC0350585C4E9518F26`), so the production `RPGPVerifier` verifies it; no lane-only verifier is needed.
- **App version**: the server requires `x-pm-appversion` but validates its `name_semver` shape only once a minimum version is set, so Sweetbriar's honest `Other` passes; its missing-header code is 5001 where production's is 5002.
- **All Mail**: the server lists the All Mail label (`"5"`) on every message and counts it, while the driver's seeded counts don't include it; the lane does not compare an All Mail total.
- **Message paging**: `GET /mail/v4/messages` ignores `Sort`, `Desc`, `End` and `EndID`, paging by `Page`/`PageSize` in insertion order; the lane's few messages fit one short page.
- **Import labels**: the importer rejects All Mail as a label and allows at most one non-user label, so a message cannot be imported into a folder and Starred together; the driver imports it, then stars it with `PUT /mail/v4/messages/label`.

## Lane status

The lane runs green. The driver signs in, the test asserts the bootstrap listing completes with 6 messages, per-label local counts and server totals match the seeded expectations, the multi-label message is one logical message with one occurrence per asserted label (plus the All Mail occurrence the server adds), and the request log shows no mutation. The nightly job records the pass summary and logs as a dated artifact.
