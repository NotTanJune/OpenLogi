//! The window's behaviour: state, actions, and the worker threads behind them.
//!
//! All state lives in one [`Ui`] on the main thread. Anything that can block
//! (the agent, the config file, the desktop helpers' `defaults` reads) runs on
//! a worker thread and hands plain data back through [`post`], which runs a
//! closure on the main queue. The write order for an edit is not decided here:
//! [`apply_edit`] owns it.
//!
//! Redraw rules, so nothing moves under the user's pointer:
//!
//! - A change of shape (another mouse, the agent going away) rebuilds the
//!   controls; a change of values only updates the controls that differ from
//!   what they should show, so a reverted edit resets its control the same way.
//! - Nothing is touched while a slider is being dragged, and a poll result that
//!   arrives while an edit is in flight updates the state but not the controls.

#![expect(
    unsafe_code,
    reason = "objc2 calls: super-init and the action target class"
)]

use std::cell::RefCell;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSApplication, NSControl, NSEventType, NSPopUpButton, NSScreen};
use objc2_foundation::NSSize;
use openlogi_core::config::Config;
use tracing::warn;

use openlogi_simple::agent::{Agent, AgentError, AgentState};
use openlogi_simple::controller::{
    DesktopEdit, DesktopRow, DesktopSettings, EditError, Outcome, agent_notice, apply_edit,
    facts_after,
};
use openlogi_simple::desktop::{Delay, Speed};
use openlogi_simple::devices::Device;
use openlogi_simple::layout::{Layout, Target, View, Widget, value_text};
use openlogi_simple::model::{Edit, Facts, Page, RowId, dpi_edit, smartshift_edit};
use openlogi_simple::store::Store;

use crate::window::{DEVICE_TAG, Rendered, Shell, present};

/// How often the worker polls the agent for battery and online state.
const POLL: Duration = Duration::from_secs(3);
/// How long starting the agent may take before its window says it is not up.
const LAUNCH_WAIT: Duration = Duration::from_secs(8);
/// How long the first load may take before the window appears anyway.
const REVEAL_AFTER: Duration = Duration::from_millis(1500);

/// What worker threads share: the agent link and the saved config.
pub struct Backend {
    agent: Agent,
    store: Mutex<Store>,
    selected: Mutex<Option<String>>,
    /// Whether this process is the installed app, which may start the agent
    /// and ask it to re-read the config. A bare binary never does either.
    production: bool,
}

impl Backend {
    /// A backend over `agent` and `store`.
    pub fn new(agent: Agent, store: Store, production: bool) -> Arc<Self> {
        Arc::new(Self {
            agent,
            store: Mutex::new(store),
            selected: Mutex::new(None),
            production,
        })
    }

    fn store(&self) -> MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn config(&self) -> Config {
        self.store().config().clone()
    }

    fn selected(&self) -> Option<String> {
        self.selected
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn select(&self, key: Option<String>) {
        *self.selected.lock().unwrap_or_else(PoisonError::into_inner) = key;
    }
}

/// How far the device list is known, which decides whether an empty list means
/// "no mouse".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Inventory {
    /// The first load has not finished.
    Loading,
    /// The agent is still scanning, so an empty list says nothing yet.
    Scanning,
    /// The list is the agent's answer.
    Ready,
}

/// What a full load found.
struct Loaded {
    agent: AgentState,
    scanning: bool,
    devices: Vec<Device>,
    selected: usize,
    facts: Facts,
    config: Config,
    desktop: DesktopSettings,
}

/// What a poll found, while the agent stayed connected.
struct Refreshed {
    devices: Vec<Device>,
    /// The mouse the poll chose and, when it read them, its facts.
    chosen: usize,
    chosen_key: Option<String>,
    facts: Option<Facts>,
}

struct State {
    agent: AgentState,
    devices: Vec<Device>,
    selected: usize,
    facts: Facts,
    config: Config,
    desktop: DesktopSettings,
}

/// The window and its state. Lives on the main thread, in [`UI`].
pub struct Ui {
    mtm: MainThreadMarker,
    backend: Arc<Backend>,
    shell: Shell,
    target: Retained<ActionTarget>,
    state: State,
    /// The layout currently drawn, to tell a change of shape from a change of
    /// values.
    layout: Layout,
    rendered: Option<Rendered>,
    /// How far the device list is known.
    inventory: Inventory,
    /// Edits in flight on worker threads.
    busy: usize,
    /// A slider is being dragged.
    dragging: bool,
    /// The window has been sized to its content.
    sized: bool,
    /// The window has been shown by the first load or its timeout.
    revealed: bool,
}

thread_local! {
    /// The window state, where the main queue's closures and the action target
    /// can reach it. A `thread_local` because everything that touches it runs
    /// on the main thread that created it.
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

/// Run `work` on the UI, if it exists and is not already borrowed.
fn with_ui(work: impl FnOnce(&mut Ui)) {
    UI.with(|slot| {
        if let Ok(mut slot) = slot.try_borrow_mut()
            && let Some(ui) = slot.as_mut()
        {
            work(ui);
        }
    });
}

/// Run `work` on the main thread against the UI, from any thread.
fn post(work: impl FnOnce(&mut Ui) + Send + 'static) {
    DispatchQueue::main().exec_async(move || with_ui(work));
}

/// Build the window and start loading. The window appears when the first load
/// lands, or after a moment if the agent is slow, so it never opens empty and
/// then jumps.
pub fn start(mtm: MainThreadMarker, backend: Arc<Backend>) {
    let target = ActionTarget::new(mtm);
    let shell = Shell::new(mtm, &target);
    let config = backend.config();
    let state = State {
        agent: AgentState::NotRunning,
        devices: Vec::new(),
        selected: 0,
        facts: Facts::default(),
        config,
        desktop: DesktopSettings {
            speed: Speed::Instant,
            hotkeys: false,
            autoraise: openlogi_simple::desktop::AutoRaiseSettings::default(),
        },
    };
    let ui = Ui {
        mtm,
        backend: Arc::clone(&backend),
        shell,
        target,
        state,
        layout: Layout {
            header: None,
            note: None,
            sections: Vec::new(),
        },
        rendered: None,
        inventory: Inventory::Loading,
        busy: 0,
        dragging: false,
        sized: false,
        revealed: false,
    };
    UI.with_borrow_mut(|slot| *slot = Some(ui));

    spawn_load(Arc::clone(&backend), true, true);
    spawn_poll(backend);
    std::thread::spawn(|| {
        std::thread::sleep(REVEAL_AFTER);
        post(Ui::reveal);
    });
}

/// The window, for the app delegate.
pub fn window() -> Option<Retained<objc2_app_kit::NSWindow>> {
    UI.with(|slot| {
        slot.try_borrow()
            .ok()
            .and_then(|slot| slot.as_ref().map(|ui| ui.shell.window.clone()))
    })
}

/// Connect (starting the agent when `launch`), then read everything the page
/// shows, on a worker thread.
fn spawn_load(backend: Arc<Backend>, launch: bool, first: bool) {
    std::thread::spawn(move || {
        let loaded = load(&backend, launch, first);
        post(move |ui| ui.loaded(loaded));
    });
}

fn load(backend: &Backend, launch: bool, first: bool) -> Loaded {
    let agent = if launch && backend.production {
        backend.agent.connect_or_launch(LAUNCH_WAIT)
    } else {
        backend.agent.connect()
    };
    if first
        && backend.production
        && agent == AgentState::Connected
        && let Err(error) = backend.agent.reload_config()
    {
        warn!(%error, "the agent did not re-read the config at startup");
    }
    let desktop = DesktopSettings::read();
    let config = backend.config();
    let mut scanning = false;
    let mut devices = Vec::new();
    if agent == AgentState::Connected {
        match backend.agent.devices(&config) {
            Ok(list) => devices = list,
            Err(AgentError::Inventory(_)) => scanning = true,
            Err(error) => warn!(%error, "could not list devices"),
        }
    }
    let selected = position_of(&devices, backend.selected().as_deref()).unwrap_or(0);
    let facts = devices
        .get(selected)
        .map(|device| backend.agent.facts(device))
        .unwrap_or_default();
    backend.select(devices.get(selected).map(|device| device.key.clone()));
    Loaded {
        agent,
        scanning,
        devices,
        selected,
        facts,
        config,
        desktop,
    }
}

fn position_of(devices: &[Device], key: Option<&str>) -> Option<usize> {
    let key = key?;
    devices.iter().position(|device| device.key == key)
}

/// Poll the agent for battery and online state, on its own thread. A mouse's
/// facts are read again only when it comes back online or another is chosen,
/// so the poll does not keep waking the device.
fn spawn_poll(backend: Arc<Backend>) {
    std::thread::spawn(move || {
        let mut read_for: Option<(String, bool)> = None;
        loop {
            std::thread::sleep(POLL);
            if backend.agent.state() != AgentState::Connected {
                let state = backend.agent.connect();
                if state == AgentState::Connected {
                    let loaded = load(&backend, false, false);
                    read_for = None;
                    post(move |ui| ui.loaded(loaded));
                } else {
                    post(move |ui| ui.agent_state(state));
                }
                continue;
            }
            let config = backend.config();
            match backend.agent.devices(&config) {
                Ok(devices) => {
                    let wanted = backend.selected();
                    let chosen = position_of(&devices, wanted.as_deref()).unwrap_or(0);
                    let Some(device) = devices.get(chosen) else {
                        post(move |ui| {
                            ui.refreshed(Refreshed {
                                devices,
                                chosen: 0,
                                chosen_key: None,
                                facts: None,
                            });
                        });
                        continue;
                    };
                    let seen = (device.key.clone(), device.online);
                    let facts =
                        (read_for.as_ref() != Some(&seen)).then(|| backend.agent.facts(device));
                    if facts.is_some() {
                        read_for = Some(seen);
                    }
                    backend.select(Some(device.key.clone()));
                    let chosen_key = Some(device.key.clone());
                    post(move |ui| {
                        ui.refreshed(Refreshed {
                            devices,
                            chosen,
                            chosen_key,
                            facts,
                        });
                    });
                }
                Err(AgentError::Inventory(_)) => {}
                Err(_) => {
                    let state = backend.agent.state();
                    post(move |ui| ui.agent_state(state));
                }
            }
        }
    });
}

impl Ui {
    fn layout(&self) -> Layout {
        Layout::build(&View {
            agent: self.state.agent,
            devices: &self.state.devices,
            selected: self.state.selected,
            facts: &self.state.facts,
            config: &self.state.config,
            desktop: self.state.desktop,
            loading: self.inventory != Inventory::Ready,
        })
    }

    fn selected_device(&self) -> Option<&Device> {
        self.state.devices.get(self.state.selected)
    }

    fn action_target(&self) -> &AnyObject {
        &self.target
    }

    /// Redraw every control from the state.
    fn render(&mut self) {
        let layout = self.layout();
        self.rendered = Some(self.shell.fill(self.mtm, &layout, self.action_target()));
        self.layout = layout;
        self.update_banner();
    }

    /// Bring the controls in line with the state: rebuild when the shape
    /// changed, otherwise update only the controls that differ, except `skip`.
    fn refresh_view(&mut self, skip: Option<usize>) {
        if self.dragging {
            return;
        }
        let layout = self.layout();
        if self.rendered.is_none() || !layout.same_shape(&self.layout) {
            self.render();
            return;
        }
        if let Some(rendered) = &self.rendered {
            if let (Some(header), Some(status)) = (&layout.header, &rendered.status) {
                status.setStringValue(&objc2_foundation::NSString::from_str(&header.status));
            }
            let specs = layout.sections.iter().flat_map(|section| &section.rows);
            for (position, (spec, built)) in specs.zip(&rendered.rows).enumerate() {
                if Some(position) != skip && built.handle.differs(&spec.widget, spec.enabled) {
                    built.handle.set(&spec.widget, spec.enabled);
                }
            }
        }
        self.layout = layout;
        self.update_banner();
    }

    fn update_banner(&self) {
        if self.inventory == Inventory::Loading {
            self.shell.set_banner(Some((
                "Connecting to OpenLogi's background service...",
                false,
            )));
        } else if let Some(notice) = agent_notice(self.state.agent) {
            self.shell.set_banner(Some((&notice.message, notice.retry)));
        } else {
            self.shell.set_banner(None);
        }
    }

    fn size_to_content(&mut self) {
        if self.sized {
            return;
        }
        self.sized = true;
        let wanted = self.shell.content_height() + self.shell.chrome_height();
        let limit = NSScreen::mainScreen(self.mtm)
            .map_or(800.0, |screen| screen.visibleFrame().size.height * 0.85);
        self.shell
            .window
            .setContentSize(NSSize::new(460.0, wanted.clamp(300.0, limit)));
        self.shell.window.center();
    }

    fn reveal(&mut self) {
        if self.revealed {
            return;
        }
        self.revealed = true;
        present(self.mtm, &self.shell.window);
    }

    fn loaded(&mut self, loaded: Loaded) {
        self.inventory = if loaded.scanning {
            Inventory::Scanning
        } else {
            Inventory::Ready
        };
        self.state = State {
            agent: loaded.agent,
            devices: loaded.devices,
            selected: loaded.selected,
            facts: loaded.facts,
            config: loaded.config,
            desktop: loaded.desktop,
        };
        self.shell.set_retry_enabled(true);
        self.render();
        self.size_to_content();
        self.reveal();
    }

    fn agent_state(&mut self, agent: AgentState) {
        if agent == self.state.agent {
            return;
        }
        self.state.agent = agent;
        self.shell.set_retry_enabled(true);
        self.render();
    }

    fn refreshed(&mut self, refreshed: Refreshed) {
        let current = self.selected_device().map(|device| device.key.clone());
        self.state.devices = refreshed.devices;
        self.inventory = Inventory::Ready;
        let kept = current.and_then(|key| {
            position_of(&self.state.devices, Some(&key)).map(|position| (key, position))
        });
        if let Some((key, position)) = kept {
            self.state.selected = position;
            if refreshed.chosen_key.as_deref() == Some(&key)
                && let Some(facts) = refreshed.facts
            {
                self.state.facts = facts;
            }
        } else {
            self.state.selected = refreshed.chosen;
            self.state.facts = refreshed.facts.unwrap_or_else(|| {
                self.selected_device()
                    .map(Device::facts)
                    .unwrap_or_default()
            });
        }
        if self.busy == 0 {
            self.refresh_view(None);
        }
    }

    fn select_device(&mut self, position: usize) {
        let Some(device) = self.state.devices.get(position).cloned() else {
            return;
        };
        self.state.selected = position;
        self.state.facts = device.facts();
        self.backend.select(Some(device.key.clone()));
        // Rebuilding here would remove the picker whose action is still
        // running, so it waits for the action to return.
        post(Ui::render);
        let backend = Arc::clone(&self.backend);
        std::thread::spawn(move || {
            let facts = backend.agent.facts(&device);
            let key = device.key;
            post(move |ui| ui.facts_ready(&key, facts));
        });
    }

    fn facts_ready(&mut self, key: &str, facts: Facts) {
        if self.selected_device().map(|device| device.key.as_str()) != Some(key) {
            return;
        }
        self.state.facts = facts;
        self.refresh_view(None);
    }

    fn retry(&mut self) {
        self.shell.set_retry_enabled(false);
        spawn_load(Arc::clone(&self.backend), true, false);
    }

    fn on_changed(&mut self, sender: &NSControl) {
        let tag = sender.tag();
        if tag == DEVICE_TAG {
            let position = sender
                .downcast_ref::<NSPopUpButton>()
                .and_then(|popup| usize::try_from(popup.indexOfSelectedItem()).ok());
            if let Some(position) = position {
                self.select_device(position);
            }
            return;
        }
        let Ok(position) = usize::try_from(tag) else {
            return;
        };
        let Some(built) = self.rendered.as_ref().and_then(|r| r.rows.get(position)) else {
            return;
        };
        let target = built.target;
        let value = Value {
            popup: built.handle.popup_index(),
            on: built.handle.switch_on(),
            slider: built.handle.slider_value(),
        };
        match target {
            Target::Row(id) => self.edit_row(position, id, value),
            Target::Desktop(row) => self.edit_desktop(row, value),
        }
    }

    fn edit_row(&mut self, position: usize, id: RowId, value: Value) {
        let Some(device) = self.selected_device().cloned() else {
            return;
        };
        if id == RowId::Dpi {
            let Some(raw) = value.slider else {
                return;
            };
            let edit = dpi_edit(raw, Some(&self.state.facts));
            if let Edit::Dpi(snapped) = &edit {
                self.show_slider_value(position, f32::from(*snapped));
            }
            if self.commit_slider_now() {
                self.start_edit(device, edit);
            }
            return;
        }
        let facts = &self.state.facts;
        let edit = match id {
            RowId::Button(_) | RowId::Flick(_) => {
                let page = Page::build(&self.state.config, &device.key, Some(facts));
                value
                    .popup
                    .and_then(|index| page.row(id)?.choice_edit(index))
            }
            RowId::Dpi => None,
            RowId::SmartShift => value
                .on
                .map(|on| smartshift_edit(&self.state.config, &device.key, Some(facts), on)),
            RowId::InvertScroll => value.on.map(Edit::InvertScroll),
        };
        if let Some(edit) = edit {
            self.start_edit(device, edit);
        }
    }

    fn show_slider_value(&self, position: usize, value: f32) {
        if let Some(built) = self.rendered.as_ref().and_then(|r| r.rows.get(position))
            && let Some(Widget::Slider { unit, .. }) = self
                .layout
                .sections
                .iter()
                .flat_map(|section| &section.rows)
                .nth(position)
                .map(|spec| &spec.widget)
        {
            built.handle.set_slider_label(&value_text(value, unit));
        }
    }

    /// Whether the slider action in progress ends the drag. While the mouse is
    /// down or dragging the label follows, and nothing is written yet.
    fn commit_slider_now(&mut self) -> bool {
        let event = NSApplication::sharedApplication(self.mtm)
            .currentEvent()
            .map(|event| event.r#type());
        self.dragging = matches!(
            event,
            Some(NSEventType::LeftMouseDown | NSEventType::LeftMouseDragged)
        );
        !self.dragging
    }

    fn start_edit(&mut self, device: Device, edit: Edit) {
        self.busy += 1;
        self.shell.set_notice(None);
        let backend = Arc::clone(&self.backend);
        std::thread::spawn(move || {
            let (result, config) = {
                let mut store = backend.store();
                let result = apply_edit(&backend.agent, &mut store, &device, edit.clone());
                (result, store.config().clone())
            };
            post(move |ui| ui.finish_edit(&edit, result, config));
        });
    }

    fn finish_edit(&mut self, edit: &Edit, result: Result<Outcome, EditError>, config: Config) {
        self.busy = self.busy.saturating_sub(1);
        self.state.config = config;
        match result {
            Ok(outcome) => {
                match &outcome {
                    Outcome::Applied | Outcome::AdoptedNewerFile => {}
                    Outcome::SavedOnly(saved) => self.shell.set_notice(Some(&saved.message())),
                }
                if !matches!(outcome, Outcome::AdoptedNewerFile) {
                    self.state.facts = facts_after(&self.state.facts, edit);
                }
            }
            Err(error) => self.shell.set_notice(Some(&error.message())),
        }
        // Control values come from the state either way: a refused edit left
        // the state alone, so this puts its control back.
        self.refresh_view(None);
    }

    fn edit_desktop(&mut self, row: DesktopRow, value: Value) {
        let edit = match row {
            DesktopRow::Speed => value
                .popup
                .and_then(|index| Speed::ALL.get(index))
                .copied()
                .map(DesktopEdit::Speed),
            DesktopRow::Hotkeys => value.on.map(DesktopEdit::Hotkeys),
            DesktopRow::AutoRaise => value.on.map(DesktopEdit::AutoRaise),
            DesktopRow::Delay => value
                .popup
                .and_then(|index| Delay::ALL.get(index))
                .copied()
                .map(DesktopEdit::Delay),
        };
        let Some(edit) = edit else {
            return;
        };
        self.busy += 1;
        self.shell.set_notice(None);
        // Shown at once, so the Delay popup follows Auto-raise without waiting.
        self.state.desktop = self.state.desktop.with(edit);
        self.refresh_view(None);
        std::thread::spawn(move || {
            let error = DesktopSettings::write(edit)
                .err()
                .map(|error| error.to_string());
            let stored = DesktopSettings::read();
            post(move |ui| ui.finish_desktop(error, stored));
        });
    }

    fn finish_desktop(&mut self, error: Option<String>, stored: DesktopSettings) {
        self.busy = self.busy.saturating_sub(1);
        self.state.desktop = stored;
        if let Some(error) = error {
            self.shell
                .set_notice(Some(&format!("Could not save that setting. {error}")));
        }
        self.refresh_view(None);
    }
}

/// What the control that sent an action holds.
#[derive(Clone, Copy)]
struct Value {
    popup: Option<usize>,
    on: Option<bool>,
    slider: Option<f32>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `ActionTarget`
    // does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "OpenLogiSimpleActionTarget"]
    struct ActionTarget;

    impl ActionTarget {
        #[unsafe(method(controlChanged:))]
        fn control_changed(&self, sender: Option<&AnyObject>) {
            if let Some(control) = sender.and_then(|sender| sender.downcast_ref::<NSControl>()) {
                with_ui(|ui| ui.on_changed(control));
            }
        }

        #[unsafe(method(retry:))]
        fn retry(&self, _sender: Option<&AnyObject>) {
            with_ui(Ui::retry);
        }
    }
);

impl ActionTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: `init` initializes our freshly allocated NSObject subclass.
        unsafe { msg_send![super(this), init] }
    }
}
