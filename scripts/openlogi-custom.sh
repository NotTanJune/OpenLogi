#!/usr/bin/env bash
# Keep the custom OpenLogi agent (two-way HID++ flicks + Strafe Space
# delegation) installed across upstream OpenLogi updates.
#
# The installed app is always an upstream release bundle whose nested agent is
# replaced with one built from this fork's patch branch, rebased onto the same
# upstream tag so the GUI <-> agent IPC protocol matches.
#
#   openlogi-custom.sh status          show what is installed
#   openlogi-custom.sh apply [APP]     port, build, and install onto a stock
#                                      upstream bundle (default: the installed
#                                      app when it is stock, else the cached
#                                      stock copy)
#   openlogi-custom.sh update          download the latest upstream release,
#                                      then apply it
#   openlogi-custom.sh restore         reinstall the last verified custom app
#   openlogi-custom.sh guard           launchd entry point: re-apply when an
#                                      update replaced the custom app
#   openlogi-custom.sh install-guard   install the launchd guard
#   openlogi-custom.sh uninstall-guard remove the launchd guard
set -euo pipefail

export PATH="/opt/homebrew/bin:/usr/local/bin:$HOME/.cargo/bin:/usr/bin:/bin:/usr/sbin:/sbin"

REPO="${OPENLOGI_CUSTOM_REPO:-$HOME/OpenLogi}"
BRANCH="${OPENLOGI_CUSTOM_BRANCH:-fix/strafe-socket-timeout}"
UPSTREAM_REMOTE=upstream
UPSTREAM_REPO=AprilNEA/OpenLogi
UPSTREAM_TEAM=8U3ZJ258K9
# Apple Development identity: a stable designated requirement across rebuilds,
# so macOS privacy grants survive each update (ad-hoc pins the cdhash).
SIGN_ID="${OPENLOGI_CUSTOM_SIGN_ID:-69C168B632296CA0F1D486769206010EE1916033}"
APP=/Applications/OpenLogi.app
AGENT_REL="Contents/Library/LoginItems/OpenLogi Agent.app"
AGENT_BIN_REL="$AGENT_REL/Contents/MacOS/openlogi-agent"
MARKER=/tmp/strafe.sock
# Bundled helpers, built from their own repositories (AutoRaise is GPL-3.0 and
# stays a separate executable).
STRAFE_REPO="${OPENLOGI_CUSTOM_STRAFE_REPO:-$HOME/strafe}"
STRAFE_BRANCH="${OPENLOGI_CUSTOM_STRAFE_BRANCH:-fix/macos27-direction-openlogi}"
AUTORAISE_REPO="${OPENLOGI_CUSTOM_AUTORAISE_REPO:-$HOME/AutoRaise}"

STATE="$HOME/Library/Application Support/OpenLogi-custom"
# Bundles live under a .noindex folder so Spotlight, and through it
# LaunchServices, never lists them as extra OpenLogi installs.
BUNDLES="$STATE/bundles.noindex"
LAST_GOOD="$BUNDLES/last-good/OpenLogi.app"
STOCK_DIR="$BUNDLES/stock"
STAGE="$BUNDLES/staging/OpenLogi.app"
STATE_FILE="$STATE/state.env"
LOG="$STATE/openlogi-custom.log"
LOCK="$STATE/lock"
GUARD_LABEL=com.nottanjune.openlogi-custom-guard
GUARD_PLIST="$HOME/Library/LaunchAgents/$GUARD_LABEL.plist"

mkdir -p "$STATE" "$STOCK_DIR"

log() { printf '%s %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$*" | tee -a "$LOG" >&2; }
die() {
  log "ERROR: $*"
  exit 1
}
notify() {
  osascript -e "display notification \"$1\" with title \"OpenLogi custom\"" >/dev/null 2>&1 || true
}
sha() { shasum -a 256 "$1" | cut -d' ' -f1; }
version_of() { defaults read "$1/Contents/Info.plist" CFBundleShortVersionString; }

lock() {
  if ! mkdir "$LOCK" 2>/dev/null; then
    # A lock older than an hour is left over from a killed run.
    if [[ -n "$(find "$LOCK" -maxdepth 0 -mmin +60 2>/dev/null)" ]]; then
      rm -rf "$LOCK" && mkdir "$LOCK"
    else
      log "another openlogi-custom run holds $LOCK; skipping"
      exit 0
    fi
  fi
  trap 'rm -rf "$LOCK"' EXIT
}

# A stock bundle is an intact upstream release signed by the upstream team.
is_stock() {
  local app=$1
  [[ -d "$app" ]] || return 1
  codesign --verify --deep --strict "$app" 2>/dev/null || return 1
  # Capture first: `grep -q` closing the pipe early fails under pipefail.
  local info
  info=$(codesign -dv "$app/$AGENT_REL" 2>&1)
  [[ "$info" == *$'\n'"TeamIdentifier=$UPSTREAM_TEAM"$'\n'* ]]
}

# Downloaded bundles carry com.apple.quarantine, and ditto copies it. A
# quarantined bundle that is not notarized makes Gatekeeper refuse every
# launch ("Open Anyway"). Only called on a bundle whose upstream signature
# was verified, or on our own re-signed build of one.
unquarantine() {
  xattr -dr com.apple.quarantine "$1" 2>/dev/null || true
}

is_custom() {
  local app=$1
  [[ -f "$app/$AGENT_BIN_REL" && -f "$STATE_FILE" ]] || return 1
  # shellcheck source=/dev/null
  source "$STATE_FILE"
  [[ "$(sha "$app/$AGENT_BIN_REL")" == "${CUSTOM_AGENT_SHA:-}" ]] &&
    codesign --verify --deep --strict "$app" 2>/dev/null
}

# Keep a pristine copy of every stock release seen, so a later apply or a
# port retry never depends on the stock app still being installed.
cache_stock() {
  local app=$1 version
  version=$(version_of "$app")
  if [[ ! -d "$STOCK_DIR/OpenLogi-$version.app" ]]; then
    ditto "$app" "$STOCK_DIR/OpenLogi-$version.app.partial"
    unquarantine "$STOCK_DIR/OpenLogi-$version.app.partial"
    mv "$STOCK_DIR/OpenLogi-$version.app.partial" "$STOCK_DIR/OpenLogi-$version.app"
    log "cached stock $version"
  fi
  printf '%s\n' "$STOCK_DIR/OpenLogi-$version.app"
}

# Settings the custom build depends on, re-asserted on every guard run so a
# GUI toggle or an upstream config migration cannot quietly undo them.
#
# mouse_profile_target = "focused": since 0.8 the default "pointer" mode runs
# keyboard actions (Copy, Paste, shortcuts) only when an Accessibility read
# confirms the hovered window is the focused one. That read has a 100 ms
# deadline that Chrome often misses, so the press is dropped.
ensure_settings() {
  local config="$HOME/.config/openlogi/config.toml"
  [[ -f "$config" ]] || return 0
  grep -q '^mouse_profile_target = "focused"$' "$config" && return 0
  local updated
  updated=$(awk '
    /^mouse_profile_target = / { next }
    { print }
    /^\[app_settings\]$/ { print "mouse_profile_target = \"focused\"" }
  ' "$config")
  grep -q '^mouse_profile_target = "focused"$' <<<"$updated" || {
    log "could not place mouse_profile_target in $config"
    return 0
  }
  # Write through the existing file so its owner-only mode is kept; the agent
  # reloads config.toml when it changes.
  printf '%s\n' "$updated" >"$config"
  log "set mouse_profile_target = \"focused\" in $config"
}

latest_cached_stock() {
  find "$STOCK_DIR" -maxdepth 1 -name 'OpenLogi-*.app' -print | sort -V | tail -1
}

# Rebase the fork patches onto upstream tag v$1 unless they already sit on it.
port_to() {
  local tag="v$1" base
  cd "$REPO"
  git fetch --quiet "$UPSTREAM_REMOTE" --tags
  git rev-parse --verify --quiet "$tag^{commit}" >/dev/null || die "upstream tag $tag not found"
  base=$(git describe --tags --abbrev=0 --match 'v*' "$BRANCH")
  if [[ "$base" == "$tag" ]]; then
    log "$BRANCH already based on $tag"
    return
  fi
  [[ -z "$(git status --porcelain --untracked-files=no)" ]] ||
    die "$REPO has uncommitted changes; commit or stash them, then rerun apply"
  local previous
  previous=$(git rev-parse --abbrev-ref HEAD)
  git tag -f "custom-backup/$BRANCH-on-$base" "$BRANCH" >/dev/null
  log "rebasing $BRANCH from $base onto $tag (backup tag custom-backup/$BRANCH-on-$base)"
  if ! git rebase --quiet --onto "$tag" "$base" "$BRANCH" >>"$LOG" 2>&1; then
    git rebase --abort >/dev/null 2>&1 || true
    git switch --quiet "$previous" 2>/dev/null || true
    die "rebase of $BRANCH onto $tag conflicted; resolve it by hand, then rerun apply"
  fi
  git switch --quiet "$previous" 2>/dev/null || true
}

build_agent() {
  cd "$REPO"
  local previous
  previous=$(git rev-parse --abbrev-ref HEAD)
  if [[ "$previous" != "$BRANCH" ]]; then
    [[ -z "$(git status --porcelain --untracked-files=no)" ]] ||
      die "$REPO has uncommitted changes on $previous; commit or stash them, then rerun apply"
    git switch --quiet "$BRANCH"
    trap 'git -C "$REPO" switch --quiet "'"$previous"'" 2>/dev/null; rm -rf "$LOCK"' EXIT
  fi
  log "testing the custom gesture and Strafe paths"
  cargo test --quiet -p openlogi-core --lib swipe >>"$LOG" 2>&1 || die "openlogi-core swipe tests failed"
  cargo test --quiet -p openlogi-device --lib session::gesture >>"$LOG" 2>&1 || die "openlogi-device gesture tests failed"
  cargo test --quiet -p openlogi-inject --lib strafe >>"$LOG" 2>&1 || die "openlogi-inject Strafe tests failed"
  log "building openlogi-agent (release)"
  DEVELOPER_DIR=/Library/Developer/CommandLineTools \
    cargo build --quiet --release -p openlogi-agent >>"$LOG" 2>&1 || die "agent build failed"
  if [[ "$previous" != "$BRANCH" ]]; then
    git switch --quiet "$previous"
    trap 'rm -rf "$LOCK"' EXIT
  fi
}

install_bundle() {
  local src=$1
  osascript -e 'tell application id "org.openlogi.openlogi" to quit' >/dev/null 2>&1 || true
  pkill -x openlogi-overlay >/dev/null 2>&1 || true
  wait_gone openlogi-desktop || pkill -x openlogi-desktop >/dev/null 2>&1 || true
  wait_gone openlogi-desktop || die "OpenLogi did not quit"
  rm -rf "$APP"
  ditto "$src" "$APP"
  unquarantine "$APP"
  ln -sf "$APP/Contents/MacOS/openlogi" /opt/homebrew/bin/openlogi 2>/dev/null || true
  # The SMAppService job carries a launch constraint for the signature that
  # registered it, so a re-signed agent is refused (OS_REASON_CODESIGNING)
  # until the GUI re-registers. A missing version marker makes it do so.
  pkill -x openlogi-agent >/dev/null 2>&1 || true
  rm -f "$HOME/.local/share/openlogi/registration-version"
  # A bundle copied a moment ago may not be registered with LaunchServices
  # yet, and `open` then fails with -10814. Register it, then retry.
  /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister \
    -f "$APP" >/dev/null 2>&1 || true
  local attempt
  for attempt in 1 2 3 4 5; do
    open "$APP" 2>>"$LOG" && break
    [[ $attempt -lt 5 ]] || die "could not launch $APP"
    sleep 2
  done
  local _
  for _ in $(seq 30); do
    launchctl print "gui/$(id -u)/org.openlogi.agent.service" 2>/dev/null |
      grep -q 'job state = running' && return 0
    sleep 1
  done
  die "the OpenLogi agent did not start; see launchctl print gui/$(id -u)/org.openlogi.agent.service"
}

wait_gone() {
  local _
  for _ in $(seq 15); do
    pgrep -x "$1" >/dev/null || return 0
    sleep 1
  done
  return 1
}

build_helpers() {
  local branch
  branch=$(git -C "$STRAFE_REPO" rev-parse --abbrev-ref HEAD)
  [[ "$branch" == "$STRAFE_BRANCH" ]] || die "$STRAFE_REPO is on $branch, expected $STRAFE_BRANCH"
  log "building strafe (release)"
  (cd "$STRAFE_REPO" && swift build --quiet -c release --product strafe) >>"$LOG" 2>&1 ||
    die "strafe build failed"
  log "building AutoRaise"
  make -C "$AUTORAISE_REPO" clean AutoRaise >>"$LOG" 2>&1 || die "AutoRaise build failed"
}

# Place and sign the helpers inside the staged agent bundle, before the agent
# bundle itself is signed so its seal covers them.
stage_helpers() {
  local helpers="$STAGE/$AGENT_REL/Contents/Helpers" name
  mkdir -p "$helpers"
  install -m 0755 "$STRAFE_REPO/.build/release/strafe" "$helpers/strafe"
  install -m 0755 "$AUTORAISE_REPO/AutoRaise" "$helpers/autoraise"
  install -m 0644 "$AUTORAISE_REPO/LICENSE.md" "$STAGE/$AGENT_REL/Contents/Resources/AutoRaise-LICENSE.md"
  for name in strafe autoraise; do
    codesign --force --options runtime --identifier "org.openlogi.helper.$name" \
      --sign "$SIGN_ID" "$helpers/$name" >>"$LOG" 2>&1 || die "signing helper $name failed"
  done
}

cmd_apply() {
  lock
  ensure_settings
  # The guard runs a private copy; keep it in step with the script that last
  # applied a build, or a later re-apply would use an outdated pipeline.
  if [[ -f "$GUARD_PLIST" && ! "$0" -ef "$STATE/openlogi-custom.sh" ]]; then
    install -m 0755 "$0" "$STATE/openlogi-custom.sh"
  fi
  local source=${1:-} version
  if [[ -z "$source" ]]; then
    if is_stock "$APP"; then source=$APP; else source=$(latest_cached_stock); fi
  fi
  [[ -n "$source" ]] || die "no stock OpenLogi bundle available; run update"
  is_stock "$source" || die "$source is not an intact upstream-signed OpenLogi bundle"
  source=$(cache_stock "$source")
  version=$(version_of "$source")
  log "applying custom agent to stock $version"

  port_to "$version"
  build_agent
  build_helpers

  local built="$REPO/target/release/openlogi-agent"
  grep -q "$MARKER" "$built" || die "built agent lacks the Strafe delegation"

  rm -rf "$STAGE"
  mkdir -p "$(dirname "$STAGE")"
  ditto "$source" "$STAGE"
  unquarantine "$STAGE"
  install -m 0755 "$built" "$STAGE/$AGENT_BIN_REL"
  # The upstream profile grants the virtual-HID-keyboard entitlement to the
  # upstream team only. Without it keys post as CGEvents, as before 0.8.
  rm -f "$STAGE/$AGENT_REL/Contents/embedded.provisionprofile"
  stage_helpers
  codesign --force --options runtime --sign "$SIGN_ID" "$STAGE/$AGENT_REL" >>"$LOG" 2>&1
  codesign --force --options runtime --preserve-metadata=identifier,entitlements \
    --sign "$SIGN_ID" "$STAGE" >>"$LOG" 2>&1
  codesign --verify --deep --strict "$STAGE" >>"$LOG" 2>&1 || die "staged bundle failed signature verification"

  local agent_sha
  agent_sha=$(sha "$STAGE/$AGENT_BIN_REL")
  cat >"$STATE_FILE" <<EOF
CUSTOM_AGENT_SHA=$agent_sha
UPSTREAM_VERSION=$version
FORK_COMMIT=$(git -C "$REPO" rev-parse "$BRANCH")
APPLIED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)
EOF
  rm -rf "$LAST_GOOD.new"
  mkdir -p "$(dirname "$LAST_GOOD")"
  ditto "$STAGE" "$LAST_GOOD.new"
  rm -rf "$LAST_GOOD"
  mv "$LAST_GOOD.new" "$LAST_GOOD"

  install_bundle "$STAGE"
  rm -rf "$STAGE"
  is_custom "$APP" || die "installed app does not match the staged custom build"
  log "installed custom OpenLogi $version (agent $agent_sha)"
  notify "Custom gestures applied to OpenLogi $version"
}

cmd_update() {
  local tag dir dmg mount
  tag=$(gh release view -R "$UPSTREAM_REPO" --json tagName -q .tagName)
  dir=$(mktemp -d "$STATE/download.XXXXXX")
  gh release download "$tag" -R "$UPSTREAM_REPO" -D "$dir" \
    -p "OpenLogi-$tag-macos-$(uname -m).dmg"
  dmg=$(find "$dir" -name '*.dmg' | head -1)
  mount=$(mktemp -d "$STATE/mount.XXXXXX")
  hdiutil attach -nobrowse -readonly -mountpoint "$mount" "$dmg" >/dev/null
  local cached
  cached=$(cache_stock "$mount/OpenLogi.app")
  hdiutil detach "$mount" >/dev/null
  rm -rf "$dir" "$mount"
  cmd_apply "$cached"
}

cmd_restore() {
  [[ -d "$LAST_GOOD" ]] || die "no last-good custom bundle at $LAST_GOOD"
  install_bundle "$LAST_GOOD"
  log "restored last-good custom bundle"
}

cmd_guard() {
  ensure_settings
  [[ -d "$APP" ]] || exit 0
  if [[ -d "$LOCK" ]] || is_custom "$APP"; then exit 0; fi
  if is_stock "$APP"; then
    log "guard: stock OpenLogi $(version_of "$APP") replaced the custom build; re-applying"
    notify "OpenLogi was updated; re-applying custom gestures"
    if (cmd_apply "$APP"); then exit 0; fi
    notify "Re-applying custom gestures failed; restored the last custom build. See $LOG"
    cmd_restore
    exit 1
  fi
  log "guard: $APP is neither stock nor the recorded custom build; leaving it alone"
}

cmd_status() {
  echo "installed:   $APP ($(version_of "$APP" 2>/dev/null || echo missing))"
  if is_custom "$APP"; then
    echo "state:       custom (verified)"
  elif is_stock "$APP"; then
    echo "state:       STOCK upstream (custom gestures missing)"
  else echo "state:       unknown"; fi
  [[ -f "$STATE_FILE" ]] && sed 's/^/recorded:    /' "$STATE_FILE"
  echo "fork base:   $(git -C "$REPO" describe --tags --abbrev=0 --match 'v*' "$BRANCH")"
  echo "last-good:   $([[ -d "$LAST_GOOD" ]] && version_of "$LAST_GOOD" || echo none)"
  echo "guard:       $(launchctl print "gui/$(id -u)/$GUARD_LABEL" >/dev/null 2>&1 && echo loaded || echo not loaded)"
}

cmd_install_guard() {
  # Run a private copy, so checking out a branch without this script in the
  # repository cannot break the guard.
  local self="$STATE/openlogi-custom.sh"
  install -m 0755 "$0" "$self"
  cat >"$GUARD_PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>$GUARD_LABEL</string>
    <key>ProgramArguments</key>
    <array>
        <string>/bin/bash</string>
        <string>$self</string>
        <string>guard</string>
    </array>
    <key>WatchPaths</key>
    <array>
        <string>/Applications</string>
        <string>$APP/Contents/Library/LoginItems/OpenLogi Agent.app/Contents/MacOS</string>
    </array>
    <key>StartInterval</key>
    <integer>1800</integer>
    <key>RunAtLoad</key>
    <true/>
    <key>ThrottleInterval</key>
    <integer>60</integer>
    <key>StandardOutPath</key>
    <string>$STATE/guard.out.log</string>
    <key>StandardErrorPath</key>
    <string>$STATE/guard.err.log</string>
</dict>
</plist>
EOF
  plutil -lint "$GUARD_PLIST" >/dev/null
  launchctl bootout "gui/$(id -u)/$GUARD_LABEL" >/dev/null 2>&1 || true
  launchctl bootstrap "gui/$(id -u)" "$GUARD_PLIST"
  log "guard installed: $GUARD_PLIST"
}

cmd_uninstall_guard() {
  launchctl bootout "gui/$(id -u)/$GUARD_LABEL" >/dev/null 2>&1 || true
  rm -f "$GUARD_PLIST"
  log "guard removed"
}

case "${1:-status}" in
  status) cmd_status ;;
  apply)
    shift
    cmd_apply "${1:-}"
    ;;
  update) cmd_update ;;
  restore)
    lock
    cmd_restore
    ;;
  guard) cmd_guard ;;
  install-guard) cmd_install_guard ;;
  uninstall-guard) cmd_uninstall_guard ;;
  *)
    sed -n '2,20p' "$0"
    exit 2
    ;;
esac
