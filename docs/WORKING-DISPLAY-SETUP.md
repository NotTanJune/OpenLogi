# Working mouse gestures and virtual display

Recorded on 2026-10-26, Asia/Singapore, after the user confirmed that left/right
mouse gestures worked on the second monitor. Host: macOS 27.0.1 (26A434), arm64.

## Preserved components

| Component | Working installed location | SHA-256 of executable |
|---|---|---|
| OpenLogi agent | `/Applications/OpenLogi.app/Contents/Library/LoginItems/OpenLogi Agent.app/Contents/MacOS/openlogi-agent` | `387752ba3a99d2e5d8140a7eb5522401201a60084b8ffc717d8e519e19cc3960` |
| Strafe | `/Applications/strafe.app/Contents/MacOS/strafe` | `e657036d621187682c834b1bc12141c757db45d8ad827fd10a36cae973241dbf` |
| Virtual-display watchdog | `~/Library/Application Support/dl120/dl120` | `357f76b77a79df43af916c51a3ce8555c5690aa68de2c457b8f9cb4d46270971` |

`artifacts/OpenLogi.app` is an exact copy of this installed app, including its
signatures. All 17 files, totaling 42,496,030 bytes, matched by size and SHA-256.
The agent's designated requirement is the existing ad-hoc cdhash
`33a6ee7f173739e594ca43cfe8b66c20417ee5b5`.

OpenLogi source baseline: `e77ccf6af14bbcee2a3809f21e5998b672467706`, with the
Strafe retry fix in `crates/openlogi-inject/src/inject/macos.rs`.
Strafe source baseline: `71edc8854db1dcb7188a7e50c637094c60cfdaf4`, with its
existing `CGDisplayPrimaryDisplay` mirror lookup edit. Both source patches
are preserved in the private snapshot below. These are baseline revisions,
not claims that the working patches have been committed or pushed.

## Routing and display layout

The MX Master 3S gesture bindings use `PreviousDesktop` and `NextDesktop`.
OpenLogi sends `switch left` / `switch right` to `/tmp/strafe.sock`.
After connecting, a rejected request or missing acknowledgement must not
trigger a global keyboard shortcut. A rejection can mean the pointer's display
is already at its first or last Space. Retrying globally can switch the main
monitor instead. The working agent logs `suppressing native retry` in this case.

The DisplayLink output mirrors a `dl120` virtual display. The virtual display
owns its Spaces. Strafe resolves the pointer's display to the mirror set's
primary before matching its UUID to the Space list.

| Role | Display UUID | Bounds in points |
|---|---|---|
| Main | `F11D1367-F862-4507-8477-510BB4470C1E` | `(0, 0)`, 2560 × 1440 |
| Physical DisplayLink mirror | `C032CD27-2CEF-48CE-8531-00C79594ACAB` | `(2560, 237)`, 1920 × 1080 |
| Virtual mirror source | `C72980B8-5B34-4F09-A6D7-2AC0E06D2DD5` | `(2560, 237)`, 1920 × 1080 |

Numeric display IDs changed from 49/50 to 51/52 during this session. They are
not persistent identifiers and must not be hardcoded.
Strafe's stored `transitionSpeed` is `0`; its complete preferences are saved.

## Login, updates, and permissions

System Settings > General > Login Items already lists **OpenLogi.app**,
**strafe.app**, and **DisplayLink Manager.app** under Open at Login.
Their background activity is enabled. Preserve these existing entries.

OpenLogi's own `launch_at_login` setting is `false`: macOS's Open at Login
entry supplies startup instead. No second OpenLogi LaunchAgent was added.
`check_for_updates = true`, but `auto_install_updates = false`, so automatic
installation will not replace this custom build. A deliberate manual update
can still replace it. Strafe has no auto-updater in this source version.

`~/Library/LaunchAgents/com.nottanjune.dl120.plist` has `RunAtLoad = true` and
`KeepAlive = true`; `launchctl print gui/501/com.nottanjune.dl120` reported it
running. Background activity for `dl120` is enabled.
The separate `com.nottanjune.displaylink-restart` job is registered for 02:00
daily and was idle when recorded. Its script and plist are also preserved.

Current System Settings observations:

- **Input Monitoring:** OpenLogi Agent is on.
- **Device Control and Data Access** (Accessibility): OpenLogi Agent and Strafe
  are on.
- The agent logged `control capture active` after its Input Monitoring entry
  was removed, re-added for the installed helper, and restarted.

An old enabled toggle initially still denied the rebuilt agent. Preserve these
exact signed bundles; rebuilding or re-signing changes their ad-hoc identity
and can require a fresh permission grant. TCC databases and authentication
credentials are not part of the snapshot. Do not reset permissions on a working
installation merely because an old error remains in the daily log.

## Private snapshot and verification

The durable local snapshot is outside Cargo's build directory:

```text
.local-state/working-display-2026-10-26-002448/
```

It contains both signed apps, `dl120` binary and source, the DisplayLink restart
script, both LaunchAgent plists, the exact OpenLogi configuration, Strafe
preferences, both source patches, and `manifest.json`. `.local-state/` is
Git-ignored and restricted to this user. It is a local recovery copy on the same
disk, not an off-device backup. Every copied top-level item was independently
checked by file count, byte count, and all file hashes against its source.

From the repository root, run this read-only check:

```sh
python3 scripts/verify-working-display-setup.py \
  .local-state/working-display-2026-10-26-002448 --installed
codesign --verify --deep --strict artifacts/OpenLogi.app
```

The verifier checks every snapshot entry. `--installed` also compares each
copied item's original path; exported preferences and source patches are
checked as snapshot files, without executing their recorded export commands.
An intentional configuration edit will report drift and needs a new snapshot.

For recovery, verify the snapshot first, stop the affected app, and restore
the exact saved bundle to its original location without re-signing it. Restore
configuration or helper files only when those are the damaged components.
Launch the installed app through LaunchServices and check its new startup log.
If macOS rejects an otherwise verified saved identity, re-add that installed
helper in the relevant permission pane and restart it. Restoring files cannot
guarantee restoration of an OS permission decision.

## Evidence and limits

- User confirmed physical mouse gestures work after the installation.
- A live Strafe round trip changed only the second monitor's Space, then
  restored both desktops and the pointer.
- The installed agent log records native retries being suppressed.
- Injector tests: 19 passed. Injector Clippy with warnings denied passed.
- The release build and installed/artifact signature checks passed.
- Full-workspace formatting still reports unrelated existing import formatting
  in `crates/openlogi-desktop/src/windows/settings/diagnostics.rs`.
- Login settings and jobs were inspected; logout/reboot was not performed.
  This snapshot does not promise compatibility with future macOS updates.
