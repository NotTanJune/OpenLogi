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
#   openlogi-custom.sh check-updates   sync Strafe and AutoRaise with their
#                                      upstreams, take a newer OpenLogi
#                                      release, rebuild; the guard runs it daily
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
  # Reentrant: check-updates holds the lock while it calls apply.
  [[ -n "${LOCK_HELD:-}" ]] && return 0
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
  LOCK_HELD=1
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
  publish_fork "$REPO" "$BRANCH"
  notify "Custom gestures applied to OpenLogi $version"
}

# The newest stable upstream OpenLogi tag, e.g. v0.8.19.
latest_openlogi_tag() {
  git ls-remote --tags --refs "https://github.com/$UPSTREAM_REPO.git" 'v*' |
    sed 's|.*refs/tags/||' | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' | sort -V | tail -1
}

# Download release $1 and cache its verified stock bundle; prints its path.
# Only a bundle carrying upstream's intact signature is accepted.
download_release() {
  local tag=$1 dir dmg mount cached
  dir=$(mktemp -d "$STATE/download.XXXXXX")
  dmg="$dir/OpenLogi-$tag.dmg"
  if ! curl -fsSL -o "$dmg" \
    "https://updates.openlogi.org/releases/$tag/OpenLogi-$tag-macos-$(uname -m).dmg"; then
    rm -rf "$dir"
    log "release $tag has no downloadable macOS build yet"
    return 1
  fi
  mount=$(mktemp -d "$STATE/mount.XXXXXX")
  hdiutil attach -nobrowse -readonly -mountpoint "$mount" "$dmg" >/dev/null
  if is_stock "$mount/OpenLogi.app"; then
    cached=$(cache_stock "$mount/OpenLogi.app")
  fi
  hdiutil detach "$mount" >/dev/null
  rm -rf "$dir" "$mount"
  [[ -n "${cached:-}" ]] || {
    log "release $tag failed upstream signature verification; ignored"
    return 1
  }
  printf '%s\n' "$cached"
}

cmd_update() {
  local tag cached
  tag=$(latest_openlogi_tag)
  [[ -n "$tag" ]] || die "could not read upstream OpenLogi tags"
  cached=$(download_release "$tag") || die "could not download OpenLogi $tag"
  cmd_apply "$cached"
}

# Publish fork branch $2 of $1. GitHub may only lose nothing: its tip must be
# contained in the local branch, or in one of the backup tags of a rewritten
# tip (pushed first). The lease is GitHub's own current tip. Any failure only
# logs; every update check retries an unpublished branch.
publish_fork() {
  local repo=$1 branch=$2 remote tag covered=""
  # A private ref, so a clone whose origin refspec omits this branch works.
  local ref="refs/openlogi-custom/github/$branch"
  git -C "$repo" fetch --quiet origin "+refs/heads/$branch:$ref" >>"$LOG" 2>&1 || {
    log "WARNING: could not reach GitHub for $branch of $repo; will retry"
    return 0
  }
  remote=$(git -C "$repo" rev-parse "$ref")
  [[ "$remote" == "$(git -C "$repo" rev-parse "$branch")" ]] && return 0
  if git -C "$repo" merge-base --is-ancestor "$remote" "$branch"; then
    covered=fast-forward
  else
    for tag in $(git -C "$repo" tag -l 'auto-backup/*' 'custom-backup/*' 'backup/*'); do
      if git -C "$repo" merge-base --is-ancestor "$remote" "$tag"; then
        git -C "$repo" push --quiet origin "refs/tags/$tag" >>"$LOG" 2>&1 || return 0
        covered=$tag
        break
      fi
    done
  fi
  if [[ -z "$covered" ]]; then
    log "WARNING: GitHub's $branch of $repo has commits this Mac lacks; not overwriting"
    notify "$(basename "$repo"): GitHub has changes this Mac lacks; push skipped"
    return 0
  fi
  if git -C "$repo" push --quiet "--force-with-lease=$branch:$remote" origin "$branch" >>"$LOG" 2>&1; then
    log "pushed $branch of $repo (previous GitHub tip kept by: $covered)"
  else
    log "WARNING: could not push $branch of $repo; will retry"
  fi
}

# Rebase fork branch $2 of $1 onto upstream $3, keep it only if $4 (a build
# and test command) passes, then publish it. Returns 0 when the branch moved.
sync_fork() {
  local repo=$1 branch=$2 upstream=$3 verify=$4 name old base tag
  name=$(basename "$repo")
  if [[ "$(git -C "$repo" rev-parse --abbrev-ref HEAD)" != "$branch" ]] ||
    [[ -n "$(git -C "$repo" status --porcelain --untracked-files=no)" ]]; then
    log "$name: not on a clean $branch; update check skipped"
    notify "$name update check skipped: commit or switch to $branch"
    return 1
  fi
  if [[ -z "${OPENLOGI_CUSTOM_OFFLINE:-}" ]]; then
    git -C "$repo" fetch --quiet "${upstream%%/*}" || {
      log "$name: fetch failed; will retry"
      return 1
    }
  fi
  git -C "$repo" merge-base --is-ancestor "$upstream" "$branch" && return 1
  old=$(git -C "$repo" rev-parse "$branch")
  base=$(git -C "$repo" merge-base "$upstream" "$branch")
  tag="auto-backup/$branch-$(date +%Y%m%d-%H%M%S)"
  git -C "$repo" tag "$tag" "$old"
  log "$name: upstream moved; rebasing $branch onto $upstream (old tip kept as $tag)"
  if ! git -C "$repo" rebase --quiet --onto "$upstream" "$base" "$branch" >>"$LOG" 2>&1; then
    git -C "$repo" rebase --abort >/dev/null 2>&1 || true
    log "$name: rebase onto $upstream conflicted; keeping the working build"
    notify "$name update needs a manual merge; your current build is unchanged"
    return 1
  fi
  if ! (cd "$repo" && eval "$verify") >>"$LOG" 2>&1; then
    git -C "$repo" reset --quiet --hard "$old"
    log "$name: updated code failed its build or tests; rolled back"
    notify "$name update failed its tests; your current build is unchanged"
    return 1
  fi
  publish_fork "$repo" "$branch"
  return 0
}

# Settings the helpers' upstreams could reset in a migration; restored after
# every rebuild.
snapshot_settings() {
  SNAP_SPEED=$(defaults read com.rileycx.strafe transitionSpeed 2>/dev/null || true)
  SNAP_HOTKEYS=$(defaults read com.rileycx.strafe spaceHotkeysEnabled 2>/dev/null || true)
}

restore_settings() {
  local now
  now=$(defaults read com.rileycx.strafe transitionSpeed 2>/dev/null || true)
  if [[ -n "$SNAP_SPEED" && "$now" != "$SNAP_SPEED" ]]; then
    defaults write com.rileycx.strafe transitionSpeed -int "$SNAP_SPEED"
    log "restored Strafe transition speed $SNAP_SPEED"
  fi
  now=$(defaults read com.rileycx.strafe spaceHotkeysEnabled 2>/dev/null || true)
  if [[ -n "$SNAP_HOTKEYS" && "$now" != "$SNAP_HOTKEYS" ]]; then
    defaults write com.rileycx.strafe spaceHotkeysEnabled -bool "$([[ "$SNAP_HOTKEYS" == 1 ]] && echo true || echo false)"
    log "restored Strafe hotkeys setting"
  fi
  # The installed helper must read the speed the menu wrote; a renamed key
  # upstream would silently detach the menu from Strafe.
  local helper="$APP/$AGENT_REL/Contents/Helpers/strafe" want
  if [[ -x "$helper" && -n "$SNAP_SPEED" ]]; then
    local -a titles=(Instant Quick Smooth)
    want=${titles[$SNAP_SPEED]:-Instant}
    "$helper" speed 2>/dev/null | head -1 | grep -q "$want" || {
      log "WARNING: bundled Strafe does not read the stored speed ($want); its settings keys may have changed"
      notify "Strafe settings may have changed upstream; check the Space Switching menu"
    }
  fi
}

cmd_check_updates() {
  lock
  local rebuild="" source="" tag installed
  snapshot_settings
  sync_fork "$STRAFE_REPO" "$STRAFE_BRANCH" upstream/main \
    'swift build --quiet -c release --product strafe && swift test --quiet' && rebuild=1
  sync_fork "$AUTORAISE_REPO" master upstream/master 'make clean AutoRaise' && rebuild=1
  tag=$(latest_openlogi_tag || true)
  installed=$(version_of "$APP" 2>/dev/null || echo 0)
  if [[ -n "$tag" && "v$installed" != "$tag" ]] &&
    [[ "$(printf '%s\n%s\n' "v$installed" "$tag" | sort -V | tail -1)" == "$tag" ]]; then
    log "OpenLogi $tag is available (installed $installed)"
    source=$(download_release "$tag") || source=""
  fi
  if [[ -n "$source" ]]; then
    notify "Updating OpenLogi to $tag with your custom features"
    cmd_apply "$source"
  elif [[ -n "$rebuild" ]]; then
    cmd_apply
  else
    log "update check: everything is current"
  fi
  restore_settings
  # Retry anything a previous run could not publish.
  publish_fork "$REPO" "$BRANCH"
  publish_fork "$STRAFE_REPO" "$STRAFE_BRANCH"
  publish_fork "$AUTORAISE_REPO" master
  date +%s >"$STATE/last-update-check"
}

cmd_restore() {
  [[ -d "$LAST_GOOD" ]] || die "no last-good custom bundle at $LAST_GOOD"
  install_bundle "$LAST_GOOD"
  log "restored last-good custom bundle"
}

cmd_guard() {
  ensure_settings
  [[ -d "$APP" ]] || exit 0
  [[ -d "$LOCK" ]] && exit 0
  if is_custom "$APP"; then
    local last
    last=$(cat "$STATE/last-update-check" 2>/dev/null || echo 0)
    if (($(date +%s) - last >= 86400)); then
      (cmd_check_updates) || log "update check failed; the working build is unchanged"
    fi
    exit 0
  fi
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

# Dispatch only when executed, so tests can source the functions.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  case "${1:-status}" in
    status) cmd_status ;;
    apply)
      shift
      cmd_apply "${1:-}"
      ;;
    update) cmd_update ;;
    check-updates) cmd_check_updates ;;
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
fi
