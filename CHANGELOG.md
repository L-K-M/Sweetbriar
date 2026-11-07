# Changelog

## Unreleased

- macOS bundle identifiers moved from `com.sweetbriar.*` to the `ch.lkmc`
  namespace: the app is now `ch.lkmc.Sweetbriar` (command namespace,
  signpost subsystem and the macOS verifier xattr follow). The new id is
  a new app identity to macOS — TCC grants and app-group state do not
  carry over.
- `scripts/build.sh` builds the macOS app into `dist/`; `--install` also
  replaces `/Applications/Sweetbriar.app`, quitting and reopening a
  running copy. Earlier history lives in the commit log.
