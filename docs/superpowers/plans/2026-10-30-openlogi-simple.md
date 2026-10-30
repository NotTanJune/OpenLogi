# OpenLogi Simple Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A one-page native AppKit settings app replacing the upstream GUI binary in the custom OpenLogi.app.

**Architecture:** Standalone-workspace crate `apps/openlogi-simple`; a pure view model over `openlogi_core::config::Config`; a tokio worker owning the agent connection; an AppKit window on the main thread; installed by `scripts/openlogi-custom.sh`.

**Tech Stack:** Rust 2024, objc2 0.6 / objc2-app-kit 0.3, tokio, tarpc 0.38, openlogi-core, openlogi-ipc.

Spec: `docs/superpowers/specs/2026-10-30-openlogi-simple-design.md`.

## Global Constraints

- Never edit any upstream file (root `Cargo.toml`, `crates/**`, locales). New files only, under `apps/openlogi-simple/`. Exception: none.
- Strafe/AutoRaise settings: include `crates/openlogi-agent/src/helpers/strafe.rs` and `autoraise.rs` with `#[path]`; never copy them.
- Mirror the upstream GUI's write path for every setting; read it in `crates/openlogi-desktop/src/state/` and `features/{mouse,pointer}` before writing ours.
- Every `unsafe` block carries a `// SAFETY:` comment; follow `.agents/rules/objc-ffi.md` (`Retained<T>`, `MainThreadMarker`, no manual retain/release).
- `cargo clippy --all-targets -- -D warnings` and `cargo test` clean inside `apps/openlogi-simple`.
- English UI strings as literals. No em dashes in docs.

## Task 1: Core (no UI)

**Files:** `apps/openlogi-simple/Cargo.toml`, `src/main.rs` (temporary CLI smoke entry), `src/model.rs`, `src/agent.rs`, `src/registration.rs`, `src/launch.rs`, `src/desktop.rs` (Strafe/AutoRaise).

**Produces (exact API Task 2 consumes):**
- `model::Page` built by `model::Page::build(config: &Config, device_key: &str, inventory: Option<&DeviceInventory>) -> Page` with sections of rows; each row `{ id: RowId, label: String, control: Control }` where `Control` is `Popup { choices: Vec<Choice>, selected: usize }`, `Switch(bool)`, `Slider { min, max, value }`, or `Text(String)`.
- `model::apply(config: &mut Config, device_key: &str, edit: Edit) -> Result<(), ModelError>` for binding, gesture, DPI, SmartShift, scroll direction edits.
- `agent::Agent` (owns a tokio runtime thread): `connect()`, `inventory()`, `devices()`, `set_dpi`, `set_smartshift`, `reload_saved_config`, state enum `{ Connected, NotRunning, VersionSkew }`, with retry and launch via `launch::spawn_agent`.
- `registration::ensure_registered()` ported from `crates/openlogi-desktop/src/platform/registration/macos.rs` (keep its tests).
- `desktop::{speed, set_speed, hotkeys, set_hotkeys, autoraise_settings, set_autoraise}` plus `desktop::restart_helper(Helper)` that stops the bundled helper process by its exact bundle path.

- [ ] Write model tests first (rows from a fixture `Config`; each `Edit` round-trips through the core setters), run, implement, pass.
- [ ] Port registration and launch with their tests.
- [ ] Smoke: `cargo run` prints the page model for the live mouse and the agent state.
- [ ] Clippy and tests clean. Commit `feat(simple): core model, agent link, and registration`.

## Task 2: Native window

**Files:** `src/main.rs` (AppKit bootstrap), `src/window.rs`, `src/app_delegate.rs`.

- [ ] `NSApplication` regular activation, app delegate handling `application:openURLs:` (show window) and reopen.
- [ ] Single-instance via `openlogi_core`'s lock as upstream uses it.
- [ ] One `NSWindow` with an `NSScrollView` holding an `NSStackView` of sections; native `NSPopUpButton`, `NSSwitch`, `NSSlider`, `NSTextField` labels; system fonts and colours only.
- [ ] One action target object (`define_class!`) mapping control tags to `model::Edit`, applying via `model::apply`, saving, reloading, and reverting on error with an inline message.
- [ ] Agent state banner with Retry.
- [ ] Clippy and tests clean. Commit `feat(simple): one-page native settings window`.

## Task 3: Install pipeline (lead)

- [ ] `openlogi-custom.sh`: build `apps/openlogi-simple` (release) in `apply`, install as `Contents/MacOS/openlogi-desktop`, sign before the outer bundle; include in the daily check's build.
- [ ] Apply, verify light/dark, exercise controls, guard simulation, docs, push.
