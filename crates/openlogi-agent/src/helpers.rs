//! Supervision of the custom build's bundled helpers: Strafe (per-display
//! Space switching behind `/tmp/strafe.sock`) and AutoRaise (raise the window
//! under the pointer).
//!
//! Both are separate executables in `OpenLogi Agent.app/Contents/Helpers/`
//! (AutoRaise is GPL-3.0 and must stay a separate program). Started as this
//! agent's children, they are attributed to it by macOS and run on its
//! Accessibility grant. Neither claims a `succession` role, so the role always
//! reads free and the supervisor reduces to start, wait, back off, restart.
//! A helper left behind by a previous agent run is evicted by its image once,
//! before the first start.

pub mod autoraise;
pub mod menu;
pub mod strafe;

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use succession::eviction::{self, AnonymousOutcome, Policy};
use succession::supervision::{Event, Restart, Supervisor};
use succession::{Role, Run};
use tracing::{info, warn};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Helper {
    Strafe,
    AutoRaise,
}

impl Helper {
    const ALL: [Self; 2] = [Self::Strafe, Self::AutoRaise];

    const fn name(self) -> &'static str {
        match self {
            Self::Strafe => "strafe",
            Self::AutoRaise => "autoraise",
        }
    }

    const fn args(self) -> &'static [&'static str] {
        match self {
            Self::Strafe => &["serve"],
            Self::AutoRaise => &[],
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Strafe => 0,
            Self::AutoRaise => 1,
        }
    }

    fn enabled(self) -> bool {
        match self {
            Self::Strafe => true,
            Self::AutoRaise => autoraise::load().enabled,
        }
    }

    /// `Contents/Helpers/<name>` of the bundle whose `Contents/MacOS` holds
    /// `executable`.
    fn binary_beside(self, executable: &Path) -> Option<PathBuf> {
        let contents = executable.parent()?.parent()?;
        Some(contents.join("Helpers").join(self.name()))
    }

    fn binary(self) -> Option<PathBuf> {
        let executable = std::env::current_exe().ok()?;
        self.binary_beside(&executable)
            .filter(|path| path.is_file())
    }
}

/// Live child pids, so a settings change or Quit can stop a helper.
static PIDS: Mutex<[Option<u32>; 2]> = Mutex::new([None, None]);

fn set_pid(helper: Helper, pid: Option<u32>) {
    if let Ok(mut pids) = PIDS.lock() {
        pids[helper.index()] = pid;
    }
}

fn pid(helper: Helper) -> Option<u32> {
    PIDS.lock().ok().and_then(|pids| pids[helper.index()])
}

const RESTART: Restart = Restart {
    base: Duration::from_secs(1),
    ceiling: Duration::from_secs(60),
    healthy_after: Duration::from_secs(30),
};

/// Start one supervisor thread per bundled helper. Called once, at arming.
pub fn spawn() {
    for helper in Helper::ALL {
        let Some(binary) = helper.binary() else {
            info!(
                helper = helper.name(),
                "bundled helper not present — not started"
            );
            continue;
        };
        let Ok(directory) = openlogi_core::paths::config_dir() else {
            warn!("could not resolve the config directory — helpers disabled");
            return;
        };
        if helper == Helper::AutoRaise {
            // Rewrite the file from the stored settings, so a file written by
            // an older build (or none at all) carries this build's fixed lines.
            if let Err(error) = autoraise::save(autoraise::load()) {
                warn!(%error, "could not write the AutoRaise config");
            }
        }
        let result = std::thread::Builder::new()
            .name(format!("openlogi-{}-supervisor", helper.name()))
            .spawn(move || supervise(helper, &binary, &directory));
        if let Err(error) = result {
            warn!(%error, helper = helper.name(), "could not start the helper supervisor");
        }
    }
}

fn supervise(helper: Helper, binary: &Path, directory: &Path) -> ! {
    evict_stale(helper, binary);
    let role = Role::new(directory, &format!("helper-{}", helper.name()));
    let mut supervisor = Supervisor::new(role, Run::mint()).restart(RESTART);
    let mut spawn = || {
        let log = log_file(helper);
        let mut command = Command::new(binary);
        command.args(helper.args()).stdin(Stdio::null());
        if let Some(log) = log {
            if let Ok(copy) = log.try_clone() {
                command.stdout(copy);
            }
            command.stderr(log);
        }
        command.spawn()
    };
    loop {
        if !helper.enabled() {
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        let mut report = |event: Event<'_>| match event {
            Event::Started(pid) => {
                set_pid(helper, Some(pid));
                info!(helper = helper.name(), "{event}");
            }
            Event::Exited { .. } => {
                set_pid(helper, None);
                info!(helper = helper.name(), "{event}");
            }
            Event::SpawnFailed(_) => warn!(helper = helper.name(), "{event}"),
            _ => tracing::debug!(helper = helper.name(), "{event}"),
        };
        if let Err(error) = supervisor.tick(&mut spawn, &mut report) {
            warn!(%error, helper = helper.name(), "could not read the helper role");
        }
    }
}

fn log_file(helper: Helper) -> Option<std::fs::File> {
    let directory = PathBuf::from(std::env::var_os("HOME")?).join(".local/state/openlogi");
    std::fs::create_dir_all(&directory).ok()?;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join(format!("helper-{}.log", helper.name())))
        .ok()
}

/// A helper started by a previous agent run outlives it; two copies would
/// fight over the Strafe socket and event taps.
fn evict_stale(helper: Helper, binary: &Path) {
    match eviction::evict_anonymous(binary, &Policy::default()) {
        AnonymousOutcome::NoCandidate => {}
        outcome => info!(
            helper = helper.name(),
            ?outcome,
            "evicted a helper left by a previous run"
        ),
    }
}

fn terminate(helper: Helper) {
    if let Some(pid) = pid(helper) {
        let _ = Command::new("/bin/kill")
            .args(["-TERM", &pid.to_string()])
            .status();
    }
}

/// Stop `helper` so its supervisor starts it again with fresh settings, or
/// leaves it stopped when it is now disabled.
pub fn restart(helper: Helper) {
    terminate(helper);
}

/// Stop every helper on the way out of a deliberate agent shutdown.
pub fn stop_all() {
    for helper in Helper::ALL {
        terminate(helper);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers_live_in_the_agent_bundle() {
        let agent = Path::new(
            "/Applications/OpenLogi.app/Contents/Library/LoginItems/OpenLogi Agent.app/Contents/MacOS/openlogi-agent",
        );
        assert_eq!(
            Helper::Strafe.binary_beside(agent),
            Some(PathBuf::from(
                "/Applications/OpenLogi.app/Contents/Library/LoginItems/OpenLogi Agent.app/Contents/Helpers/strafe"
            ))
        );
        assert_eq!(
            Helper::AutoRaise
                .binary_beside(agent)
                .and_then(|path| path.file_name().map(ToOwned::to_owned)),
            Some("autoraise".into())
        );
    }
}
