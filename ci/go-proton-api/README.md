# The `go-proton-api` lane (M2.17)

A CI-only driver that runs Proton's development API server (`github.com/ProtonMail/go-proton-api/server`), seeds one synthetic account, and prints one JSON line the Rust lane test reads (`crates/sb-service/src/accounts/proton_tests/lane.rs`). Nothing here is linked into the app, and nothing contacts the live Proton API (spec/11).

## Pins

- `go-proton-api` commit [`79fbe0626d9b737a5a2b447e1f66e4bee4717763`](https://github.com/ProtonMail/go-proton-api/commit/79fbe0626d9b737a5a2b447e1f66e4bee4717763) (2026-11-02), pseudo-version `v0.0.0-20261102101905-79fbe0626d9b`.
- Go `go1.26.10`, pinned by `go.mod` (the server asks for `go 1.26.1` / `toolchain go1.26.10`); `go.sum` is committed.

## Licence

The driver is Sweetbriar's own code, `Unlicense OR 0BSD`. It depends on `go-proton-api` (MIT); the `replace` of `go-resty/resty/v2` with Proton's fork is the server's own, carried here because a dependency's `replace` applies only to the main module.

## Running locally

```sh
cd ci/go-proton-api && go build -o /tmp/gpa-driver .
/tmp/gpa-driver -request-log /tmp/gpa-requests.log > /tmp/gpa-line.json &
SB_GPA_LANE=/tmp/gpa-line.json SB_GPA_LANE_SUMMARY=/tmp/gpa-summary.json \
  cargo test -p sb-service --lib lane -- --nocapture
```

The JSON line carries the base URL, the lane certificate as PEM, the synthetic username and password, the modulus public key, the SRP salt length, the per-label counts, and the seeded messages.

## Known differences from production

- **TLS**: the server's httptest certificate is a self-signed `CA:TRUE` certificate, which rustls refuses as an end entity (`CaUsedAsEndEntity`); the driver terminates the lane's TLS with an ephemeral `CA:FALSE` certificate for `127.0.0.1` and proxies to the server's own TLS.
- **SRP salt length**: the server sends a 16-byte SRP salt; production's is 10 (`proton-srp`'s `SALT_LEN_BYTES`), and Sweetbriar's client refuses any other length, so the lane cannot sign in and stops with a printed note. This is the lane's blocker.
- **`Code`**: the server omits `Code` from success responses; production puts `1000` on every success, which Sweetbriar's client requires.
- **Key `Version`**: `/core/v4/users` and `/core/v4/addresses` key objects carry no `Version`; production includes it.
- **`Address.Send`/`Receive`**: the server sends Proton's `0`/`1` (`APIBool`); Sweetbriar's `Address` deserializer expects a JSON boolean, so the restore path fails. A Sweetbriar-side finding the lane is meant to catch; the driver does not paper over it.
- **Modulus key**: the server signs its SRP modulus with Proton's real modulus key (`proton@srp.modulus`, fingerprint `248097092B458509C508DAC0350585C4E9518F26`), so the production `RPGPVerifier` verifies it; no lane-only verifier is needed.
- **App version**: the server requires `x-pm-appversion` but validates its `name_semver` shape only once a minimum version is set, so Sweetbriar's honest `Other` passes; its missing-header code is 5001 where production's is 5002.
- **All Mail**: the server never assigns the All Mail label (`"5"`), so the engine's All Mail mailbox stays empty; the fake assigns it.
- **Message paging**: `GET /mail/v4/messages` ignores `Sort`, `Desc`, `End` and `EndID`, paging by `Page`/`PageSize` in insertion order; the lane's few messages fit one short page.
- **Import labels**: the importer rejects All Mail as a label and allows at most one non-user label, so a message cannot be imported into a folder and Starred together; the driver imports it, then stars it with `PUT /mail/v4/messages/label`.

## Lane status

The driver seeds and serves correctly. The lane test stops at the SRP salt difference, so against this commit it records a skipped summary rather than coverage counts. The suite's assertions are in place; the server's remaining differences (the missing `Code`, the key `Version`, and `Address.Send`/`Receive`) still block it after the salt.
