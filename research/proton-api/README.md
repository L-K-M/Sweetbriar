# Proton API research (2026-11-15)

Gathered for the owner's decision of 2026-11-15: Sweetbriar talks to
Proton through its own implementation of the Proton API, not through
Proton Mail Bridge (plan/README.md, "Owner decisions, 2026-11-15").

Sources are Proton's MIT-licensed `go-proton-api`, `go-srp` and
`proton-crypto-rs`, the MIT-licensed third-party `hydroxide`, public
issue trackers and Proton's published terms. `proton-bridge` is
GPL-3.0: it was read for protocol facts only, and Sweetbriar copies no
code from it. Every claim cites a file and line or a URL; inferences
and unverified items are marked as such. A reviewer spot-checked the
label IDs, event actions, response codes and app-version defaults
against the cited sources.

Some findings come from unauthenticated probes of Proton's public API
endpoints (marked **Live**): no account, credential or owner data was
used. Future research and CI never probe the live API (spec/11).

| Report | Covers |
|---|---|
| [auth-session.md](auth-session.md) | Hosts and headers, `x-pm-appversion`, SRP login, 2FA, human verification, tokens, key passphrase, TLS, rate limits, what to persist |
| [keys-crypto.md](keys-crypto.md) | The key model (user and address keys, salts, tokens), body and attachment decryption, the send-package model, the Rust crates and their licences/advisories |
| [mail-sync.md](mail-sync.md) | Labels and folders, the message model, listing and paging, full messages and attachments, events, mutations, drafts and sending, RFC 822 reconstruction, import |
| [policy-risk.md](policy-risk.md) | Proton's terms, client identity, observed blocks, human verification, free versus paid plans, 2FA and FIDO2, the risk summary |

Raw notes were written by a LongCat 2.5 research helper and kept as
written, apart from local paths.
