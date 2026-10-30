//! OpenLogi Simple: the one-page native settings window.
//!
//! `openlogi-simple --print` prints the page model instead (see `smoke`).
//!
//! Environment: `OPENLOGI_SIMPLE_DEV=1` skips the single-instance lock, so a
//! development build can run beside the installed app. A binary outside an
//! `.app` bundle never registers the agent's login item, with or without it.

mod app_delegate;
mod smoke;
mod ui;
mod window;

use std::process::ExitCode;

use objc2::MainThreadMarker;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use openlogi_core::single_instance::{self, InstanceError, Role};
use openlogi_simple::agent::Agent;
use openlogi_simple::controller::is_bundled;
use openlogi_simple::registration;
use openlogi_simple::store::Store;
use tracing::{info, warn};

use crate::app_delegate::{AppDelegate, install_main_menu};
use crate::ui::Backend;

fn main() -> ExitCode {
    openlogi_core::logging::init_stderr();
    let result = if std::env::args().any(|argument| argument == "--print") {
        smoke::run()
    } else {
        run()
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let dev = std::env::var_os("OPENLOGI_SIMPLE_DEV").is_some_and(|value| value == "1");
    let bundled = std::env::current_exe().is_ok_and(|executable| is_bundled(&executable));
    // The installed app: it owns the agent's service registration and may start
    // the agent. Nothing else does, whatever the environment says.
    let production = bundled && !dev && !openlogi_core::paths::is_dev_profile();

    let _instance = if dev {
        None
    } else {
        match single_instance::acquire(Role::Gui) {
            Ok(guard) => Some(guard),
            Err(InstanceError::AlreadyRunning { path }) => {
                info!(
                    path = %path.display(),
                    "another OpenLogi window is already running, exiting"
                );
                return Ok(());
            }
            Err(error) => return Err(format!("single-instance check failed: {error}")),
        }
    };

    let mtm = MainThreadMarker::new().ok_or("not on the main thread")?;
    let store = Store::load().map_err(|error| format!("could not read the config: {error}"))?;
    let agent = Agent::new().map_err(|error| format!("could not start the agent link: {error}"))?;

    if production {
        // XPC must not delay the first paint, so it runs off the main thread.
        std::thread::spawn(|| {
            if let Err(error) = registration::ensure_registered() {
                warn!(error, "startup service registration failed");
            }
        });
    }

    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let delegate = AppDelegate::new(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    install_main_menu(mtm, &app);
    ui::start(mtm, Backend::new(agent, store, production));
    // `delegate` stays bound: AppKit holds its delegate weakly.
    app.run();
    drop(delegate);
    Ok(())
}
