//! What the window does with a user's change, with no AppKit in it.
//!
//! - [`apply_edit`] is the one place that knows the write order for a page
//!   edit, so the window never re-derives it. It mirrors the upstream GUI.
//! - [`DesktopSettings`] and [`DesktopEdit`] carry the Space switching and
//!   Auto-raise rows, which live in the helpers' own stores, not in
//!   `config.toml`.
//! - [`agent_notice`] turns the agent link's state into the banner text.
//! - [`facts_after`] keeps the page's live readings in step with an edit the
//!   device took, so the next render shows it without another read.
//! - [`is_bundled`] says whether this process runs from an `.app` bundle.

use std::io;
use std::path::Path;

use openlogi_core::config::{ConfigError, ConfigRevision};
use openlogi_core::hid::{DeviceRoute, Dpi, SmartShiftStatus, WriteError};
use openlogi_ipc::SavedConfigVerdict;
use openlogi_ipc::client::ProtocolSkew;

use crate::agent::{Agent, AgentError, AgentState};
use crate::desktop::{self, AutoRaiseSettings, Delay, Speed};
use crate::devices::Device;
use crate::model::{Edit, Facts, ModelError, Reading};
use crate::store::{Store, StoreError};

/// The agent calls a write needs. [`Agent`] is the real one; the tests use a
/// recording stand-in to pin the order.
pub trait DeviceLink {
    /// Apply `dpi` to the device on `route` now.
    fn set_dpi(&self, route: &DeviceRoute, dpi: Dpi) -> Result<(), AgentError>;
    /// Apply the whole SmartShift `status` to the device on `route` now.
    fn set_smartshift(
        &self,
        route: &DeviceRoute,
        status: SmartShiftStatus,
    ) -> Result<(), AgentError>;
    /// Tell the agent the config was saved at `revision`.
    fn reload_saved_config(
        &self,
        revision: ConfigRevision,
    ) -> Result<SavedConfigVerdict, AgentError>;
}

impl DeviceLink for Agent {
    fn set_dpi(&self, route: &DeviceRoute, dpi: Dpi) -> Result<(), AgentError> {
        Self::set_dpi(self, route, dpi)
    }

    fn set_smartshift(
        &self,
        route: &DeviceRoute,
        status: SmartShiftStatus,
    ) -> Result<(), AgentError> {
        Self::set_smartshift(self, route, status)
    }

    fn reload_saved_config(
        &self,
        revision: ConfigRevision,
    ) -> Result<SavedConfigVerdict, AgentError> {
        Self::reload_saved_config(self, revision)
    }
}

/// How an edit that was saved ended.
#[derive(Debug)]
pub enum Outcome {
    /// Saved, and the agent and device have it.
    Applied,
    /// Saved, but another writer changed the file afterwards and the agent
    /// applied that instead. The store now holds the file as it is, so the
    /// page must be redrawn from it.
    AdoptedNewerFile,
    /// Saved to the file, but not applied live. The agent applies the file on
    /// its next reload or when the device reconnects.
    SavedOnly(SavedOnly),
}

/// Why a saved edit was not applied live.
#[derive(Debug)]
pub enum SavedOnly {
    /// The device is asleep or away, so there is nothing to write to now.
    DeviceOffline,
    /// The agent could not be reached or refused.
    Agent(AgentError),
}

impl SavedOnly {
    /// The short line the window shows.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::DeviceOffline => "Saved. The mouse will use it when it is connected.".to_owned(),
            Self::Agent(error) => format!(
                "Saved, but the background service did not apply it. {}",
                agent_reason(error)
            ),
        }
    }
}

/// Why an edit was not saved. Nothing changed on disk or on the device.
#[derive(Debug, thiserror::Error)]
pub enum EditError {
    /// A device write needs a mouse that is connected now.
    #[error("the mouse is not connected")]
    NotConnected,
    /// The file changed elsewhere after this app read it. The store has
    /// re-read it; the user can make the change again.
    #[error("the settings file changed elsewhere")]
    Conflict,
    /// Saving failed or the edit was refused.
    #[error(transparent)]
    Store(StoreError),
    /// The device or the agent refused before anything was saved.
    #[error(transparent)]
    Agent(AgentError),
}

impl EditError {
    /// The short line the window shows, saying what is true now.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::NotConnected => "The mouse is not connected.".to_owned(),
            Self::Conflict => {
                "The settings file changed elsewhere. It was reloaded, so try again.".to_owned()
            }
            Self::Store(error) => error.to_string(),
            Self::Agent(AgentError::Device(WriteError::DpiNotApplied { actual, .. })) => {
                format!("The mouse kept {actual} DPI.")
            }
            Self::Agent(error) => agent_reason(error),
        }
    }
}

fn agent_reason(error: &AgentError) -> String {
    match error {
        AgentError::Unavailable(_) | AgentError::Lost(_) => {
            "OpenLogi's background service is not reachable.".to_owned()
        }
        AgentError::Device(_) => "The mouse did not take it.".to_owned(),
        AgentError::ConfigRejected(_) | AgentError::Inventory(_) => error.to_string(),
    }
}

/// Apply one page edit for `device`: save it, tell the agent, and write the
/// device, in the order the upstream GUI uses.
///
/// - Bindings, flicks and scroll direction: save, then `reload_saved_config`.
/// - DPI: write the device first and save only if it took the value, because
///   the agent writes the saved DPI again on every reconnect, so a value the
///   device refused must never reach the file.
/// - SmartShift: save and reload first, then write the device. The saved state
///   is what the agent applies on a reconnect, so it is right to keep even when
///   the live write fails ([`Outcome::SavedOnly`]).
///
/// # Errors
///
/// [`EditError`]: nothing was saved and the device was not changed (a DPI the
/// device refused included).
pub fn apply_edit(
    link: &impl DeviceLink,
    store: &mut Store,
    device: &Device,
    edit: Edit,
) -> Result<Outcome, EditError> {
    match edit {
        Edit::Dpi(dpi) => {
            if dpi == Dpi::new(0) {
                return Err(EditError::Store(StoreError::Edit(ModelError::InvalidDpi)));
            }
            let route = live_route(device).ok_or(EditError::NotConnected)?;
            link.set_dpi(route, dpi).map_err(EditError::Agent)?;
            commit(store, device, Edit::Dpi(dpi))?;
            Ok(publish(link, store))
        }
        Edit::SmartShift(status) => {
            commit(store, device, Edit::SmartShift(status))?;
            let published = publish(link, store);
            if !matches!(published, Outcome::Applied) {
                return Ok(published);
            }
            // `persistable` may have changed the threshold in the file; the
            // device gets what was saved.
            let saved = store
                .config()
                .devices
                .get(&device.key)
                .and_then(|entry| entry.smartshift)
                .map_or(status, SmartShiftStatus::from);
            let Some(route) = live_route(device) else {
                return Ok(Outcome::SavedOnly(SavedOnly::DeviceOffline));
            };
            Ok(match link.set_smartshift(route, saved) {
                Ok(()) => Outcome::Applied,
                Err(error) => Outcome::SavedOnly(SavedOnly::Agent(error)),
            })
        }
        edit @ (Edit::Binding { .. } | Edit::Flick { .. } | Edit::InvertScroll(_)) => {
            commit(store, device, edit)?;
            Ok(publish(link, store))
        }
    }
}

/// The route a live write goes to, when the device is online and has one.
fn live_route(device: &Device) -> Option<&DeviceRoute> {
    device.route.as_ref().filter(|_| device.online)
}

/// `Store::commit`, with a lost race against another writer turned into a
/// re-read store and [`EditError::Conflict`].
fn commit(store: &mut Store, device: &Device, edit: Edit) -> Result<(), EditError> {
    match store.commit(device, edit) {
        Ok(()) => Ok(()),
        Err(StoreError::File(ConfigError::Conflict { .. })) => {
            store.refresh().map_err(EditError::Store)?;
            Err(EditError::Conflict)
        }
        Err(error) => Err(EditError::Store(error)),
    }
}

/// Tell the agent about the save just made. The save stands whatever the
/// answer; the answer says whether the agent applied it.
fn publish(link: &impl DeviceLink, store: &mut Store) -> Outcome {
    match link.reload_saved_config(store.revision()) {
        Ok(SavedConfigVerdict::Accepted) => Outcome::Applied,
        Ok(SavedConfigVerdict::FileChangedAfterSave) => match store.refresh() {
            Ok(()) => Outcome::AdoptedNewerFile,
            Err(error) => Outcome::SavedOnly(SavedOnly::Agent(AgentError::ConfigRejected(
                openlogi_ipc::ConfigReloadError {
                    message: error.to_string(),
                },
            ))),
        },
        Err(error) => Outcome::SavedOnly(SavedOnly::Agent(error)),
    }
}

/// `facts` with the live readings an edit just set. A DPI or SmartShift edit
/// the device took changes what a read would now return; everything else is
/// untouched.
#[must_use]
pub fn facts_after(facts: &Facts, edit: &Edit) -> Facts {
    let mut next = facts.clone();
    match (edit, &mut next.dpi, &mut next.smartshift) {
        (Edit::Dpi(dpi), Reading::Ready(info), _) => info.current = *dpi,
        (Edit::SmartShift(status), _, smartshift @ Reading::Ready(_)) => {
            *smartshift = Reading::Ready(*status);
        }
        _ => {}
    }
    next
}

/// The Desktop section's stored values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DesktopSettings {
    /// How fast a Space switch animates.
    pub speed: Speed,
    /// Whether Ctrl+Option+Arrow switches Spaces.
    pub hotkeys: bool,
    /// Raising the window under the pointer.
    pub autoraise: AutoRaiseSettings,
}

/// A row of the Desktop section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesktopRow {
    /// Space switching speed.
    Speed,
    /// Ctrl+Option+Arrow hotkeys.
    Hotkeys,
    /// Auto-raise on or off.
    AutoRaise,
    /// Auto-raise delay.
    Delay,
}

/// One change to the Desktop section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesktopEdit {
    /// Pick a Space switching speed.
    Speed(Speed),
    /// Turn the hotkeys on or off.
    Hotkeys(bool),
    /// Turn Auto-raise on or off.
    AutoRaise(bool),
    /// Pick the Auto-raise delay.
    Delay(Delay),
}

impl DesktopSettings {
    /// What the helpers have stored now. Each field is a `defaults` or file
    /// read, so call it from a worker thread.
    #[must_use]
    pub fn read() -> Self {
        Self {
            speed: desktop::speed(),
            hotkeys: desktop::hotkeys(),
            autoraise: desktop::autoraise_settings(),
        }
    }

    /// These settings with `edit` applied. Pure.
    #[must_use]
    pub fn with(self, edit: DesktopEdit) -> Self {
        let mut next = self;
        match edit {
            DesktopEdit::Speed(speed) => next.speed = speed,
            DesktopEdit::Hotkeys(on) => next.hotkeys = on,
            DesktopEdit::AutoRaise(on) => next.autoraise.enabled = on,
            DesktopEdit::Delay(delay) => next.autoraise.delay = delay,
        }
        next
    }

    /// Store `edit` and restart the helper it concerns. Auto-raise edits start
    /// from the stored file, not from a copy the window holds, so a change made
    /// from the menu bar in the meantime is kept.
    ///
    /// # Errors
    ///
    /// The helper's store could not be written.
    pub fn write(edit: DesktopEdit) -> io::Result<()> {
        match edit {
            DesktopEdit::Speed(speed) => desktop::set_speed(speed),
            DesktopEdit::Hotkeys(on) => desktop::set_hotkeys(on),
            DesktopEdit::AutoRaise(_) | DesktopEdit::Delay(_) => {
                let current = Self {
                    speed: Speed::Instant,
                    hotkeys: false,
                    autoraise: desktop::autoraise_settings(),
                };
                desktop::set_autoraise(current.with(edit).autoraise)
            }
        }
    }
}

/// What the banner says about the agent link, and what it allows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentNotice {
    /// The banner text.
    pub message: String,
    /// Whether a Retry button is offered.
    pub retry: bool,
    /// Whether controls stay usable.
    pub writable: bool,
}

/// The banner for `state`, or `None` when connected.
#[must_use]
pub fn agent_notice(state: AgentState) -> Option<AgentNotice> {
    match state {
        AgentState::Connected => None,
        AgentState::NotRunning => Some(AgentNotice {
            message: "OpenLogi's background service is not running.".to_owned(),
            retry: true,
            writable: true,
        }),
        AgentState::VersionSkew(ProtocolSkew::AgentOlder { .. }) => Some(AgentNotice {
            message: "OpenLogi's background service is older than this window and is waiting \
                      to be replaced. Settings cannot be changed until it is."
                .to_owned(),
            retry: true,
            writable: false,
        }),
        AgentState::VersionSkew(ProtocolSkew::AgentNewer { .. }) => Some(AgentNotice {
            message: "OpenLogi's background service is newer than this window. Quit and reopen \
                      OpenLogi to update it."
                .to_owned(),
            retry: false,
            writable: false,
        }),
    }
}

/// Whether `executable` sits in an app bundle: `Name.app/Contents/MacOS/name`.
///
/// Registering the agent's login item resolves a plist against the calling
/// bundle, so a bare binary (a `cargo run`, the smoke check) must never do it.
#[must_use]
pub fn is_bundled(executable: &Path) -> bool {
    executable
        .ancestors()
        .nth(3)
        .and_then(Path::extension)
        .is_some_and(|extension| extension == "app")
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::PathBuf;

    use openlogi_core::binding::Action;
    use openlogi_core::hid::{
        DpiCapabilities, DpiInfo, SmartShiftAutoDisengage, SmartShiftMode, SmartShiftThreshold,
    };

    use crate::model::Button;

    use super::*;

    /// What a [`Recorder`] does when it is asked to write.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Answer {
        Accept,
        Refuse,
        FileChanged,
    }

    /// Records every call, and what the file held when the call came.
    struct Recorder {
        calls: RefCell<Vec<String>>,
        path: PathBuf,
        answer: Answer,
    }

    impl Recorder {
        fn new(path: PathBuf, answer: Answer) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                path,
                answer,
            }
        }

        fn note(&self, call: &str) {
            self.calls
                .borrow_mut()
                .push(format!("{call}, file {}", self.saved()));
        }

        fn saved(&self) -> &'static str {
            if self.path.exists() { "saved" } else { "empty" }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }

        fn device_answer(&self) -> Result<(), AgentError> {
            match self.answer {
                Answer::Refuse => Err(AgentError::Device(WriteError::DpiNotApplied {
                    requested: Dpi::new(3200),
                    actual: Dpi::new(1600),
                })),
                Answer::Accept | Answer::FileChanged => Ok(()),
            }
        }
    }

    impl DeviceLink for Recorder {
        fn set_dpi(&self, _route: &DeviceRoute, _dpi: Dpi) -> Result<(), AgentError> {
            self.note("set_dpi");
            self.device_answer()
        }

        fn set_smartshift(
            &self,
            _route: &DeviceRoute,
            _status: SmartShiftStatus,
        ) -> Result<(), AgentError> {
            self.note("set_smartshift");
            self.device_answer()
        }

        fn reload_saved_config(
            &self,
            _revision: ConfigRevision,
        ) -> Result<SavedConfigVerdict, AgentError> {
            self.note("reload");
            Ok(match self.answer {
                Answer::FileChanged => SavedConfigVerdict::FileChangedAfterSave,
                Answer::Accept | Answer::Refuse => SavedConfigVerdict::Accepted,
            })
        }
    }

    fn device(online: bool) -> Device {
        Device {
            key: "unit:aa".to_owned(),
            persistent: true,
            name: "Test mouse".to_owned(),
            route: Some(DeviceRoute::Bolt {
                receiver_uid: "R".to_owned(),
                slot: 1,
            }),
            route_key: "bolt:R:1".to_owned(),
            online,
            battery: None,
            capabilities: None,
        }
    }

    fn setup(answer: Answer) -> (tempfile::TempDir, Store, Recorder) {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("config.toml");
        let store = Store::load_from(&path).expect("a missing file loads as defaults");
        (dir, store, Recorder::new(path, answer))
    }

    fn smartshift_off() -> Edit {
        Edit::SmartShift(SmartShiftStatus {
            mode: SmartShiftMode::Ratchet,
            auto_disengage: SmartShiftAutoDisengage::Permanent,
            tunable_torque: None,
        })
    }

    #[test]
    fn dpi_goes_to_the_device_before_the_file_and_then_the_agent_reloads() {
        let (_dir, mut store, link) = setup(Answer::Accept);
        let outcome = apply_edit(&link, &mut store, &device(true), Edit::Dpi(Dpi::new(3200)))
            .expect("applies");
        assert!(matches!(outcome, Outcome::Applied), "{outcome:?}");
        assert_eq!(
            link.calls(),
            ["set_dpi, file empty", "reload, file saved"],
            "the device is written while the file does not exist yet"
        );
        assert_eq!(store.config().dpi("unit:aa"), Some(Dpi::new(3200)));
    }

    #[test]
    fn a_dpi_the_device_refused_is_not_saved_and_not_reloaded() {
        let (_dir, mut store, link) = setup(Answer::Refuse);
        let result = apply_edit(&link, &mut store, &device(true), Edit::Dpi(Dpi::new(3200)));
        let error = result.expect_err("the device refused");
        assert_eq!(error.message(), "The mouse kept 1600 DPI.");
        assert_eq!(link.calls(), ["set_dpi, file empty"]);
        assert_eq!(store.config().dpi("unit:aa"), None);
        assert!(!link.path.exists(), "nothing reached the disk");
    }

    #[test]
    fn dpi_for_an_offline_device_or_zero_touches_nothing() {
        let (_dir, mut store, link) = setup(Answer::Accept);
        let offline = apply_edit(&link, &mut store, &device(false), Edit::Dpi(Dpi::new(800)));
        assert!(
            matches!(offline, Err(EditError::NotConnected)),
            "{offline:?}"
        );
        let zero = apply_edit(&link, &mut store, &device(true), Edit::Dpi(Dpi::new(0)));
        assert!(
            matches!(
                zero,
                Err(EditError::Store(StoreError::Edit(ModelError::InvalidDpi)))
            ),
            "{zero:?}"
        );
        assert!(link.calls().is_empty(), "no agent call was made");
        assert!(!link.path.exists());
    }

    #[test]
    fn smartshift_is_saved_and_reloaded_before_the_device_is_written() {
        let (_dir, mut store, link) = setup(Answer::Accept);
        let outcome =
            apply_edit(&link, &mut store, &device(true), smartshift_off()).expect("applies");
        assert!(matches!(outcome, Outcome::Applied), "{outcome:?}");
        assert_eq!(
            link.calls(),
            ["reload, file saved", "set_smartshift, file saved"]
        );
    }

    #[test]
    fn smartshift_that_the_device_refuses_stays_saved_and_says_so() {
        let (_dir, mut store, link) = setup(Answer::Refuse);
        let outcome = apply_edit(&link, &mut store, &device(true), smartshift_off())
            .expect("the save stands");
        let Outcome::SavedOnly(saved) = outcome else {
            panic!("expected SavedOnly, got {outcome:?}");
        };
        assert!(
            saved.message().starts_with("Saved, but"),
            "{}",
            saved.message()
        );
        assert!(
            store
                .config()
                .devices
                .get("unit:aa")
                .and_then(|d| d.smartshift)
                .is_some(),
            "the file keeps it so a reconnect applies it"
        );
    }

    #[test]
    fn smartshift_for_an_offline_device_is_saved_without_a_device_call() {
        let (_dir, mut store, link) = setup(Answer::Accept);
        let outcome =
            apply_edit(&link, &mut store, &device(false), smartshift_off()).expect("saves");
        assert!(
            matches!(outcome, Outcome::SavedOnly(SavedOnly::DeviceOffline)),
            "{outcome:?}"
        );
        assert_eq!(link.calls(), ["reload, file saved"]);
    }

    #[test]
    fn bindings_save_then_reload_and_never_touch_the_device() {
        let (_dir, mut store, link) = setup(Answer::Accept);
        let edit = Edit::Binding {
            button: Button::Back,
            action: Action::Copy,
        };
        let outcome = apply_edit(&link, &mut store, &device(true), edit).expect("applies");
        assert!(matches!(outcome, Outcome::Applied), "{outcome:?}");
        assert_eq!(link.calls(), ["reload, file saved"]);

        let outcome = apply_edit(&link, &mut store, &device(true), Edit::InvertScroll(true))
            .expect("applies");
        assert!(matches!(outcome, Outcome::Applied), "{outcome:?}");
        assert_eq!(link.calls().len(), 2);
    }

    #[test]
    fn a_file_changed_after_the_save_is_adopted_and_reported() {
        let (_dir, mut store, link) = setup(Answer::FileChanged);
        let outcome =
            apply_edit(&link, &mut store, &device(true), Edit::InvertScroll(true)).expect("saved");
        assert!(matches!(outcome, Outcome::AdoptedNewerFile), "{outcome:?}");
    }

    #[test]
    fn an_edit_that_loses_a_race_re_reads_the_file_and_changes_nothing() {
        let (_dir, mut store, link) = setup(Answer::Accept);
        let edit = |action| Edit::Binding {
            button: Button::Back,
            action,
        };
        apply_edit(&link, &mut store, &device(true), edit(Action::Copy)).expect("saves");

        // Another writer (the CLI, a hand edit) changes the file.
        let mut other = Store::load_from(&link.path).expect("loads");
        other
            .commit(&device(true), edit(Action::Paste))
            .expect("saves");

        let calls = link.calls().len();
        let result = apply_edit(&link, &mut store, &device(true), edit(Action::Cut));
        assert!(matches!(result, Err(EditError::Conflict)), "{result:?}");
        assert_eq!(link.calls().len(), calls, "no reload for an unsaved edit");
        let back = store
            .config()
            .stored_bindings("unit:aa")
            .get(&openlogi_core::binding::ButtonId::Back)
            .cloned();
        assert_eq!(
            back,
            Some(openlogi_core::binding::Binding::Single(Action::Paste)),
            "the store adopted the other writer's file"
        );
    }

    #[test]
    fn facts_follow_a_dpi_or_smartshift_edit_and_ignore_the_rest() {
        let facts = Facts {
            dpi: Reading::Ready(DpiInfo {
                current: Dpi::new(800),
                capabilities: DpiCapabilities::new(vec![800, 1600]).expect("a list"),
            }),
            smartshift: Reading::Ready(SmartShiftStatus {
                mode: SmartShiftMode::Ratchet,
                auto_disengage: SmartShiftAutoDisengage::Threshold(
                    SmartShiftThreshold::try_new(20).expect("in range"),
                ),
                tunable_torque: None,
            }),
            ..Facts::default()
        };
        let next = facts_after(&facts, &Edit::Dpi(Dpi::new(1600)));
        assert!(matches!(&next.dpi, Reading::Ready(info) if info.current == Dpi::new(1600)));
        assert_eq!(next.smartshift, facts.smartshift);

        let Edit::SmartShift(status) = smartshift_off() else {
            unreachable!("smartshift_off builds a SmartShift edit");
        };
        let next = facts_after(&facts, &smartshift_off());
        assert_eq!(next.smartshift, Reading::Ready(status));

        assert_eq!(facts_after(&facts, &Edit::InvertScroll(true)), facts);
        // A reading the device never gave is not invented.
        let blind = Facts::default();
        assert_eq!(facts_after(&blind, &Edit::Dpi(Dpi::new(1600))), blind);
    }

    #[test]
    fn desktop_edits_change_only_their_field() {
        let base = DesktopSettings {
            speed: Speed::Instant,
            hotkeys: false,
            autoraise: AutoRaiseSettings {
                enabled: true,
                delay: Delay::Short,
            },
        };
        assert_eq!(
            base.with(DesktopEdit::Speed(Speed::Smooth)).speed,
            Speed::Smooth
        );
        assert!(base.with(DesktopEdit::Hotkeys(true)).hotkeys);
        let off = base.with(DesktopEdit::AutoRaise(false));
        assert!(!off.autoraise.enabled);
        assert_eq!(
            off.autoraise.delay,
            Delay::Short,
            "the delay is kept while off"
        );
        assert_eq!(
            base.with(DesktopEdit::Delay(Delay::Long)).autoraise.delay,
            Delay::Long
        );
        assert_eq!(base.with(DesktopEdit::Delay(Delay::Long)).speed, base.speed);
    }

    #[test]
    fn the_banner_follows_the_agent_state() {
        assert_eq!(agent_notice(AgentState::Connected), None);

        let down = agent_notice(AgentState::NotRunning).expect("a banner");
        assert!(down.retry && down.writable);

        let older = agent_notice(AgentState::VersionSkew(ProtocolSkew::AgentOlder {
            agent: 1,
        }))
        .expect("a banner");
        assert!(
            older.retry && !older.writable,
            "an older agent may be replaced"
        );

        let newer = agent_notice(AgentState::VersionSkew(ProtocolSkew::AgentNewer {
            agent: 99,
        }))
        .expect("a banner");
        assert!(!newer.retry && !newer.writable, "only a relaunch helps");
    }

    #[test]
    fn only_a_path_inside_an_app_bundle_counts_as_bundled() {
        assert!(is_bundled(Path::new(
            "/Applications/OpenLogi.app/Contents/MacOS/openlogi-desktop"
        )));
        assert!(!is_bundled(Path::new(
            "/Users/me/OpenLogi/apps/openlogi-simple/target/release/openlogi-simple"
        )));
        assert!(!is_bundled(Path::new(
            "/Applications/OpenLogi.app/openlogi"
        )));
        assert!(!is_bundled(Path::new("openlogi-simple")));
    }
}
