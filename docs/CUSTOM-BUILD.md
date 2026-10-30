# Custom OpenLogi build (flicks + Strafe)

This fork carries two patches on top of an upstream release tag:

- `feat(gesture): add two-way HID++ flicks`: the dedicated MX gesture button
  commits one left/right flick per hold at an 8-unit threshold, with no hold
  gate. Up, Down, and Click slots of that button are ignored.
- `feat(macos): delegate Space switches to Strafe when it is running`:
  `PreviousDesktop`/`NextDesktop` send `switch left`/`switch right` to
  `/tmp/strafe.sock`. Only an unavailable socket falls back to upstream's
  native switch; a rejection or missing reply never retries natively.

Only the nested agent is replaced. The GUI, overlay, and Sparkle framework stay
upstream's binaries, so the patch branch must sit on the same upstream tag as
the installed app (the GUI and agent share a versioned IPC protocol).

## The settings window (openlogi-simple)

The app's main program (`Contents/MacOS/openlogi-desktop`) is
[openlogi-simple](https://github.com/NotTanJune/openlogi-simple) (private),
checked out at `~/openlogi-simple`, beside this fork. It is a one-page settings window built from native AppKit
controls, so it follows the macOS appearance, accent colour, and accessibility
settings. It replaces upstream's GPUI window; the bundle identity, Dock icon,
Login Item, and `openlogi://` links are unchanged.

- Scope: buttons, gesture flicks, DPI, SmartShift, scroll direction, Space
  switching, Auto-raise, and a System / Light / Dark appearance toggle. Camera, lighting, keyboard settings, the Actions
  Ring editor, per-app profiles, pairing, and Sparkle are left out; the agent
  keeps applying anything already configured for them.
- It is its own repository and Cargo workspace, so upstream's `Cargo.toml` is
  never edited. Its path dependencies point at `../OpenLogi`, so it always
  builds against the same OpenLogi code as the agent. It reuses `openlogi-core` setters and the agent's IPC exactly as
  upstream's window does, and includes the agent's Strafe and AutoRaise settings
  modules by path.
- It takes over the window's process duties: SMAppService registration,
  launching and arming the agent, single instance, and showing the window for
  menu-bar links. Closing the window quits it, as upstream did.
- `apply` clones it when `~/openlogi-simple` is missing, runs its tests and
  builds it; an update that breaks it keeps the working build. For a dev run beside the installed app:
  `OPENLOGI_SIMPLE_DEV=1 cargo run` in `~/openlogi-simple`
  (an unbundled binary never registers the agent service).
- Its `src/registration.rs` carries macOS FFI ported from
  `crates/openlogi-desktop/src/platform/registration/macos.rs`; it is not listed
  in `.agents/rules/objc-ffi.md`, which stays upstream's file.
- Known gap: an unparseable `config.toml` makes the app exit instead of opening
  with defaults.

## Bundled helpers

Strafe and AutoRaise ship inside the agent bundle at
`OpenLogi Agent.app/Contents/Helpers/{strafe,autoraise}`; there is no separate
Strafe or AutoRaise app, Login Item, or privacy row.

- The agent starts both at arming as its own children (`strafe serve`, the
  headless mode from `NotTanJune/strafe`), so they run on the agent's
  Accessibility grant. `succession` restarts a helper that exits (1 s, doubling
  to 60 s, reset after 30 s of healthy running). launchd ends them with the
  agent's job; a helper left by an abrupt exit is evicted before the next start.
- Settings live on OpenLogi's menu-bar icon: **Space Switching** (transition
  speed, Ctrl+Option+Arrow hotkeys) and **Auto-Raise Windows** (on/off, delay).
  Each is stored by its own tool: Strafe's `com.rileycx.strafe` defaults and
  `~/.config/AutoRaise/config` (rewritten by the menu). A change restarts only
  that helper.
- AutoRaise is GPL-3.0 and stays a separate executable built from
  `NotTanJune/AutoRaise`; its license ships as
  `Contents/Resources/AutoRaise-LICENSE.md`.
- Helper output: `~/.local/state/openlogi/helper-{strafe,autoraise}.log`.
- Differences from the design spec: restarts back off to 60 s indefinitely
  instead of stopping after five crashes, and the menu does not show an
  "unavailable" state.

## Update protection

The guard runs `check-updates` once a day (at login, then every 24 hours):

| Upstream | What happens |
|---|---|
| OpenLogi (`AprilNEA/OpenLogi`) | A newer stable tag is downloaded from `updates.openlogi.org`, accepted only with upstream's intact signature, and the patch branch is rebased onto it |
| Strafe (`rileycx/strafe`) | `fix/macos27-direction-openlogi` is rebased onto `upstream/main`, then `swift build` and `swift test` must pass |
| AutoRaise (`sbmpost/AutoRaise`) | `master` is fast-forwarded to `upstream/master`, then `make` must pass |

Any change triggers a full rebuild and install. A conflict, failed build, or
failed test keeps the working build and posts a notification; nothing is
installed. Each rewritten branch is pushed to the `NotTanJune` fork with a lease
on GitHub's actual tip, and only after a backup tag holding that tip is pushed,
so nothing on GitHub is ever lost. A push that fails is retried on the next
check. Strafe's speed and hotkey settings are snapshotted around each rebuild,
restored if an upstream migration changed them, and checked against the
bundled helper (a renamed settings key raises a notification).

Run it by hand with `scripts/openlogi-custom.sh check-updates`.

## Settings the guard enforces

`mouse_profile_target = "focused"` in `~/.config/openlogi/config.toml`. Since
0.8 the default `"pointer"` mode runs keyboard actions (Copy, Paste, shortcuts)
only after an Accessibility read confirms the hovered window has focus. That
read has a 100 ms deadline Chrome often misses, so presses were dropped
(the agent logs `mouse action target unavailable, moved, or not focused`). This
config has no per-app profiles, so focused mode loses nothing. The guard
re-asserts the setting on every run.

## Tooling

`scripts/openlogi-custom.sh` owns the whole lifecycle:

| Command | Effect |
|---|---|
| `status` | installed version, custom or stock, recorded build, guard state |
| `apply [APP]` | rebase the patch branch onto the app's tag if needed, run the focused tests, build the agent and both helpers, re-sign, install, relaunch, and refresh the guard's script copy |
| `update` | download the latest stable upstream release, then `apply` it |
| `check-updates` | sync all three upstreams and rebuild when anything changed (see below) |
| `restore` | reinstall the last verified custom bundle |
| `install-guard` / `uninstall-guard` | manage the launchd guard |

The guard (`~/Library/LaunchAgents/com.nottanjune.openlogi-custom-guard.plist`)
watches `/Applications` and runs every 30 minutes. When an intact,
upstream-signed OpenLogi replaces the custom build (Sparkle, a manual install,
Homebrew), it re-applies the patches. If the rebase conflicts, a test fails, or
the build fails, it reinstalls the last custom bundle and posts a notification
naming the log.

State lives in `~/Library/Application Support/OpenLogi-custom/`: `state.env`,
`openlogi-custom.log`, and `bundles.noindex/` (pristine stock releases and the
last-good custom app; `.noindex` keeps them out of Spotlight and LaunchServices).

## Signing and permissions

- The agent and outer app are signed with the local Apple Development identity.
  Its designated requirement names the certificate, not a cdhash, so Input
  Monitoring and Accessibility grants survive every rebuild. Re-granting is
  needed only when that identity changes (certificate expiry: 2027-06-28).
- Upstream's agent profile grants `com.apple.developer.hid.virtual.device` to
  upstream's team only. The custom agent drops it, so synthesized keys post as
  `CGEvent`s, as they did before 0.8.
- The SMAppService job carries a launch constraint for the signature that
  registered it. `apply` deletes `~/.local/share/openlogi/registration-version`
  so the GUI re-registers the job; otherwise launchd refuses the agent with
  `OS_REASON_CODESIGNING`.
- Downloaded bundles carry `com.apple.quarantine`. A quarantined app that is not
  notarized triggers Gatekeeper's "Open Anyway" on every launch, so the script
  strips quarantine, but only from bundles whose upstream signature it verified.
- If a stale permission row ever appears (a toggle that is on but does nothing):

  ```sh
  tccutil reset ListenEvent org.openlogi.agent
  tccutil reset Accessibility org.openlogi.agent
  ```

  then relaunch OpenLogi and allow both prompts.

## When a rebase conflicts

Resolve it by hand on the patch branch (`fix/strafe-socket-timeout`), then run
`scripts/openlogi-custom.sh apply`. Each automatic rebase first tags the old tip
as `custom-backup/<branch>-on-<old tag>`.
