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
| `apply [APP]` | rebase the patch branch onto the app's tag if needed, run the focused tests, build the agent, re-sign, install, relaunch |
| `update` | download the latest upstream DMG with `gh`, then `apply` it |
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
