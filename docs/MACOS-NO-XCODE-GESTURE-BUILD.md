# macOS gesture build without Xcode

This is the tested, local-only workflow for the `feature/gesture-button-flicks`
branch:

<https://github.com/tanmay-nargas/OpenLogi/tree/feature/gesture-button-flicks>

It builds the background agent with Apple's Command Line Tools, then places
that agent inside the normal `OpenLogi.app` bundle. The stock GUI and overlay
remain the app shell, while the replacement agent owns HID++ capture, the OS
input hook, gesture dispatch, and the LaunchAgent lifecycle.

This route is useful when a full GUI build is not possible. GPUI's macOS build
needs Apple's `metal` shader compiler, which is shipped with Xcode's Metal
Toolchain and is not included in Command Line Tools. The agent does not build
GPUI shaders, so it can be built with Command Line Tools alone.

## Requirements

- macOS 13 or later.
- Rust stable and Cargo.
- Apple's Command Line Tools:

  ```sh
  xcode-select --install
  xcrun --find clang
  ```

- The repository checked out at the feature branch.
- Logi Options+ and any other Logitech HID++ manager quit before capture is
  tested. Two managers cannot safely own the same receiver.

No Xcode installation, Apple Developer account, Developer ID certificate, or
notarization is required. The local bundle is ad-hoc signed for macOS code-sign
and TCC consistency; it is not a distributable notarized release.

## Install the prebuilt gesture release

The fork's GitHub Releases page contains `OpenLogi-gesture-button-flicks-macos-arm64.zip`.
It is the ad-hoc-signed application bundle produced by this workflow. Download
the ZIP, verify its published SHA-256 checksum, quit OpenLogi, and replace
`/Applications/OpenLogi.app` with the extracted app. Because the signature is
local rather than Apple-notarized, macOS may require **Open Anyway** on first
launch and may require the nested **OpenLogi Agent** to be re-added under Input
Monitoring and Accessibility as described below.

The release tag points at the exact source revision used by the packaged agent.
The GUI shell remains the upstream application; the nested agent contains the
gesture-button and interruptible animated Space-transition behavior.

## Build the custom agent

From the repository root:

```sh
git switch feature/gesture-button-flicks
DEVELOPER_DIR=/Library/Developer/CommandLineTools \
  cargo build -p openlogi-agent --release
```

The result is:

```text
target/release/openlogi-agent
```

Do not use `cargo run -p openlogi-desktop` for this CLT-only workflow. A GUI
build can fail in `gpui_macos` with `xcrun: unable to find utility "metal"`.

## Replace only the embedded agent

The exact nested path in the released app is:

```text
/Applications/OpenLogi.app/Contents/Library/LoginItems/
  OpenLogi Agent.app/Contents/MacOS/openlogi-agent
```

First quit the app and make a recoverable, timestamped backup. Keep the backup
outside `/Applications`:

```sh
APP=/Applications/OpenLogi.app
BACKUP_ROOT="$HOME/OpenLogi-backups"
BACKUP_APP="$BACKUP_ROOT/OpenLogi.app.$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$BACKUP_ROOT"
osascript -e 'tell application "OpenLogi" to quit' 2>/dev/null || true
ditto "$APP" "$BACKUP_APP"
```

Stage a copy, replace the nested agent, and ad-hoc sign the staged bundle:

```sh
STAGED_APP=/private/tmp/OpenLogi-custom.app
rm -rf "$STAGED_APP"
ditto "$APP" "$STAGED_APP"
install -m 0755 target/release/openlogi-agent \
  "$STAGED_APP/Contents/Library/LoginItems/OpenLogi Agent.app/Contents/MacOS/openlogi-agent"
codesign --force --deep --sign - "$STAGED_APP"
codesign --verify --deep --strict --verbose=2 "$STAGED_APP"
```

Install the staged bundle only after verification:

```sh
ditto "$STAGED_APP" "$APP"
```

The GUI, overlay, resources, bundle identifiers, and existing configuration
are preserved. The GUI and overlay executable contents are not replaced, but
their nested ad-hoc signatures are regenerated as part of signing the
containing bundle. Rebuilding or replacing the bundle changes its code
identity, so macOS may require the agent to be re-added under **System Settings
→ Privacy & Security → Input Monitoring**, and OpenLogi/its agent under
**Accessibility**.

## Make it persistent after restart

In OpenLogi's settings, enable **Launch at login**. The custom agent writes:

```text
~/Library/LaunchAgents/org.openlogi.agent.plist
```

The plist runs the embedded agent from the installed bundle, has
`RunAtLoad=true`, and keeps it alive after an ordinary exit. Verify the job
without starting a second agent:

```sh
launchctl print "gui/$(id -u)/org.openlogi.agent" | \
  sed -n '1,100p'
```

After an update, reload the existing job rather than launching a second copy:

```sh
launchctl kickstart -k "gui/$(id -u)/org.openlogi.agent"
open /Applications/OpenLogi.app
```

The agent log is normally under:

```text
~/.local/state/openlogi/agent.YYYY-MM-DD.log
```

Healthy startup includes `openlogi-agent started`, `OS input hook installed`,
`IPC server listening`, and a `control capture active` line. A CLI process
that reports its own Input Monitoring denial is not a valid agent health check;
check the embedded agent's log and launchd process instead.

If the new log says `Input Monitoring is NOT granted`, quit OpenLogi, open both
**Privacy & Security → Input Monitoring** and **Privacy & Security →
Accessibility**, and enable the installed **OpenLogi Agent**. If it is absent,
use `+` and select this installed app bundle (not the unsigned build output):

```text
/Applications/OpenLogi.app/Contents/Library/LoginItems/OpenLogi Agent.app
```

Remove an old disabled duplicate if macOS shows more than one OpenLogi Agent,
then restart the existing job:

```sh
launchctl kickstart -k "gui/$(id -u)/org.openlogi.agent"
```

## Gesture and Space-switch behavior

The HID++ gesture path is intentionally split into two layers:

1. `crates/openlogi-device/src/session/gesture.rs` turns the diverted raw-XY
   reports into a directional gesture and preserves balanced button edges.
2. `crates/openlogi-agent-core/src/watchers/gesture/dispatch.rs` resolves the
   direction against the per-device binding and dispatches the action while the
   physical press is still valid.

The dedicated MX gesture button uses the immediate 8-unit first-flick policy;
fresh-contact raw-XY reports are discarded so a rapid re-press cannot invent an
opposite direction. The shared accumulator still commits at most one gesture
per physical hold and treats an uncommitted release as a click.

When Strafe is not running, `PreviousDesktop` and `NextDesktop` use the user's
configured symbolic hotkey, preserving the normal Space animation. If the
opposite Space request arrives within the 900 ms reversal window, the agent
reads the active display's ordered Space IDs through the private
CoreGraphics/SkyLight symbols. It then posts a private Dock swipe sequence with
Began, partial Changed, and moderate-velocity Ended phases. Starting a fresh
interactive swipe interrupts the in-flight transition; leaving most of its
progress unfinished lets Dock smoothly animate the remainder in the opposite
direction instead of jumping. The complete snapshot-and-event operation is
hopped from the agent's HID worker to AppKit's main thread; otherwise macOS can
return success without interrupting the visible animation. If the event cannot
be created, the private managed-space setter is attempted, and the existing
symbolic hotkey path remains the final fallback.

When the Strafe menu-bar app is running, the same two actions first send
`switch left` or `switch right` to Strafe's owner-only `/tmp/strafe.sock`
endpoint. Strafe then applies its currently selected Transition Speed to the
synthetic Dock gesture. If the endpoint is unavailable or rejects the request,
OpenLogi uses the native path described above.

For a complete cross-machine setup, see the [Strafe integration guide](https://github.com/tanmay-nargas/strafe/blob/main/docs/OPENLOGI-INTEGRATION.md).
The committed `artifacts/OpenLogi.app` bundle in this fork contains the same
agent integration for a quick install. Any ad-hoc-signed replacement creates a
new macOS TCC identity, so grant Input Monitoring to **OpenLogi Agent** and
restart OpenLogi before testing the gesture button.

## Verification checklist

Run the offline checks from the repository root:

```sh
cargo fmt --all -- --check
DEVELOPER_DIR=/Library/Developer/CommandLineTools \
  cargo test -p openlogi-inject --no-fail-fast
DEVELOPER_DIR=/Library/Developer/CommandLineTools \
  cargo build -p openlogi-agent --release
```

Then verify the installed bundle and runtime:

```sh
codesign --verify --deep --strict --verbose=2 /Applications/OpenLogi.app
launchctl print "gui/$(id -u)/org.openlogi.agent" | \
  sed -n '1,100p'
pgrep -af '/Applications/OpenLogi.app/.*openlogi-agent' || true
```

If the GUI is open while installing, it may briefly launch a detached helper
when the IPC socket is unavailable. For a reliable persistence check, quit the
GUI, stop the one exact agent process if necessary, then start the registered
LaunchAgent and confirm that `launchctl print` reports `state = running` and
the `program` path is under `/Applications/OpenLogi.app`. Reopen the GUI after
that check; it should connect to the already-running agent.

Finally test with the real gesture button: flick to the next Space, flick back
before the animation ends, and repeat in both directions. The reverse flick
should select the opposite Space immediately, including near the end of the
first animation.

## Rollback

Stop the agent, restore the desired timestamped backup, then restart the same
LaunchAgent:

```sh
launchctl kill SIGTERM "gui/$(id -u)/org.openlogi.agent" 2>/dev/null || true
ditto "$HOME/OpenLogi-backups/OpenLogi.app.<timestamp>" \
  /Applications/OpenLogi.app
codesign --verify --deep --strict --verbose=2 /Applications/OpenLogi.app
launchctl kickstart -k "gui/$(id -u)/org.openlogi.agent"
```

Do not delete the backup until the replacement has survived a logout/restart
and a real-device capture test.
