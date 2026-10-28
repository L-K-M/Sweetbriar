# Sweetbriar

> [!IMPORTANT]
> LLM disclosure: This codebase was written with substantial help from large language models: AI coding agents working from the [`AGENTS.md`](AGENTS.md) brief in this repo.

**Latest release:** v<!-- version -->0.0.1<!-- /version --> · [Download](https://github.com/L-K-M/Sweetbriar/releases/latest)

A native, fast email client with a shared Rust core: macOS first, Linux
later.

- [research/](research/00-summary.md): the foundation research and its
  conclusions.
- [spec/](spec/README.md): the specification the implementation is built
  against.
- [plan/](plan/README.md): the step-by-step implementation plan.

## Build and install

On an Apple silicon Mac with Xcode, [rustup](https://rustup.rs) and
XcodeGen (`brew install xcodegen`):

```sh
scripts/build.sh            # build Sweetbriar.app into dist/
scripts/build.sh --install  # also install it into /Applications
```

`--install` quits a running Sweetbriar first and reopens it afterwards.
The first time, macOS asks whether your terminal may control Sweetbriar
so it can ask it to quit. `SWEETBRIAR_APPLICATIONS_DIR=~/Applications`
installs there instead. `scripts/build.sh --help` lists the other
options. Builds are signed ad hoc, not notarized.

## Licence

Sweetbriar's own code is available under the [Unlicense](LICENSE)
(public domain) or [0BSD](LICENSE-0BSD), your choice. 0BSD covers places
where a public-domain dedication may not be recognised. The project
doesn't accept contributions; you're free to fork it.

The macOS app bundles third-party Rust crates, SQLite (public domain)
and the Rust standard library, which keep their own licences. Some of
the crates, among them UniFFI, `cssparser` and `selectors`, are under
the MPL-2.0; [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) names each
crate's version and source. The notices ship in
`Sweetbriar.app/Contents/Resources` and at the DMG root as
`THIRD_PARTY_NOTICES.md`, which cargo-about generates from `Cargo.lock`
and CI checks for drift, and `Rust-Licenses.html`, the Rust standard
library's notices from the pinned toolchain.
