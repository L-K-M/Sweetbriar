# Test certificate

`imap-localhost.pem` and `imap-localhost-key.pem` are a synthetic, self-signed
pair for the fake IMAP server's STARTTLS on `127.0.0.1` (spec/04's Bridge
model). The private key is public on purpose. Never use it outside the test
harness, and never copy this pattern for a real credential.
