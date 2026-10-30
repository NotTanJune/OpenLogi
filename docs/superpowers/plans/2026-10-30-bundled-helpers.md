# Bundled Strafe and AutoRaise Helpers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Run Strafe and AutoRaise as agent-supervised helpers inside the custom OpenLogi bundle, configured from OpenLogi's menu-bar menu, surviving updates and restarts.

**Architecture:** The agent starts both helper executables (`Contents/Helpers/` of `OpenLogi Agent.app`) as child processes through the `succession` supervisor it already uses for the overlay, so they inherit its Accessibility grant. Two new tray submenus write each tool's own settings store and restart that helper. `scripts/openlogi-custom.sh apply` builds, signs, and places the helpers.

**Tech Stack:** Rust (agent, objc2 AppKit tray, `succession`), Swift (Strafe), Objective-C++ (AutoRaise), bash.

Spec: `docs/superpowers/specs/2026-10-30-bundled-helpers-design.md`.

## Global Constraints

- No change to `config.toml` schema, IPC wire types, any `Cargo.toml`, or locale catalogs (keeps upstream rebases conflict-free). Menu titles are English literals.
- AutoRaise stays a separate GPL-3.0 executable; never link or copy its source into Rust crates. Its `LICENSE.md` ships in `OpenLogi Agent.app/Contents/Resources/AutoRaise-LICENSE.md`.
- Strafe settings: defaults domain `com.rileycx.strafe`, keys `transitionSpeed` (0 instant, 1 quick, 2 smooth) and `spaceHotkeysEnabled` (bool, default true).
- AutoRaise settings: `~/.config/AutoRaise/config`; disabled is the line `#openlogi-disabled`; delay presets Instant/Short/Medium/Long = `delay` 1/2/4/8 at `pollMillis=50`; default enabled, Short; `ignoreApps="OpenLogi,OpenLogi Overlay"`.
- Restart policy: `succession::supervision::Restart { base: 1 s, ceiling: 60 s, healthy_after: 30 s }`.
- Helper output appends to `~/.local/state/openlogi/helper-<name>.log`.
- Helpers are signed with identity `69C168B632296CA0F1D486769206010EE1916033`, hardened runtime, before the agent bundle is signed.
- No em dashes in documents.

## File Structure

| File | Responsibility |
|---|---|
| `~/strafe/Sources/strafe/main.swift` | add `serve` mode: menu-bar app without status item, no permission prompt |
| `crates/openlogi-agent/src/helpers.rs` | `Helper` enum, binary paths, supervision threads, restart/stop |
| `crates/openlogi-agent/src/helpers/autoraise.rs` | AutoRaise settings: pure parse/render + file I/O |
| `crates/openlogi-agent/src/helpers/strafe.rs` | Strafe settings via `defaults`: pure parsing + I/O |
| `crates/openlogi-agent/src/helpers/menu.rs` | `Choice` (menu tag codec) and `apply` |
| `crates/openlogi-agent/src/main.rs` | `mod helpers;` (macOS) |
| `crates/openlogi-agent/src/lifecycle.rs` | call `helpers::spawn()` at arming |
| `crates/openlogi-agent/src/tray.rs` | submenus, selector, stop helpers on Quit |
| `scripts/openlogi-custom.sh` | build/sign/place helpers |
| `docs/CUSTOM-BUILD.md` | document helpers |

---

### Task 1: Strafe `serve` mode

**Files:** Modify `~/strafe/Sources/strafe/main.swift`

- [ ] Step 1: Route `serve` before the CLI switch and thread a `showsStatusItem` flag:

```swift
if args.isEmpty {
    runMenuBarApp(engine: engine, showsStatusItem: true)
} else if args == ["serve"] {
    // Headless helper inside OpenLogi: same services, no menu-bar icon, and no
    // permission prompt (the supervising agent's grant covers this process).
    runMenuBarApp(engine: engine, showsStatusItem: false)
} else {
    exit(runCLI(args, engine: engine))
}
```

`runMenuBarApp(engine:showsStatusItem:)` passes the flag to `AppDelegate(engine:showsStatusItem:)`; `applicationDidFinishLaunching` calls `Permissions.checkAccessibility(prompt: showsStatusItem)` and creates `StatusItemController` only when `showsStatusItem`. Add `strafe serve` to the usage text.

- [ ] Step 2: `swift build -c release --product strafe` succeeds; `.build/release/strafe status` prints status.
- [ ] Step 3: Commit `Add a headless serve mode for embedding in OpenLogi` and push to `NotTanJune/strafe`.

### Task 2: Settings stores (pure, tested)

**Files:** Create `crates/openlogi-agent/src/helpers/autoraise.rs`, `crates/openlogi-agent/src/helpers/strafe.rs`

**Produces:** `autoraise::{Delay, Settings, load, save}`, `strafe::{Speed, speed, set_speed, hotkeys, set_hotkeys}`.

- [ ] Step 1: Write tests first (in each file's `mod tests`): AutoRaise `parse(render(s)) == s` for every delay and both enabled states; `parse("")` is the default (enabled, Short); unknown `delay=3` reads as Short; render contains `ignoreApps="OpenLogi,OpenLogi Overlay"`. Strafe `parse_int("2\n") == Some(2)`, `parse_int("") == None`, `Speed::from_raw` round trip, `parse_bool("0\n") == Some(false)`.
- [ ] Step 2: `cargo test -p openlogi-agent helpers` fails (missing items).
- [ ] Step 3: Implement (code in the files as committed).
- [ ] Step 4: Tests pass. Commit `feat(agent): model the bundled helpers' settings stores`.

### Task 3: Supervisor and spike

**Files:** Create `crates/openlogi-agent/src/helpers.rs`; modify `main.rs`, `lifecycle.rs` (call `helpers::spawn()` after `overlay::spawn()`, macOS only), `tray.rs` (`helpers::stop_all()` beside `overlay::evict_on_quit()`).

**Produces:** `helpers::{Helper, spawn, restart, stop_all}`.

- [ ] Step 1: Implement: one thread per helper; evict a stale process running the same image once (`succession::eviction::evict_anonymous`); loop `if enabled { supervisor.tick(..) } else { sleep 1 s }`; record the child pid on `Event::Started`, clear on `Event::Exited`; `restart` and `stop_all` send SIGTERM via `/bin/kill`.
- [ ] Step 2: Unit test `Helper::binary_beside` layout (`.../OpenLogi Agent.app/Contents/MacOS/openlogi-agent` maps to `.../Contents/Helpers/<name>`).
- [ ] Step 3: Spike (manual, blocking): build helpers, `apply`, then confirm `strafe` and `autoraise` run with the agent as parent (`ps -o ppid`), `/tmp/strafe.sock` answers `ok`, and helper logs show no Accessibility denial. If denied: stop and report.
- [ ] Step 4: Commit `feat(agent): supervise bundled Strafe and AutoRaise helpers`.

### Task 4: Tray submenus

**Files:** Create `crates/openlogi-agent/src/helpers/menu.rs`; modify `tray.rs`.

- [ ] Step 1: Test `Choice::from_tag(c.tag()) == Some(c)` for every choice; unknown tag is `None`.
- [ ] Step 2: Implement `Choice` and `apply(choice)` (write store, `restart(helper)`), the `selectHelperSetting:` method on `MenuTarget` (reads the sender's `tag`, applies, then `relocalize()` to refresh checkmarks), and `add_helper_menus(menu, mtm, target)` called from `build_menu` before Settings.
- [ ] Step 3: `cargo test -p openlogi-agent helpers` and `cargo clippy -p openlogi-agent --all-targets -- -D warnings` pass. Commit `feat(agent): configure bundled helpers from the menu bar`.

### Task 5: Build pipeline

**Files:** Modify `scripts/openlogi-custom.sh`

- [ ] Step 1: `build_helpers`: require `~/strafe` on `fix/macos27-direction-openlogi`, `swift build -c release --product strafe`; `make -C ~/AutoRaise clean AutoRaise`.
- [ ] Step 2: In `cmd_apply` staging: `install -m 0755` both into `$AGENT_REL/Contents/Helpers/`, copy `~/AutoRaise/LICENSE.md` to `$AGENT_REL/Contents/Resources/AutoRaise-LICENSE.md`, sign each helper (`--identifier org.openlogi.helper.<name>`) before the agent bundle.
- [ ] Step 3: `shellcheck`, `shfmt -d` clean. Commit `chore(macos): build and bundle the Strafe and AutoRaise helpers`.

### Task 6: Integration, retire standalone Strafe, docs

- [ ] Step 1: Quit standalone Strafe, `apply`, run the Task 3 spike checks, change each menu setting and confirm only that helper restarts.
- [ ] Step 2: Guard simulation: reinstall stock, wait for re-apply, helpers return.
- [ ] Step 3: Remove Strafe's Login Item, delete `/Applications/strafe.app`, unregister it, `tccutil reset Accessibility com.rileycx.strafe`.
- [ ] Step 4: Update `docs/CUSTOM-BUILD.md`; commit; push both repos.
