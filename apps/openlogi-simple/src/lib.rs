//! OpenLogi Simple: a one-page settings app for the OpenLogi agent.
//!
//! The crate is a library so the core (everything but the window) is testable
//! and linted as public API; `main.rs` is the binary.
//!
//! - [`model`]: the page as data, and the write path for each edit (pure).
//! - [`devices`]: the mice the agent reports, resolved to their config keys.
//! - [`store`]: the saved `config.toml` and its conflict-safe commits.
//! - [`agent`]: the blocking IPC link to the agent.
//! - [`launch`]: starting the agent when it is down.
//! - [`registration`]: `SMAppService` registration of the agent's launchd job.
//! - [`desktop`]: Strafe and AutoRaise settings and restarting those helpers.

#[cfg(not(target_os = "macos"))]
compile_error!(
    "openlogi-simple is a macOS app: registration, launching and the helpers are macOS-only"
);

pub mod agent;
pub mod desktop;
pub mod devices;
pub mod launch;
pub mod model;
pub mod registration;
pub mod store;
