# Import fixtures

Synthetic messages only. `.test` addresses do not route to owner mail.

- `quoting.mbox`: requires a variant choice. `expected/mboxo.eml` and
  `expected/mboxrd.eml` are its exact decoded bytes; the tests pin their
  SHA-256 hashes. Only mbox `From ` escaping changes bytes.
- `Inbox.mbox/`: Apple Mail export shape, with two byte-identical messages.
  Both separators remain candidates. `Info.plist` is metadata, not mail.

Mbox ranges exclude envelope separator lines and retain all other bytes,
including blank lines and line endings. Byte quoting cannot identify the
variant when `>>From ` lines exist: both interpretations are possible.
