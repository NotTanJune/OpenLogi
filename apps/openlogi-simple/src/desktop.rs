//! The desktop helpers' settings: Strafe (Space switching) and AutoRaise.
//!
//! The settings code is the agent's own. `strafe.rs` and `autoraise.rs` are
//! compiled into this crate from the agent's source tree, so the app and the
//! agent's menu-bar menu read and write exactly one implementation of each
//! store. After a change the helper is stopped and the agent's supervisor
//! starts it again with the new settings, which is what the menu does
//! (`helpers/menu.rs::apply`).

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use openlogi_core::brand::Helper as AgentHelper;
use tracing::{info, warn};

// The agent's files are compiled unmodified. In the agent's binary crate their
// `pub` items are not exported, so `must_use_candidate` never looks at them;
// here the types are re-exported API and it does.
#[expect(
    clippy::must_use_candidate,
    reason = "the agent's own file, compiled unmodified from a binary crate"
)]
#[path = "../../../crates/openlogi-agent/src/helpers/autoraise.rs"]
mod autoraise;
#[expect(
    clippy::must_use_candidate,
    reason = "the agent's own file, compiled unmodified from a binary crate"
)]
#[path = "../../../crates/openlogi-agent/src/helpers/strafe.rs"]
mod strafe;

pub use autoraise::{Delay, Settings as AutoRaiseSettings};
pub use strafe::Speed;

/// A helper the agent supervises, by the name of its executable in
/// `OpenLogi Agent.app/Contents/Helpers/`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Helper {
    /// Per-display Space switching.
    Strafe,
    /// Raising the window under the pointer.
    AutoRaise,
}

impl Helper {
    const fn name(self) -> &'static str {
        match self {
            Self::Strafe => "strafe",
            Self::AutoRaise => "autoraise",
        }
    }
}

/// The stored Space-switching speed.
#[must_use]
pub fn speed() -> Speed {
    strafe::speed()
}

/// Store the Space-switching speed and restart Strafe so it takes effect.
///
/// # Errors
///
/// The `defaults write` failed; Strafe is left running.
pub fn set_speed(speed: Speed) -> io::Result<()> {
    strafe::set_speed(speed)?;
    restart_helper(Helper::Strafe);
    Ok(())
}

/// Whether the Ctrl+Option+Arrow Space hotkeys are on.
#[must_use]
pub fn hotkeys() -> bool {
    strafe::hotkeys()
}

/// Store the hotkeys setting and restart Strafe so it takes effect.
///
/// # Errors
///
/// The `defaults write` failed; Strafe is left running.
pub fn set_hotkeys(enabled: bool) -> io::Result<()> {
    strafe::set_hotkeys(enabled)?;
    restart_helper(Helper::Strafe);
    Ok(())
}

/// The stored AutoRaise settings; the defaults when nothing is stored yet.
#[must_use]
pub fn autoraise_settings() -> AutoRaiseSettings {
    autoraise::load()
}

/// Store the AutoRaise settings and restart AutoRaise: it reads its file at
/// start, and the supervisor leaves it stopped when it is now disabled.
///
/// # Errors
///
/// The file could not be written; AutoRaise is left running.
pub fn set_autoraise(settings: AutoRaiseSettings) -> io::Result<()> {
    autoraise::save(settings)?;
    restart_helper(Helper::AutoRaise);
    Ok(())
}

/// Stop `helper` so the agent's supervisor starts it again with fresh
/// settings, or leaves it stopped when it is now disabled.
///
/// The process is found by its exact path inside the running bundle, so a
/// same-named program elsewhere is never touched. Returns how many processes
/// were signalled: none when the helper is not running (it is then not
/// restarting for the supervisor to bring back either, and the next start
/// reads the stored settings anyway). Failures are logged; a restart is a
/// convenience, not the write.
pub fn restart_helper(helper: Helper) -> usize {
    let Some(binary) = std::env::current_exe()
        .ok()
        .and_then(|executable| helper_binary(&executable, helper))
    else {
        info!(
            helper = helper.name(),
            "no bundled helper beside this app, nothing to restart"
        );
        return 0;
    };
    let listing = match Command::new("/bin/ps")
        .args(["-axo", "pid=,comm="])
        .output()
    {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).into_owned()
        }
        Ok(output) => {
            warn!(status = %output.status, "ps failed, so the helper was not restarted");
            return 0;
        }
        Err(error) => {
            warn!(%error, "could not run ps, so the helper was not restarted");
            return 0;
        }
    };
    let mut stopped = 0;
    for pid in pids_running(&listing, &binary) {
        match Command::new("/bin/kill")
            .args(["-TERM", &pid.to_string()])
            .status()
        {
            Ok(status) if status.success() => stopped += 1,
            Ok(status) => warn!(pid, %status, "kill refused"),
            Err(error) => warn!(pid, %error, "could not run kill"),
        }
    }
    stopped
}

/// `Contents/Library/LoginItems/OpenLogi Agent.app/Contents/Helpers/<helper>`
/// of the bundle whose `Contents/MacOS` holds `executable`.
///
/// The agent bundle is found the way the agent finds itself for its own
/// supervisor: through [`AgentHelper::executable_candidates`], so every layout
/// the helper has shipped under resolves, and only one that holds the helper
/// counts. `None` for a bare binary outside a bundle.
fn helper_binary(executable: &Path, helper: Helper) -> Option<PathBuf> {
    let app = executable.ancestors().nth(3)?;
    if app.extension()? != "app" {
        return None;
    }
    AgentHelper::Agent
        .executable_candidates()
        .into_iter()
        .filter_map(|relative| {
            let agent_executable = app.join(relative);
            let contents = agent_executable.parent()?.parent()?;
            Some(contents.join("Helpers").join(helper.name()))
        })
        .find(|path| path.is_file())
}

/// The pids in a `ps -axo pid=,comm=` listing whose command is exactly
/// `binary`. `comm` is the executable's full path and may hold spaces, so a
/// line is split once, after the pid.
fn pids_running(listing: &str, binary: &Path) -> Vec<u32> {
    listing
        .lines()
        .filter_map(|line| {
            let (pid, command) = line.trim_start().split_once(char::is_whitespace)?;
            (Path::new(command.trim()) == binary).then(|| pid.parse().ok())?
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const AGENT_APP: &str = "Contents/Library/LoginItems/OpenLogi Agent.app";

    /// A bundle with the helpers in place, as the custom build installs it.
    fn bundle(dir: &Path) -> PathBuf {
        let app = dir.join("OpenLogi.app");
        let helpers = app.join(AGENT_APP).join("Contents/Helpers");
        std::fs::create_dir_all(&helpers).expect("creates the helpers dir");
        std::fs::create_dir_all(app.join("Contents/MacOS")).expect("creates MacOS");
        for name in ["strafe", "autoraise"] {
            std::fs::write(helpers.join(name), "").expect("writes a helper");
        }
        app
    }

    #[test]
    fn helpers_resolve_inside_the_agent_bundle_of_the_running_app() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let app = bundle(dir.path());
        let executable = app.join("Contents/MacOS/openlogi-desktop");

        assert_eq!(
            helper_binary(&executable, Helper::Strafe),
            Some(app.join(AGENT_APP).join("Contents/Helpers/strafe"))
        );
        assert_eq!(
            helper_binary(&executable, Helper::AutoRaise),
            Some(app.join(AGENT_APP).join("Contents/Helpers/autoraise"))
        );
    }

    #[test]
    fn no_helper_resolves_without_a_bundle_or_a_helper_file() {
        let dir = tempfile::tempdir().expect("a temp dir");
        // A bare cargo binary is not in a bundle.
        assert_eq!(
            helper_binary(
                &dir.path().join("target/debug/openlogi-simple"),
                Helper::Strafe
            ),
            None
        );
        // A bundle whose agent carries no helper has nothing to restart.
        let app = dir.path().join("Empty.app");
        std::fs::create_dir_all(app.join(AGENT_APP).join("Contents/MacOS")).expect("creates dirs");
        assert_eq!(
            helper_binary(
                &app.join("Contents/MacOS/openlogi-desktop"),
                Helper::AutoRaise
            ),
            None
        );
    }

    #[test]
    fn only_the_exact_path_matches_a_listing_line() {
        let binary = Path::new(
            "/Applications/OpenLogi.app/Contents/Library/LoginItems/OpenLogi Agent.app/Contents/Helpers/strafe",
        );
        let listing = "\
    1 /sbin/launchd
20874 /Applications/OpenLogi.app/Contents/Library/LoginItems/OpenLogi Agent.app/Contents/Helpers/strafe
20875 /Applications/OpenLogi.app/Contents/Library/LoginItems/OpenLogi Agent.app/Contents/Helpers/autoraise
30001 /Users/me/other/Helpers/strafe
30002 /Applications/OpenLogi.app/Contents/Library/LoginItems/OpenLogi Agent.app/Contents/Helpers/strafe-extra
 4242 /Applications/OpenLogi.app/Contents/Library/LoginItems/OpenLogi Agent.app/Contents/Helpers/strafe
not-a-pid /Applications/OpenLogi.app/Contents/Library/LoginItems/OpenLogi Agent.app/Contents/Helpers/strafe
";
        assert_eq!(pids_running(listing, binary), [20874, 4242]);
        assert_eq!(pids_running("", binary), Vec::<u32>::new());
    }
}
