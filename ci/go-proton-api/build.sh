#!/usr/bin/env bash
# Build the go-proton-api lane driver against a patched copy of the
# pinned development server.
#
# The pinned server hard-codes crypto.RandomToken(16) for the SRP salt
# in server/backend/backend.go, while production's salt is 10 bytes
# (proton-srp's SALT_LEN_BYTES) and Sweetbriar's client refuses any other
# length. Its /mail/v4/messages/count also omits user-label totals, which
# production reports. patches/srp-salt-10.patch fixes both; it is applied
# to a temporary copy of the downloaded module, so the committed go.mod
# and go.sum keep pinning the upstream commit.
#
# Usage: build.sh [-o OUTPUT]     (default: ./gpa-driver)
set -euo pipefail

driver_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
dep="github.com/ProtonMail/go-proton-api"
patch="$driver_dir/patches/srp-salt-10.patch"

output=""
while [ "$#" -gt 0 ]; do
	case "$1" in
	-o)
		[ "$#" -ge 2 ] || {
			echo "build.sh: -o needs a path" >&2
			exit 2
		}
		output="$2"
		shift 2
		;;
	-h | --help)
		echo "usage: build.sh [-o OUTPUT]" >&2
		exit 0
		;;
	*)
		echo "build.sh: unknown argument: $1" >&2
		exit 2
		;;
	esac
done
output="${output:-gpa-driver}"

cd "$driver_dir"

version="$(go list -m -f '{{.Version}}' "$dep")"
download="$(go mod download -json "$dep@$version")"
module_dir="$(printf '%s\n' "$download" | sed -n 's/.*"Dir": "\([^"]*\)".*/\1/p')"
if [ -z "$module_dir" ] || [ ! -d "$module_dir" ]; then
	echo "build.sh: go mod download gave no module directory for $dep@$version" >&2
	exit 1
fi

tmp_base="${TMPDIR:-/tmp}"
tmp_base="${tmp_base%/}"
work="$(mktemp -d "$tmp_base/gpa-build.XXXXXX")"
trap 'rm -rf "$work"' EXIT HUP INT TERM

cp -R "$module_dir" "$work/server"
chmod -R u+w "$work/server"

if ! (cd "$work/server" && patch -p1 --batch --no-backup-if-mismatch < "$patch"); then
	echo "build.sh: the server patch did not apply to $dep@$version" >&2
	exit 1
fi

# The alternate modfile/sum pair keeps the upstream pin and hashes in the
# committed go.mod/go.sum; only the temporary copy carries the replace.
cp go.mod "$work/lane.mod"
cp go.sum "$work/lane.sum"
{
	echo
	echo "// Lane-local: build against the patched server copy."
	echo "replace $dep => $work/server"
} >> "$work/lane.mod"

case "$output" in
/*) ;;
*) output="$driver_dir/$output" ;;
esac

go build -modfile="$work/lane.mod" -o "$output" .