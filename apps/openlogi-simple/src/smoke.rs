//! `openlogi-simple --print`: connect to the running agent and print what the
//! page would show for the first mouse, plus the agent state and the desktop
//! helpers' settings.
//!
//! Read-only by construction. It does not register the agent service, launch
//! the agent, save the config, or write to a device.

use openlogi_simple::agent::{Agent, AgentState};
use openlogi_simple::desktop;
use openlogi_simple::model::Page;
use openlogi_simple::store::Store;

/// Print the page model for the first mouse.
pub fn run() -> Result<(), String> {
    let store = Store::load().map_err(|error| format!("could not read the config: {error}"))?;
    let agent = Agent::new().map_err(|error| format!("could not start the agent link: {error}"))?;

    let state = agent.connect();
    println!("agent: {state:?}");
    if state != AgentState::Connected {
        return Ok(());
    }

    let devices = agent
        .devices(store.config())
        .map_err(|error| format!("could not list devices: {error}"))?;
    println!("mice: {}", devices.len());
    let Some(device) = devices.first() else {
        return Ok(());
    };
    println!(
        "device: {} (key {}, {}, battery {})",
        device.name,
        device.key,
        if device.online { "online" } else { "offline" },
        device.battery_label().as_deref().unwrap_or("unknown"),
    );

    let facts = agent.facts(device);
    print!("{}", Page::build(store.config(), &device.key, Some(&facts)));

    println!("Desktop (read only)");
    println!("  Space switching speed: {:?}", desktop::speed());
    println!("  Space hotkeys: {}", desktop::hotkeys());
    let autoraise = desktop::autoraise_settings();
    println!(
        "  Auto-raise: {} ({:?})",
        autoraise.enabled, autoraise.delay
    );
    Ok(())
}
