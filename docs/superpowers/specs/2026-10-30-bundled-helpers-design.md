# Bundled helpers: Strafe and AutoRaise inside the custom OpenLogi build

Date: 2026-10-30
Status: implemented 2026-10-30 (see docs/CUSTOM-BUILD.md for deviations)

## Goal

Run Strafe (per-display Space switching) and AutoRaise (raise and focus the
window under the pointer) as part of the custom OpenLogi build, so neither is a
separate app, Login Item, or privacy-permission row. Their settings change from
OpenLogi's menu-bar menu. Everything survives OpenLogi updates and restarts
through the existing `scripts/openlogi-custom.sh` pipeline and guard.

## Decisions

| Question | Decision |
|---|---|
| Integration | Bundled helper executables, built from source, launched and supervised by the agent |
| Settings location | Submenus on OpenLogi's menu-bar icon (drawn by the agent); no GUI rebuild |
| AutoRaise behaviour | Raise and focus (AutoRaise's standard mode), default On with the Short delay |
| Strafe menu-bar icon | Removed; its settings move into OpenLogi's menu |

Not in scope: a page in the OpenLogi settings window (would require building
the upstream GUI, a new IPC call, and heavier rebases), focus-only mode, and a
Rust reimplementation of either tool.

## Architecture

```
OpenLogi.app/Contents/Library/LoginItems/OpenLogi Agent.app/Contents/
  MacOS/openlogi-agent   custom agent: supervises helpers, owns the menu
  Helpers/strafe         built from NotTanJune/strafe (MIT)
  Helpers/autoraise      built from NotTanJune/AutoRaise (GPL-3.0)
```

AutoRaise stays a separate executable. Linking its GPL-3.0 source into the
MIT/Apache agent would make the agent a GPL-3.0 work; shipping it beside the
agent is aggregation, so each program keeps its own license. Its `LICENSE.md`
ships next to the binary.

### Units

1. **Strafe `serve` mode** (patch in `NotTanJune/strafe`). The menu-bar app
   without its `NSStatusItem`: control socket `/tmp/strafe.sock`, swipe
   interception, and the Ctrl+Opt+Arrow hotkeys keep running. Existing CLI
   commands are unchanged.
2. **AutoRaise build** (fork `NotTanJune/AutoRaise`). The stock command-line
   binary from its `Makefile`; configuration from `~/.config/AutoRaise/config`.
3. **Helper supervisor** (new module in `openlogi-agent`). Spawns both helpers
   as child processes when the agent arms, restarts a crashed helper, and stops
   them when the agent goes dormant or exits. Children are attributed to the
   agent by macOS, so they use its Accessibility grant.
4. **Settings menu** (agent tray). Two submenus on OpenLogi's existing menu:
   - `Space switching`: transition-speed presets (Strafe's `TransitionSpeed`
     cases) and Hotkeys on/off.
   - `Auto-raise`: On/Off and delay presets Instant, Short, Medium, Long,
     mapped to AutoRaise `delay` values 1, 2, 4, 8 at `pollMillis` 50.
5. **Build pipeline** (`scripts/openlogi-custom.sh apply`). Builds the agent,
   `swift build -c release` for Strafe and `make` for AutoRaise, signs each
   helper with the Apple Development identity, places them under `Helpers/`,
   then signs the agent and outer bundle as today.

### Settings ownership

Each setting keeps exactly one owner, the tool's own store; the menu reads and
writes that store and restarts the affected helper:

| Setting | Owner |
|---|---|
| Strafe transition speed, hotkeys | defaults domain `com.rileycx.strafe` (existing values carry over) |
| AutoRaise on/off, delay | `~/.config/AutoRaise/config`; Off means the supervisor does not start the helper, recorded as a commented `#openlogi-disabled` line in that file |

Nothing is added to OpenLogi's `config.toml` or the IPC wire format, so
upstream updates cannot conflict with these settings.

AutoRaise's `ignoreApps` always includes OpenLogi and its overlay so the
Actions Ring never takes focus.

## Data flow

- Gesture → agent → `NextDesktop` → existing Strafe delegation over
  `/tmp/strafe.sock` → bundled Strafe helper switches the display under the
  pointer. No agent change on this path.
- Pointer rests on a window → AutoRaise helper raises and focuses it.
- Menu selection → write owner store → supervisor restarts that helper.

## Failure handling

- Crash: restart with exponential backoff from 1 s to 60 s. Five crashes in
  five minutes stop restarts; the submenu title shows "unavailable". Space
  switches then use upstream's native path (the existing fallback when the
  socket is absent).
- Missing helper binary (for example a stock bundle before the guard
  re-applies): logged once, no restart loop.
- Agent dormant or exiting: SIGTERM, then SIGKILL after 2 s; no orphans.
- Helper build failure during `apply`: same as an agent build failure; the
  guard restores the last-good bundle and notifies.

## Retiring the standalone Strafe app

Only after the bundled helper passes integration checks: remove Strafe's
Login Item, quit it, delete `/Applications/strafe.app`, unregister it from
LaunchServices, and reset its now-unused Accessibility row (`com.rileycx.strafe`).

## Testing

1. Spike first: a helper spawned by the agent can post events and read the
   accessibility tree using the agent's grant. If not, stop and revisit the
   design before building further.
2. Unit: the supervisor restart policy as a sans-I/O state machine with an
   explicit `now`; menu model to settings-store round trips.
3. Integration after `apply`: both helpers run as agent children, the socket
   answers `ok`, a menu change restarts only the affected helper, and the guard
   simulation (stock reinstall, re-apply) brings the helpers back.
4. Manual: hover-raise between windows; flicks on both monitors.

## Repositories

- `NotTanJune/OpenLogi` branch `fix/strafe-socket-timeout`: supervisor, menu,
  script, docs.
- `NotTanJune/strafe` branch `fix/macos27-direction-openlogi`: `serve` mode.
- `NotTanJune/AutoRaise`: new GitHub fork of `sbmpost/AutoRaise`, cloned to
  `~/AutoRaise`; build-only, no source changes planned.
