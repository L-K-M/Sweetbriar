#!/usr/bin/env bash
# Check scripts/build.sh --install against a fake macOS toolchain: the
# app replaces the installed copy, a running app is asked to quit and is
# reopened, and failures leave the installed app alone. Runs on Linux
# and macOS; touches nothing outside a temp dir.
#
# The fakes' bodies are single-quoted on purpose: they expand when the
# fake runs, not here.
# shellcheck disable=SC2016
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

failures=0
fail() { echo "FAIL: $*" >&2; failures=$((failures + 1)); }

# A throwaway repo holding only build.sh, so builds write nothing here.
repo="$work/repo"
mkdir -p "$repo/scripts"
cp "$here/build.sh" "$repo/scripts/build.sh"
printf '[workspace.package]\nversion = "9.9.9"\n' > "$repo/Cargo.toml"

# Fakes record their calls in $log and read their behavior from $state:
#   app-running       holds the command line of this user's running app;
#                     pgrep matches its pattern against it, and a quit
#                     naming that app removes it
#   helper-running    the same, for a SweetbriarHelper next to the app
#   interrupted-swap  the old app sits in .Sweetbriar.app.outgoing, as a
#                     run killed mid-swap leaves it
#   mv-into-fails     moving the new copy into place fails
#   ditto-fails, open-fails, xcodebuild-fails, app-ignores-quit
fakes="$work/bin"
log="$work/calls.log"
state="$work/state"
mkdir -p "$fakes"

fake() {
  printf '#!/usr/bin/env bash\nlog=%q; state=%q\n%s\n' "$log" "$state" "$2" > "$fakes/$1"
  chmod +x "$fakes/$1"
}
fake uname 'echo Darwin'
fake cargo 'echo "cargo $*" >> "$log"'
fake xcodegen 'echo "xcodegen $*" >> "$log"'
fake xcodebuild '
echo "xcodebuild $*" >> "$log"
[[ -e "$state/xcodebuild-fails" ]] && exit 1
while [[ $# -gt 0 ]]; do
  case "$1" in
    -configuration) config="$2"; shift ;;
    -derivedDataPath) derived="$2"; shift ;;
  esac
  shift
done
app="$derived/Build/Products/$config/Sweetbriar.app/Contents/MacOS"
mkdir -p "$app" && echo new > "$app/Sweetbriar"'
fake ditto '[[ -e "$state/ditto-fails" ]] && exit 1; cp -R "$1" "$2"'
fake mv '[[ -e "$state/mv-into-fails" && "$1" == *.incoming ]] && exit 1; exec /bin/mv "$@"'
# Refuses to run unscoped to this user, then matches like the real tool.
fake pgrep '
echo "pgrep $*" >> "$log"
[[ "$1 $2" == "-U $(id -u)" ]] || exit 2
[[ -e "$state/app-running" ]] && grep -Eq -- "${!#}" "$state/app-running"'
# `osascript -e "tell application \"<app>\" to quit"` quits that app.
fake osascript '
echo "osascript $*" >> "$log"
target="${2#tell application \"}"; target="${target%%\"*}"
[[ -e "$state/app-running" && ! -e "$state/app-ignores-quit" ]] || exit 0
running="$(cat "$state/app-running")"
[[ "$running" == "$target/Contents/MacOS/Sweetbriar" ]] && rm -f "$state/app-running"
exit 0'
fake sleep ':'
fake open 'echo "open $*" >> "$log"; [[ ! -e "$state/open-fails" ]]'

# Regex metacharacters and a space: the process pattern must quote them.
apps="$work/My Apps (test)"
installed="$apps/Sweetbriar.app/Contents/MacOS/Sweetbriar"

# run NAME [STATE...] -- ARGS: reset the fakes, preinstall an old app,
# run build.sh, and keep its output in $out and exit status in $status.
# applications_dir=... before run passes another SWEETBRIAR_APPLICATIONS_DIR.
run() {
  name="$1"; shift
  rm -rf "$state" "$apps" "$repo/dist" "$repo/build" "$log"
  mkdir -p "$state" "$(dirname "$installed")"
  echo old > "$installed"
  while [[ "$1" != "--" ]]; do touch "$state/$1"; shift; done
  shift
  if [[ -e "$state/app-running" ]]; then
    echo "$installed" > "$state/app-running"
  fi
  if [[ -e "$state/helper-running" ]]; then
    echo "${installed}Helper" > "$state/app-running"
  fi
  if [[ -e "$state/interrupted-swap" ]]; then
    mv "$apps/Sweetbriar.app" "$apps/.Sweetbriar.app.outgoing"
  fi
  echo "== $name"
  status=0
  out="$(PATH="$fakes:$PATH" SWEETBRIAR_APPLICATIONS_DIR="${applications_dir:-$apps}" \
    SWEETBRIAR_QUIT_SECONDS=2 "$BASH" "$repo/scripts/build.sh" "$@" 2>&1)" || status=$?
  touch "$log"
}

expect_status() { [[ "$status" -eq "$1" ]] || fail "$name: exit $status, expected $1"; }
expect_out() { grep -qF -- "$1" <<<"$out" || fail "$name: output lacks '$1'"; }
expect_call() { grep -qF -- "$1" "$log" || fail "$name: no call '$1'"; }
expect_no_call() { ! grep -qF -- "$1" "$log" || fail "$name: unexpected call '$1'"; }
expect_installed() {
  [[ "$(cat "$installed" 2>/dev/null)" == "$1" ]] || fail "$name: installed app is not the $1 one"
}
expect_no_staging() {
  [[ ! -e "$apps/.Sweetbriar.app.incoming" && ! -e "$apps/.Sweetbriar.app.outgoing" ]] \
    || fail "$name: staging copies left behind"
}

run "builds the app through xtask, XcodeGen and xcodebuild" -- app
expect_status 0
expect_call "cargo xtask ffi"
expect_call "xcodegen generate --spec apps/macos/project.yml"
expect_call "-scheme Sweetbriar -configuration Release"
expect_out "app: built (Release) -> dist/Sweetbriar.app"
[[ -d "$repo/dist/Sweetbriar.app" ]] || fail "$name: dist/Sweetbriar.app missing"
expect_installed old

run "builds Debug under --debug" -- app --debug
expect_status 0
expect_call "-configuration Debug"

run "replaces a running app, asking it to quit, and reopens it" app-running -- app --install
expect_status 0
expect_installed new
expect_out "install app: ok ($apps/Sweetbriar.app)"
expect_call "osascript -e tell application \"$apps/Sweetbriar.app\" to quit"
[[ ! -e "$state/app-running" ]] || fail "$name: the running app was not quit"
expect_call "open $apps/Sweetbriar.app"
expect_no_staging

run "leaves a stopped app stopped" -- app --install
expect_status 0
expect_installed new
expect_no_call "osascript"
expect_no_call "open $apps/Sweetbriar.app"

run "does not quit another app with a similar name" helper-running -- app --install
expect_status 0
expect_installed new
expect_no_call "osascript"

run "installs nothing when the build fails" xcodebuild-fails -- app --install
expect_status 1
expect_installed old
expect_out "app: FAILED (xcodebuild)"
if grep -qF "install app" <<<"$out"; then fail "$name: attempted an install"; fi

run "keeps the old app when Sweetbriar does not quit" app-running app-ignores-quit -- app --install
expect_status 1
expect_installed old
expect_out "install app: FAILED (Sweetbriar did not quit; quit it and rerun)"
[[ ! -e "$apps/.Sweetbriar.app.incoming" ]] || fail "$name: incoming copy left behind"

run "keeps the old app when the copy fails" ditto-fails -- app --install
expect_status 1
expect_installed old
expect_out "install app: FAILED (cannot write $apps"

run "restores the old app when the swap fails" mv-into-fails -- app --install
expect_status 1
expect_installed old
expect_out "install app: FAILED (cannot move the new app into place)"
expect_no_staging

run "recovers the app an interrupted swap left aside" interrupted-swap ditto-fails -- app --install
expect_status 1
expect_installed old

run "reports an app that does not reopen" app-running open-fails -- app --install
expect_status 0
expect_installed new
expect_out "install app: ok ($apps/Sweetbriar.app), but Sweetbriar did not reopen"

applications_dir="$apps/" run "quits the app under a trailing-slash directory" app-running -- app --install
expect_status 0
expect_installed new
[[ ! -e "$state/app-running" ]] || fail "$name: the running app was not quit"
expect_out "install app: ok ($apps/Sweetbriar.app)"

run "only plans an install under --check" -- app --install --check
expect_status 0
expect_installed old
expect_out "-- would install: $apps/Sweetbriar.app"
expect_no_call "xcodebuild"

if [[ $failures -ne 0 ]]; then
  echo "test-build-install: $failures failure(s)" >&2
  exit 1
fi
echo "test-build-install: ok"
