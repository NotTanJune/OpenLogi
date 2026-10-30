# OpenLogi Simple: a one-page native settings app

Date: 2026-10-30
Status: implemented 2026-10-30

## Goal

Replace the upstream GPUI settings window inside the custom OpenLogi.app with
a small one-page app built from native AppKit controls, so every control
follows the macOS appearance (light/dark, accent colour, accessibility), and
remove every feature the user does not use.

## Scope

Kept, on one scrolling page (Option A):

- Header: device picker (shown only with more than one mouse) and battery.
- Buttons: Back, Forward, Middle, Thumb wheel up, Thumb wheel down, each an
  action popup.
- Gesture button: Flick left, Flick right.
- Pointer and scroll: DPI slider, SmartShift switch, scroll direction.
- Desktop: Space switching speed, Ctrl+Option+Arrow hotkeys, Auto-raise on/off,
  Auto-raise delay.

Removed: camera, lighting, keyboard settings, Actions Ring editor, per-app
profiles, pairing, in-app updater (Sparkle). The agent keeps applying whatever
is already configured for removed features.

## Architecture

- New crate `apps/openlogi-simple` with its own `[workspace]` table, so the
  upstream root `Cargo.toml` is never edited (verified: path dependencies on
  `openlogi-core` and `openlogi-ipc` resolve, and the app connected to the
  live agent as `ClientKind::Gui`).
- Installed by `scripts/openlogi-custom.sh` as
  `OpenLogi.app/Contents/MacOS/openlogi-desktop`, replacing the upstream GUI
  binary, so bundle identity, Dock icon, Login Item, and `openlogi://` links
  are unchanged.
- Native AppKit via `objc2`, same crate generation as the workspace.
- Settings writes reuse `openlogi-core`'s `Config` setters, save through
  `ConfigFile`, then call the agent's `reload_saved_config` (the upstream GUI's
  path). DPI and SmartShift also use the agent's live read/set calls where the
  upstream GUI does.
- Strafe and AutoRaise settings reuse the agent's own settings modules
  (`crates/openlogi-agent/src/helpers/{strafe,autoraise}.rs`) through `#[path]`
  includes, one source for both writers; after a change the app stops that
  helper and the agent's supervisor restarts it.
- The app takes over the upstream window's process duties: SMAppService
  registration of the agent job (with the stale-version re-registration),
  launching and arming the agent, single-instance, and showing the window for
  `openlogi://` links from the menu bar.

## Failure handling

| Situation | Behaviour |
|---|---|
| Agent not running | Window states it and offers Retry; no stale values shown |
| Protocol version skew | Version-mismatch message; no writes |
| Mouse asleep/disconnected | Bindings stay editable from the saved file; DPI and battery show "not connected" |
| A save fails | Control reverts to the stored value; short message states why |
| Upstream update breaks the app build | Daily check keeps the working build and notifies |

## Testing

- Unit tests for the pure view model: rows built from a `Config`, every edit
  written back through the core setters, action choices, round trips.
- On the Mac: light and dark screenshots, each control exercised against the
  live agent, guard simulation after installation.
