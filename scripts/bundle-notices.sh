#!/usr/bin/env bash
# Put the app's license notices into its Resources folder and fail if any is
# missing or empty. Runs as an Xcode Run Script phase (no arguments) or from a
# SwiftPM bundle script, with the bundle's Contents/Resources as argument 1.
# Template from the shared license-notices recipe: copy it to scripts/ and
# edit only the settings block. Bash 3.2 compatible (macOS /bin/bash).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# --- Settings for this repository -----------------------------------------
# Product name for the notices heading.
TITLE="Sweetbriar"
# Generated file name; keep an existing name if the app already ships one.
NOTICES_NAME="THIRD_PARTY_NOTICES.md"
# Package.resolved files whose pins are linked into the app. Leave the array
# empty when the app has no Swift package dependencies.
# Sweetbriar has none: SweetbriarCore is a local package around the Rust
# XCFramework, whose crates THIRD_PARTY_NOTICES.md (cargo-about) covers.
RESOLVED=()
# Explicit overrides for swiftpm-notices.py; empty when none are needed.
OVERRIDES=""
# Builds started from Xcode don't inherit the shell PATH. rustup picks the
# pinned toolchain from the repository's rust-toolchain.toml: the one that
# built the XCFramework, whose standard library is statically linked into
# the app.
export PATH="$HOME/.cargo/bin:$PATH"
RUST_SYSROOT="$(cd "$ROOT" && rustc --print sysroot)"
# Files shipped verbatim, as SOURCE=NAME-IN-RESOURCES. Add the root LICENSE
# when it carries someone else's copyright (forks) or a finding asks for it.
COPY=(
  "$ROOT/THIRD_PARTY_NOTICES.md=THIRD_PARTY_NOTICES.md"
  "$ROOT/LICENSE=LICENSE"
  "$ROOT/LICENSE-0BSD=LICENSE-0BSD"
  "$RUST_SYSROOT/share/doc/rust/COPYRIGHT-library.html=Rust-Licenses.html"
)
# ---------------------------------------------------------------------------

RESOURCES="${1:-${TARGET_BUILD_DIR:?run from Xcode or pass the Resources dir}/${UNLOCALIZED_RESOURCES_FOLDER_PATH:?}}"
mkdir -p "$RESOURCES"
shipped=()

if [ -n "${RESOLVED[*]+x}" ] && [ -n "${RESOLVED[*]}" ]; then
  args=(--title "$TITLE" --output "$RESOURCES/$NOTICES_NAME")
  for resolved in "${RESOLVED[@]}"; do
    args+=(--resolved "$resolved")
  done
  if [ -n "$OVERRIDES" ]; then args+=(--overrides "$OVERRIDES"); fi
  # Set NOTICES_CHECKOUTS when building with -clonedSourcePackagesDirPath.
  if [ -n "${NOTICES_CHECKOUTS:-}" ]; then args+=(--checkouts "$NOTICES_CHECKOUTS"); fi
  python3 -I "$ROOT/scripts/swiftpm-notices.py" "${args[@]}"
  shipped+=("$RESOURCES/$NOTICES_NAME")
fi

for entry in ${COPY[@]+"${COPY[@]}"}; do
  src="${entry%%=*}"
  name="${entry#*=}"
  if [ ! -s "$src" ]; then
    echo "error: license file missing or empty: $src" >&2
    exit 1
  fi
  install -m 0644 "$src" "$RESOURCES/$name"
  shipped+=("$RESOURCES/$name")
done

if [ -z "${shipped[*]+x}" ]; then
  echo "error: nothing to ship: set RESOLVED or COPY in $0" >&2
  exit 1
fi
for file in "${shipped[@]}"; do
  if [ ! -s "$file" ]; then
    echo "error: $file is missing or empty" >&2
    exit 1
  fi
done
