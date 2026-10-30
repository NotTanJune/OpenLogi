//! The settings page as data: a pure view model over [`Config`], with no I/O.
//!
//! [`Page::build`] turns the saved configuration plus whatever the agent
//! reported about the device ([`Facts`]) into sections of [`Row`]s, each row
//! carrying the [`Control`] the window should draw. [`apply`] is the inverse:
//! it writes one [`Edit`] back through the same `openlogi-core` setters the
//! upstream GUI's `AppState::commit_*` methods use. Saving the file and telling
//! the agent are the callers' job ([`crate::store::Store`], [`crate::agent::Agent`]).
//!
//! # Final API
//!
//! The types are the ones the plan names, with these differences forced by the
//! real core API:
//!
//! - `Page::build(config, device_key, facts: Option<&Facts>)` takes [`Facts`],
//!   not a `DeviceInventory`. An inventory is one *receiver* with several
//!   paired devices, so it cannot say which device a key names, and it carries
//!   neither the DPI list nor the SmartShift state, which only `read_dpi` and
//!   `read_smartshift` return. [`crate::devices::Device::facts`] and
//!   [`crate::agent::Agent::facts`] fill it.
//! - [`Control::Slider`] also carries `step`: the sensor lists DPI values in
//!   steps, and the slider snaps to them.
//! - [`Control::Popup`] choices carry their [`Action`], so a selected index
//!   maps back to an [`Edit`] with [`Row::choice_edit`].
//! - A SmartShift edit is a whole [`SmartShiftStatus`]; build it with
//!   [`smartshift_edit`], which keeps the wheel's torque and threshold.
//!
//! Behaviour that is deliberate and differs from the upstream GUI:
//!
//! - Editing a button that is in gesture mode (a hand-edited or older config)
//!   sets that gesture's click action instead of replacing the whole direction
//!   map, so no saved gesture is lost.
//! - Editing a flick turns gesture mode back on for the dedicated gesture
//!   button when the config had turned it off. The upstream GUI has a mode
//!   toggle and ignores such an edit; this page has no toggle.
//! - While gesture mode is off the flick rows read "Do Nothing", the nearest
//!   honest value, because no flick is dispatched.

use std::fmt;

use openlogi_core::binding::{
    Action, Binding, ButtonId, GestureDirection, LongPressBinding, WheelHoldBinding,
};
use openlogi_core::bindings::{bindings_for, hidpp_gesture_maps_for};
use openlogi_core::config::{Config, SMARTSHIFT_AUTO_DISENGAGE_DEFAULT, SmartShift};
use openlogi_core::device::Capabilities;
use openlogi_core::hid::{Dpi, DpiInfo, SmartShiftAutoDisengage, SmartShiftMode, SmartShiftStatus};

/// What an unreadable device shows where a live value would be.
const NOT_CONNECTED: &str = "Not connected";

/// A mouse button the page binds an action to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    /// The thumb-side back button.
    Back,
    /// The thumb-side forward button.
    Forward,
    /// The wheel click.
    Middle,
    /// Rotating the thumb wheel up (forward).
    ThumbWheelUp,
    /// Rotating the thumb wheel down (backward).
    ThumbWheelDown,
}

impl Button {
    /// The buttons in page order.
    pub const ALL: [Self; 5] = [
        Self::Back,
        Self::Forward,
        Self::Middle,
        Self::ThumbWheelUp,
        Self::ThumbWheelDown,
    ];

    /// The core button this one binds.
    #[must_use]
    pub const fn id(self) -> ButtonId {
        match self {
            Self::Back => ButtonId::Back,
            Self::Forward => ButtonId::Forward,
            Self::Middle => ButtonId::MiddleClick,
            Self::ThumbWheelUp => ButtonId::ThumbwheelScrollUp,
            Self::ThumbWheelDown => ButtonId::ThumbwheelScrollDown,
        }
    }

    /// The row label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Back => "Back",
            Self::Forward => "Forward",
            Self::Middle => "Middle",
            Self::ThumbWheelUp => "Thumb wheel up",
            Self::ThumbWheelDown => "Thumb wheel down",
        }
    }

    const fn needs_thumb_wheel(self) -> bool {
        matches!(self, Self::ThumbWheelUp | Self::ThumbWheelDown)
    }
}

/// A flick of the dedicated gesture button. The custom build dispatches only
/// left and right for that button, the two slots
/// [`GestureDirection::LEFT_RIGHT`] keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flick {
    /// Hold and flick left.
    Left,
    /// Hold and flick right.
    Right,
}

impl Flick {
    /// The flicks in page order.
    pub const ALL: [Self; 2] = [Self::Left, Self::Right];

    /// The core direction this flick maps to.
    #[must_use]
    pub const fn direction(self) -> GestureDirection {
        match self {
            Self::Left => GestureDirection::Left,
            Self::Right => GestureDirection::Right,
        }
    }

    /// The row label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Left => "Flick left",
            Self::Right => "Flick right",
        }
    }
}

/// What the agent could say about one value on one device.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Reading<T> {
    /// Not read, or the read failed: the mouse is asleep, away, or the agent
    /// is down. Shown as "Not connected".
    #[default]
    Unavailable,
    /// The device answered that it has no such feature. The row is left out.
    Unsupported,
    /// The value the device reported.
    Ready(T),
}

/// Everything the page needs beyond the saved configuration.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Facts {
    /// The route the device was reached by, which selects the per-link
    /// overrides the upstream GUI reads settings through.
    pub route_key: Option<String>,
    /// What the device announced it can do, when known.
    pub capabilities: Option<Capabilities>,
    /// The sensor's DPI and supported values.
    pub dpi: Reading<DpiInfo>,
    /// The wheel's SmartShift state.
    pub smartshift: Reading<SmartShiftStatus>,
}

/// One page: sections of rows, top to bottom.
#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    /// The sections in display order; none is empty.
    pub sections: Vec<Section>,
}

/// A titled group of rows.
#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    /// The section heading.
    pub title: &'static str,
    /// The rows, top to bottom.
    pub rows: Vec<Row>,
}

/// One labelled control.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// What editing this row means.
    pub id: RowId,
    /// The text beside the control.
    pub label: String,
    /// The control to draw.
    pub control: Control,
}

/// The setting a row edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowId {
    /// A button's action.
    Button(Button),
    /// A gesture-button flick's action.
    Flick(Flick),
    /// The sensor DPI.
    Dpi,
    /// SmartShift on or off.
    SmartShift,
    /// Whether the wheel's scroll direction is inverted.
    InvertScroll,
}

/// A control the window should draw for a row.
#[derive(Clone, Debug, PartialEq)]
pub enum Control {
    /// A pop-up menu of choices.
    Popup {
        /// The menu items in order.
        choices: Vec<Choice>,
        /// The index of the current choice.
        selected: usize,
    },
    /// An on/off switch.
    Switch(bool),
    /// A slider. Values are `f32` because DPI never exceeds 65535, which `f32`
    /// holds exactly; AppKit widens it losslessly.
    Slider {
        /// The lowest value.
        min: f32,
        /// The highest value.
        max: f32,
        /// The smallest gap between supported values.
        step: f32,
        /// The current value.
        value: f32,
    },
    /// Read-only text, such as "Not connected".
    Text(String),
}

/// One pop-up item.
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    /// The menu text.
    pub label: String,
    /// The action it binds.
    pub action: Action,
}

/// One change the user made on the page.
#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    /// Bind `action` to `button`.
    Binding {
        /// The button.
        button: Button,
        /// The action.
        action: Action,
    },
    /// Bind `action` to a flick of the gesture button.
    Flick {
        /// The flick.
        flick: Flick,
        /// The action.
        action: Action,
    },
    /// Set the sensor DPI. The device must have taken it before it is saved.
    Dpi(Dpi),
    /// Set the wheel's whole SmartShift state; see [`smartshift_edit`].
    SmartShift(SmartShiftStatus),
    /// Invert, or restore, the wheel's scroll direction.
    InvertScroll(bool),
}

/// Why an edit could not be applied.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ModelError {
    /// The device key is empty, so there is no entry to write.
    #[error("no device is selected")]
    NoDevice,
    /// Zero is not a sensor resolution.
    #[error("0 is not a DPI value")]
    InvalidDpi,
}

impl Page {
    /// The page for `device_key` from the saved `config` and what the agent
    /// reported. With no `facts`, bindings still show from the file and every
    /// live value reads "Not connected".
    #[must_use]
    pub fn build(config: &Config, device_key: &str, facts: Option<&Facts>) -> Self {
        let default_facts = Facts::default();
        let facts = facts.unwrap_or(&default_facts);
        let caps = facts
            .capabilities
            .or_else(|| saved_capabilities(config, device_key));
        let mut sections = Vec::new();

        // A device that announced no reprogrammable buttons has neither the
        // button rows nor the gesture button.
        if caps.is_none_or(|caps| caps.buttons) {
            let actions = bindings_for(config, Some(device_key), None);
            let rows = Button::ALL
                .into_iter()
                .filter(|button| !button.needs_thumb_wheel() || caps.is_none_or(|c| c.thumbwheel))
                .map(|button| {
                    let action = actions
                        .get(&button.id())
                        .cloned()
                        .unwrap_or_else(|| openlogi_core::binding::default_binding(button.id()));
                    action_row(RowId::Button(button), button.label(), &action)
                })
                .collect();
            sections.push(Section {
                title: "Buttons",
                rows,
            });

            let flicks = hidpp_gesture_maps_for(config, Some(device_key), None)
                .remove(&ButtonId::GestureButton)
                .unwrap_or_default();
            let rows = Flick::ALL
                .into_iter()
                .map(|flick| {
                    let action = flicks
                        .get(&flick.direction())
                        .cloned()
                        .unwrap_or(Action::None);
                    action_row(RowId::Flick(flick), flick.label(), &action)
                })
                .collect();
            sections.push(Section {
                title: "Gesture button",
                rows,
            });
        }

        let rows: Vec<Row> = [
            dpi_row(&facts.dpi),
            smartshift_row(&facts.smartshift),
            Some(scroll_row(config, device_key, facts, caps)),
        ]
        .into_iter()
        .flatten()
        .collect();
        if !rows.is_empty() {
            sections.push(Section {
                title: "Pointer and scroll",
                rows,
            });
        }
        Self { sections }
    }

    /// The row editing `id`, if the page has one.
    #[must_use]
    pub fn row(&self, id: RowId) -> Option<&Row> {
        self.sections
            .iter()
            .flat_map(|section| &section.rows)
            .find(|row| row.id == id)
    }
}

impl Row {
    /// The edit that picking item `index` of a pop-up row makes, or `None` for
    /// a row that is not a pop-up or an index past its choices.
    #[must_use]
    pub fn choice_edit(&self, index: usize) -> Option<Edit> {
        let Control::Popup { choices, .. } = &self.control else {
            return None;
        };
        let action = choices.get(index)?.action.clone();
        match self.id {
            RowId::Button(button) => Some(Edit::Binding { button, action }),
            RowId::Flick(flick) => Some(Edit::Flick { flick, action }),
            RowId::Dpi | RowId::SmartShift | RowId::InvertScroll => None,
        }
    }
}

impl fmt::Display for Page {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for section in &self.sections {
            writeln!(formatter, "{}", section.title)?;
            for row in &section.rows {
                writeln!(formatter, "  {}: {}", row.label, row.control)?;
            }
        }
        Ok(())
    }
}

impl fmt::Display for Control {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Popup { choices, selected } => match choices.get(*selected) {
                Some(choice) => formatter.write_str(&choice.label),
                None => formatter.write_str("?"),
            },
            Self::Switch(on) => formatter.write_str(if *on { "on" } else { "off" }),
            Self::Slider {
                min, max, value, ..
            } => write!(formatter, "{value} ({min} to {max})"),
            Self::Text(text) => formatter.write_str(text),
        }
    }
}

/// A pop-up row of the actions a mouse button can take.
///
/// This is [`Action::catalog`] without [`Action::ShowActionsRing`]. Every other
/// catalog entry can fire from a mouse button or a wheel direction, and the
/// upstream picker offers them all. This app has no Actions Ring editor, so
/// choosing the ring would bind a button to a ring the user cannot configure
/// here. A button already bound to it keeps showing it, so the row never
/// misreports the saved value.
fn action_row(id: RowId, label: &str, current: &Action) -> Row {
    let mut actions: Vec<Action> = Action::catalog()
        .into_iter()
        .filter(|action| *action != Action::ShowActionsRing || current == action)
        .collect();
    // A saved action outside the picker (a recorded shortcut, an app launch)
    // is listed first, so the row shows it and an unrelated edit never
    // replaces it.
    let selected = actions
        .iter()
        .position(|action| action == current)
        .unwrap_or_else(|| {
            actions.insert(0, current.clone());
            0
        });
    Row {
        id,
        label: label.to_owned(),
        control: Control::Popup {
            choices: actions
                .into_iter()
                .map(|action| Choice {
                    label: action.label(),
                    action,
                })
                .collect(),
            selected,
        },
    }
}

fn dpi_row(dpi: &Reading<DpiInfo>) -> Option<Row> {
    let control = match dpi {
        Reading::Unsupported => return None,
        Reading::Unavailable => Control::Text(NOT_CONNECTED.to_owned()),
        Reading::Ready(info) => Control::Slider {
            min: f32::from(info.capabilities.min()),
            max: f32::from(info.capabilities.max()),
            step: f32::from(info.capabilities.step_hint()),
            value: f32::from(info.current),
        },
    };
    Some(Row {
        id: RowId::Dpi,
        label: "DPI".to_owned(),
        control,
    })
}

fn smartshift_row(smartshift: &Reading<SmartShiftStatus>) -> Option<Row> {
    let control = match smartshift {
        Reading::Unsupported => return None,
        Reading::Unavailable => Control::Text(NOT_CONNECTED.to_owned()),
        Reading::Ready(status) => Control::Switch(smartshift_enabled(*status)),
    };
    Some(Row {
        id: RowId::SmartShift,
        label: "SmartShift".to_owned(),
        control,
    })
}

/// Scroll inversion is offered once the device's capabilities are known, as the
/// upstream GUI does. macOS can always reverse the wheel in the capture layer
/// when the firmware has no invert bit (`openlogi-hook`'s
/// `SOFTWARE_SCROLL_INVERSION` is on everywhere but Windows), so on this
/// platform a probed device always qualifies.
fn scroll_row(config: &Config, device_key: &str, facts: &Facts, caps: Option<Capabilities>) -> Row {
    let control = if caps.is_some() {
        Control::Switch(invert_scroll(
            config,
            device_key,
            facts.route_key.as_deref(),
        ))
    } else {
        Control::Text(NOT_CONNECTED.to_owned())
    };
    Row {
        id: RowId::InvertScroll,
        label: "Invert scroll direction".to_owned(),
        control,
    }
}

/// The measured capabilities a link of the device last recorded in the file,
/// which keeps a sleeping mouse's rows in place.
fn saved_capabilities(config: &Config, device_key: &str) -> Option<Capabilities> {
    config
        .devices
        .get(device_key)?
        .links
        .values()
        .find_map(|link| link.capabilities)
}

/// Whether the wheel's scroll direction is inverted, read as the upstream GUI
/// reads it: the link's override when `route_key` has one, else the device's.
fn invert_scroll(config: &Config, device_key: &str, route_key: Option<&str>) -> bool {
    match (config.devices.get(device_key), route_key) {
        (Some(device), Some(route)) => device.effective_invert_scroll(route),
        _ => config.invert_scroll(device_key),
    }
}

/// Whether SmartShift is on: the wheel ratchets and releases into free-spin at
/// speed. Free-spin and a permanently engaged ratchet both read as off.
#[must_use]
pub fn smartshift_enabled(status: SmartShiftStatus) -> bool {
    status.mode == SmartShiftMode::Ratchet && !status.auto_disengage.is_permanent()
}

/// The edit that switches SmartShift on or off from the wheel's current state
/// (`facts`, else the saved state, else the firmware default), keeping its
/// torque and threshold.
///
/// On is ratchet with the speed threshold, restoring the default threshold if
/// the wheel was on a permanent ratchet. Off keeps the ratchet engaged
/// permanently, unless the wheel is already in free-spin.
#[must_use]
pub fn smartshift_edit(
    config: &Config,
    device_key: &str,
    facts: Option<&Facts>,
    enabled: bool,
) -> Edit {
    let base = facts
        .and_then(|facts| match facts.smartshift {
            Reading::Ready(status) => Some(status),
            Reading::Unavailable | Reading::Unsupported => None,
        })
        .or_else(|| {
            let route = facts.and_then(|facts| facts.route_key.as_deref());
            let device = config.devices.get(device_key)?;
            match route {
                Some(route) => device.effective_smartshift(route),
                None => device.smartshift,
            }
            .map(SmartShiftStatus::from)
        })
        .unwrap_or(SmartShiftStatus {
            mode: SmartShiftMode::Ratchet,
            auto_disengage: SmartShiftAutoDisengage::Threshold(SMARTSHIFT_AUTO_DISENGAGE_DEFAULT),
            tunable_torque: None,
        });
    let status = if enabled {
        SmartShiftStatus {
            mode: SmartShiftMode::Ratchet,
            auto_disengage: base.auto_disengage.threshold().map_or(
                SmartShiftAutoDisengage::Threshold(SMARTSHIFT_AUTO_DISENGAGE_DEFAULT),
                SmartShiftAutoDisengage::Threshold,
            ),
            ..base
        }
    } else if base.mode == SmartShiftMode::Free {
        base
    } else {
        SmartShiftStatus {
            auto_disengage: SmartShiftAutoDisengage::Permanent,
            ..base
        }
    };
    Edit::SmartShift(persistable(status))
}

/// `status` with an auto-disengage the config loader accepts.
///
/// Firmware can report a threshold below the configuration floor (an MX Master
/// 3 reports `4`, upstream #2079); saving it would write a `config.toml` the
/// next launch refuses to parse. Such a threshold becomes the default, as the
/// upstream GUI's `with_persistable_threshold` does, before the write reaches
/// the device or the file.
fn persistable(status: SmartShiftStatus) -> SmartShiftStatus {
    if SmartShift::accepts_auto_disengage(status.auto_disengage) {
        status
    } else {
        SmartShiftStatus {
            auto_disengage: SmartShiftAutoDisengage::Threshold(SMARTSHIFT_AUTO_DISENGAGE_DEFAULT),
            ..status
        }
    }
}

/// The edit for a DPI slider moved to `value`, snapped to a value the sensor
/// lists when `facts` has them.
#[must_use]
pub fn dpi_edit(value: f32, facts: Option<&Facts>) -> Edit {
    let dpi = Dpi::from_rounded(value);
    let snapped = facts.map_or(dpi, |facts| match &facts.dpi {
        Reading::Ready(info) => info.capabilities.nearest(dpi),
        Reading::Unavailable | Reading::Unsupported => dpi,
    });
    Edit::Dpi(snapped)
}

/// Write `edit` into `config` for `device_key` through the core setters.
///
/// This is the in-memory half of a commit. The upstream order after it is: save
/// the file, then `reload_saved_config`; a SmartShift edit is then sent to the
/// device, while a DPI edit is sent to the device first and saved only if the
/// device took it. [`crate::store::Store::commit`] and
/// [`crate::agent::Agent`] carry those steps.
///
/// # Errors
///
/// [`ModelError::NoDevice`] for an empty key, [`ModelError::InvalidDpi`] for 0.
pub fn apply(config: &mut Config, device_key: &str, edit: Edit) -> Result<(), ModelError> {
    if device_key.is_empty() {
        return Err(ModelError::NoDevice);
    }
    match edit {
        Edit::Binding { button, action } => bind(config, device_key, button.id(), action),
        Edit::Flick { flick, action } => {
            let gesture_button = ButtonId::GestureButton;
            if !config.is_gesture_mode(device_key, gesture_button) {
                config.set_gesture_mode(device_key, gesture_button, true);
            }
            config.set_gesture_direction(device_key, gesture_button, flick.direction(), action);
        }
        Edit::Dpi(dpi) => {
            if dpi == Dpi::new(0) {
                return Err(ModelError::InvalidDpi);
            }
            config.set_dpi(device_key, dpi);
        }
        Edit::SmartShift(status) => {
            config.set_smartshift(device_key, SmartShift::from(persistable(status)));
        }
        Edit::InvertScroll(invert) => config.set_invert_scroll(device_key, invert),
    }
    Ok(())
}

/// The upstream `commit_binding` write for the device's global profile: a
/// long-press button keeps its long action and a wheel-hold button keeps
/// holding the wheel, so the picked action replaces only the short or click
/// action. A gesture button gets its click direction set instead of losing its
/// map (see the module docs).
fn bind(config: &mut Config, device_key: &str, button: ButtonId, action: Action) {
    let binding = match config.stored_bindings(device_key).get(&button) {
        Some(Binding::LongPress(binding)) => {
            Binding::LongPress(LongPressBinding::new(action, binding.long().clone()))
        }
        Some(Binding::WheelHold(_)) => {
            Binding::WheelHold(WheelHoldBinding::horizontal_scroll(action))
        }
        Some(Binding::Gesture(_)) => {
            config.set_gesture_direction(device_key, button, GestureDirection::Click, action);
            return;
        }
        Some(Binding::Single(_)) | None => Binding::Single(action),
    };
    config.set_binding(device_key, button, binding);
}

#[cfg(test)]
mod tests {
    use openlogi_core::binding::default_binding;
    use openlogi_core::config::WheelMode;
    use openlogi_core::hid::{DpiCapabilities, SmartShiftThreshold, TunableTorque};

    use super::*;

    const KEY: &str = "unit:6be9d300";
    const ROUTE: &str = "bolt:abc:1";

    fn mouse_caps() -> Capabilities {
        Capabilities {
            buttons: true,
            pointer: true,
            thumbwheel: true,
            hires_wheel: true,
            ..Capabilities::default()
        }
    }

    fn dpi_info(current: u16) -> DpiInfo {
        DpiInfo {
            current: Dpi::new(current),
            capabilities: DpiCapabilities::new(vec![200, 400, 800, 1600, 3200, 8000])
                .expect("a non-empty list"),
        }
    }

    fn live_facts() -> Facts {
        Facts {
            route_key: Some(ROUTE.to_owned()),
            capabilities: Some(mouse_caps()),
            dpi: Reading::Ready(dpi_info(1600)),
            smartshift: Reading::Ready(SmartShiftStatus {
                mode: SmartShiftMode::Ratchet,
                auto_disengage: SmartShiftAutoDisengage::Threshold(
                    SmartShiftThreshold::try_new(20).expect("in range"),
                ),
                tunable_torque: Some(TunableTorque::DEFAULT),
            }),
        }
    }

    fn selected_action(page: &Page, id: RowId) -> Action {
        let row = page.row(id).expect("the row exists");
        let Control::Popup { choices, selected } = &row.control else {
            panic!("{id:?} is not a pop-up");
        };
        choices[*selected].action.clone()
    }

    fn config_with(bindings: &[(ButtonId, Action)]) -> Config {
        let mut config = Config::default();
        for (button, action) in bindings {
            config.set_binding(KEY, *button, Binding::Single(action.clone()));
        }
        config
    }

    #[test]
    fn rows_show_the_saved_bindings() {
        let config = config_with(&[
            (ButtonId::Back, Action::Copy),
            (ButtonId::Forward, Action::Paste),
        ]);
        let page = Page::build(&config, KEY, Some(&live_facts()));

        assert_eq!(
            selected_action(&page, RowId::Button(Button::Back)),
            Action::Copy
        );
        assert_eq!(
            selected_action(&page, RowId::Button(Button::Forward)),
            Action::Paste
        );
        // Untouched buttons show the built-in default.
        assert_eq!(
            selected_action(&page, RowId::Button(Button::Middle)),
            default_binding(ButtonId::MiddleClick)
        );
        assert_eq!(
            selected_action(&page, RowId::Button(Button::ThumbWheelUp)),
            default_binding(ButtonId::ThumbwheelScrollUp)
        );
    }

    #[test]
    fn sections_are_titled_in_page_order() {
        let page = Page::build(&Config::default(), KEY, Some(&live_facts()));
        let titles: Vec<_> = page.sections.iter().map(|section| section.title).collect();
        assert_eq!(titles, ["Buttons", "Gesture button", "Pointer and scroll"]);
        let ids: Vec<_> = page.sections[0].rows.iter().map(|row| row.id).collect();
        assert_eq!(ids, Button::ALL.map(RowId::Button));
    }

    #[test]
    fn the_picker_lists_the_catalog_without_the_actions_ring() {
        let page = Page::build(&Config::default(), KEY, Some(&live_facts()));
        let Control::Popup { choices, .. } =
            &page.row(RowId::Button(Button::Back)).expect("row").control
        else {
            panic!("a pop-up");
        };
        let offered: Vec<_> = choices.iter().map(|choice| choice.action.clone()).collect();
        let expected: Vec<_> = Action::catalog()
            .into_iter()
            .filter(|action| *action != Action::ShowActionsRing)
            .collect();
        assert_eq!(offered, expected);
        assert!(
            choices
                .iter()
                .all(|choice| choice.label == choice.action.label())
        );
    }

    #[test]
    fn a_saved_action_outside_the_picker_still_shows() {
        let ring = config_with(&[(ButtonId::Back, Action::ShowActionsRing)]);
        let page = Page::build(&ring, KEY, Some(&live_facts()));
        assert_eq!(
            selected_action(&page, RowId::Button(Button::Back)),
            Action::ShowActionsRing
        );

        let custom = Action::SetDpiPreset(2);
        let config = config_with(&[(ButtonId::Forward, custom.clone())]);
        let page = Page::build(&config, KEY, Some(&live_facts()));
        let row = page.row(RowId::Button(Button::Forward)).expect("row");
        let Control::Popup { choices, selected } = &row.control else {
            panic!("a pop-up");
        };
        assert_eq!(*selected, 0, "the foreign action leads the list");
        assert_eq!(choices[0].action, custom);
        assert_eq!(choices[0].label, "DPI Preset 3");
    }

    #[test]
    fn flicks_read_the_gesture_maps_left_and_right() {
        let mut config = Config::default();
        config.set_gesture_direction(
            KEY,
            ButtonId::GestureButton,
            GestureDirection::Left,
            Action::Undo,
        );
        config.set_gesture_direction(
            KEY,
            ButtonId::GestureButton,
            GestureDirection::Right,
            Action::Redo,
        );
        let page = Page::build(&config, KEY, Some(&live_facts()));
        assert_eq!(
            selected_action(&page, RowId::Flick(Flick::Left)),
            Action::Undo
        );
        assert_eq!(
            selected_action(&page, RowId::Flick(Flick::Right)),
            Action::Redo
        );
    }

    #[test]
    fn a_fresh_config_shows_the_default_flicks() {
        let page = Page::build(&Config::default(), KEY, Some(&live_facts()));
        assert_eq!(
            selected_action(&page, RowId::Flick(Flick::Left)),
            Action::PrevTab
        );
        assert_eq!(
            selected_action(&page, RowId::Flick(Flick::Right)),
            Action::NextTab
        );
    }

    #[test]
    fn flicks_read_do_nothing_while_gesture_mode_is_off() {
        let mut config = Config::default();
        config.set_gesture_mode(KEY, ButtonId::GestureButton, false);
        let page = Page::build(&config, KEY, Some(&live_facts()));
        assert_eq!(
            selected_action(&page, RowId::Flick(Flick::Left)),
            Action::None
        );
    }

    #[test]
    fn dpi_is_a_slider_over_the_sensors_own_range() {
        let page = Page::build(&Config::default(), KEY, Some(&live_facts()));
        let row = page.row(RowId::Dpi).expect("a DPI row");
        assert_eq!(row.label, "DPI");
        assert_eq!(
            row.control,
            Control::Slider {
                min: 200.0,
                max: 8000.0,
                step: 200.0,
                value: 1600.0
            }
        );
    }

    #[test]
    fn an_unread_device_says_not_connected_instead_of_a_stale_value() {
        let mut config = Config::default();
        config.set_dpi(KEY, Dpi::new(800));
        let page = Page::build(&config, KEY, None);
        for id in [RowId::Dpi, RowId::SmartShift, RowId::InvertScroll] {
            assert_eq!(
                page.row(id).expect("row").control,
                Control::Text("Not connected".to_owned()),
                "{id:?}"
            );
        }
        // Bindings stay editable from the saved file.
        assert!(page.row(RowId::Button(Button::Back)).is_some());
    }

    #[test]
    fn a_device_without_a_feature_loses_only_that_row() {
        let facts = Facts {
            dpi: Reading::Unsupported,
            smartshift: Reading::Unsupported,
            ..live_facts()
        };
        let page = Page::build(&Config::default(), KEY, Some(&facts));
        assert!(page.row(RowId::Dpi).is_none());
        assert!(page.row(RowId::SmartShift).is_none());
        assert!(page.row(RowId::InvertScroll).is_some());
    }

    #[test]
    fn capabilities_decide_which_button_rows_exist() {
        let no_wheel = Facts {
            capabilities: Some(Capabilities {
                thumbwheel: false,
                ..mouse_caps()
            }),
            ..live_facts()
        };
        let page = Page::build(&Config::default(), KEY, Some(&no_wheel));
        assert!(page.row(RowId::Button(Button::ThumbWheelUp)).is_none());
        assert!(page.row(RowId::Button(Button::ThumbWheelDown)).is_none());
        assert!(page.row(RowId::Button(Button::Back)).is_some());

        let no_buttons = Facts {
            capabilities: Some(Capabilities {
                buttons: false,
                ..mouse_caps()
            }),
            ..live_facts()
        };
        let page = Page::build(&Config::default(), KEY, Some(&no_buttons));
        assert!(page.row(RowId::Button(Button::Back)).is_none());
        assert!(page.row(RowId::Flick(Flick::Left)).is_none());
        assert!(page.row(RowId::Dpi).is_some());
    }

    #[test]
    fn a_sleeping_mouse_keeps_the_capabilities_its_link_recorded() {
        let mut config = Config::default();
        config
            .devices
            .entry(KEY.to_owned())
            .or_default()
            .links
            .insert(
                ROUTE.to_owned(),
                openlogi_core::config::LinkConfig {
                    capabilities: Some(Capabilities {
                        thumbwheel: false,
                        ..mouse_caps()
                    }),
                    ..Default::default()
                },
            );
        let page = Page::build(&config, KEY, None);
        assert!(page.row(RowId::Button(Button::ThumbWheelUp)).is_none());
        assert!(
            page.row(RowId::InvertScroll)
                .is_some_and(|row| matches!(row.control, Control::Switch(false)))
        );
    }

    #[test]
    fn scroll_inversion_reads_the_links_override_first() {
        let mut config = Config::default();
        config.set_invert_scroll(KEY, true);
        let facts = live_facts();
        let page = Page::build(&config, KEY, Some(&facts));
        assert_eq!(
            page.row(RowId::InvertScroll).expect("row").control,
            Control::Switch(true)
        );

        config
            .devices
            .entry(KEY.to_owned())
            .or_default()
            .links
            .insert(
                ROUTE.to_owned(),
                openlogi_core::config::LinkConfig {
                    overrides: openlogi_core::config::LinkOverrides {
                        invert_scroll: Some(false),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            );
        let page = Page::build(&config, KEY, Some(&facts));
        assert_eq!(
            page.row(RowId::InvertScroll).expect("row").control,
            Control::Switch(false)
        );
    }

    #[test]
    fn smartshift_is_on_only_for_a_ratchet_that_releases() {
        let status = |mode, auto_disengage| SmartShiftStatus {
            mode,
            auto_disengage,
            tunable_torque: None,
        };
        let threshold = SmartShiftAutoDisengage::Threshold(SMARTSHIFT_AUTO_DISENGAGE_DEFAULT);
        assert!(smartshift_enabled(status(
            SmartShiftMode::Ratchet,
            threshold
        )));
        assert!(!smartshift_enabled(status(
            SmartShiftMode::Ratchet,
            SmartShiftAutoDisengage::Permanent
        )));
        assert!(!smartshift_enabled(status(SmartShiftMode::Free, threshold)));
        let page = Page::build(&Config::default(), KEY, Some(&live_facts()));
        assert_eq!(
            page.row(RowId::SmartShift).expect("row").control,
            Control::Switch(true)
        );
    }

    #[test]
    fn page_text_names_each_row_and_its_value() {
        let config = config_with(&[(ButtonId::Back, Action::Copy)]);
        let text = Page::build(&config, KEY, Some(&live_facts())).to_string();
        assert!(text.contains("Buttons\n  Back: Copy\n"), "{text}");
        assert!(text.contains("  Flick left: Previous Tab\n"), "{text}");
        assert!(text.contains("  DPI: 1600 (200 to 8000)\n"), "{text}");
        assert!(text.contains("  SmartShift: on\n"), "{text}");
        assert!(text.contains("  Invert scroll direction: off\n"), "{text}");
    }

    #[test]
    fn a_binding_edit_round_trips_through_the_page() {
        let mut config = Config::default();
        let facts = live_facts();
        let page = Page::build(&config, KEY, Some(&facts));
        let row = page.row(RowId::Button(Button::Back)).expect("row");
        let Control::Popup { choices, .. } = &row.control else {
            panic!("a pop-up");
        };
        let index = choices
            .iter()
            .position(|c| c.action == Action::Copy)
            .expect("Copy is offered");
        let edit = row
            .choice_edit(index)
            .expect("a pop-up choice makes an edit");
        assert_eq!(
            edit,
            Edit::Binding {
                button: Button::Back,
                action: Action::Copy
            }
        );

        apply(&mut config, KEY, edit).expect("applies");
        let page = Page::build(&config, KEY, Some(&facts));
        assert_eq!(
            selected_action(&page, RowId::Button(Button::Back)),
            Action::Copy
        );
        assert_eq!(
            config.stored_bindings(KEY).get(&ButtonId::Back),
            Some(&Binding::Single(Action::Copy))
        );
    }

    #[test]
    fn each_button_edit_lands_on_its_own_core_button() {
        for button in Button::ALL {
            let mut config = Config::default();
            apply(
                &mut config,
                KEY,
                Edit::Binding {
                    button,
                    action: Action::Undo,
                },
            )
            .expect("applies");
            let stored = config.stored_bindings(KEY);
            assert_eq!(stored.len(), 1, "{button:?} wrote exactly one binding");
            assert_eq!(
                stored.get(&button.id()),
                Some(&Binding::Single(Action::Undo))
            );
        }
    }

    #[test]
    fn a_binding_edit_keeps_a_long_press_and_a_wheel_hold() {
        let mut config = Config::default();
        config.set_binding(
            KEY,
            ButtonId::Back,
            Binding::LongPress(LongPressBinding::new(Action::Copy, Action::Paste)),
        );
        config.set_binding(
            KEY,
            ButtonId::Forward,
            Binding::WheelHold(WheelHoldBinding::horizontal_scroll(Action::Copy)),
        );
        for button in [Button::Back, Button::Forward] {
            apply(
                &mut config,
                KEY,
                Edit::Binding {
                    button,
                    action: Action::Cut,
                },
            )
            .expect("applies");
        }
        let stored = config.stored_bindings(KEY);
        assert_eq!(
            stored.get(&ButtonId::Back),
            Some(&Binding::LongPress(LongPressBinding::new(
                Action::Cut,
                Action::Paste
            )))
        );
        assert_eq!(
            stored.get(&ButtonId::Forward),
            Some(&Binding::WheelHold(WheelHoldBinding::horizontal_scroll(
                Action::Cut
            )))
        );
    }

    #[test]
    fn a_binding_edit_on_a_gesture_button_sets_the_click_and_keeps_the_swipes() {
        let mut config = Config::default();
        config.set_gesture_mode(KEY, ButtonId::MiddleClick, true);
        config.set_gesture_direction(
            KEY,
            ButtonId::MiddleClick,
            GestureDirection::Left,
            Action::Undo,
        );
        apply(
            &mut config,
            KEY,
            Edit::Binding {
                button: Button::Middle,
                action: Action::Find,
            },
        )
        .expect("applies");
        let stored = config.stored_bindings(KEY);
        let Some(Binding::Gesture(map)) = stored.get(&ButtonId::MiddleClick) else {
            panic!("the button is still a gesture binding");
        };
        assert_eq!(map.get(&GestureDirection::Click), Some(&Action::Find));
        assert_eq!(map.get(&GestureDirection::Left), Some(&Action::Undo));
    }

    #[test]
    fn a_flick_edit_round_trips_and_leaves_the_other_flick() {
        let mut config = Config::default();
        apply(
            &mut config,
            KEY,
            Edit::Flick {
                flick: Flick::Left,
                action: Action::Undo,
            },
        )
        .expect("applies");
        let facts = live_facts();
        let page = Page::build(&config, KEY, Some(&facts));
        assert_eq!(
            selected_action(&page, RowId::Flick(Flick::Left)),
            Action::Undo
        );
        assert_eq!(
            selected_action(&page, RowId::Flick(Flick::Right)),
            Action::NextTab
        );
        let row = page.row(RowId::Flick(Flick::Right)).expect("row");
        let Control::Popup { choices, .. } = &row.control else {
            panic!("a pop-up");
        };
        let redo = choices
            .iter()
            .position(|c| c.action == Action::Redo)
            .expect("offered");
        assert_eq!(
            row.choice_edit(redo),
            Some(Edit::Flick {
                flick: Flick::Right,
                action: Action::Redo
            })
        );
    }

    #[test]
    fn a_flick_edit_turns_gesture_mode_back_on_and_restores_the_stashed_map() {
        let mut config = Config::default();
        config.set_gesture_direction(
            KEY,
            ButtonId::GestureButton,
            GestureDirection::Right,
            Action::Save,
        );
        config.set_gesture_mode(KEY, ButtonId::GestureButton, false);
        assert!(!config.is_gesture_mode(KEY, ButtonId::GestureButton));

        apply(
            &mut config,
            KEY,
            Edit::Flick {
                flick: Flick::Left,
                action: Action::Undo,
            },
        )
        .expect("applies");
        assert!(config.is_gesture_mode(KEY, ButtonId::GestureButton));
        let page = Page::build(&config, KEY, Some(&live_facts()));
        assert_eq!(
            selected_action(&page, RowId::Flick(Flick::Left)),
            Action::Undo
        );
        assert_eq!(
            selected_action(&page, RowId::Flick(Flick::Right)),
            Action::Save
        );
    }

    #[test]
    fn a_dpi_edit_is_saved_per_device() {
        let mut config = Config::default();
        apply(&mut config, KEY, Edit::Dpi(Dpi::new(800))).expect("applies");
        assert_eq!(config.dpi(KEY), Some(Dpi::new(800)));
        assert_eq!(config.dpi("another-device"), None);
    }

    #[test]
    fn the_slider_snaps_to_a_dpi_the_sensor_lists() {
        let facts = live_facts();
        assert_eq!(dpi_edit(1000.0, Some(&facts)), Edit::Dpi(Dpi::new(800)));
        assert_eq!(dpi_edit(2600.0, Some(&facts)), Edit::Dpi(Dpi::new(3200)));
        assert_eq!(dpi_edit(99999.0, Some(&facts)), Edit::Dpi(Dpi::new(8000)));
        assert_eq!(dpi_edit(1234.4, None), Edit::Dpi(Dpi::new(1234)));
    }

    #[test]
    fn a_zero_dpi_and_an_empty_key_are_refused() {
        let mut config = Config::default();
        assert_eq!(
            apply(&mut config, KEY, Edit::Dpi(Dpi::new(0))),
            Err(ModelError::InvalidDpi)
        );
        assert_eq!(
            apply(&mut config, "", Edit::InvertScroll(true)),
            Err(ModelError::NoDevice)
        );
        assert!(config.devices.is_empty(), "a refused edit writes nothing");
    }

    #[test]
    fn scroll_inversion_edits_round_trip() {
        let mut config = Config::default();
        apply(&mut config, KEY, Edit::InvertScroll(true)).expect("applies");
        assert!(config.invert_scroll(KEY));
        apply(&mut config, KEY, Edit::InvertScroll(false)).expect("applies");
        assert!(!config.invert_scroll(KEY));
    }

    fn smartshift_of(edit: &Edit) -> SmartShiftStatus {
        let Edit::SmartShift(status) = edit else {
            panic!("not a SmartShift edit: {edit:?}");
        };
        *status
    }

    #[test]
    fn smartshift_off_keeps_the_ratchet_engaged_and_the_torque() {
        let facts = live_facts();
        let edit = smartshift_edit(&Config::default(), KEY, Some(&facts), false);
        let status = smartshift_of(&edit);
        assert_eq!(status.mode, SmartShiftMode::Ratchet);
        assert_eq!(status.auto_disengage, SmartShiftAutoDisengage::Permanent);
        assert_eq!(status.tunable_torque, Some(TunableTorque::DEFAULT));
    }

    #[test]
    fn smartshift_on_restores_the_default_threshold_after_a_permanent_ratchet() {
        let facts = Facts {
            smartshift: Reading::Ready(SmartShiftStatus {
                mode: SmartShiftMode::Ratchet,
                auto_disengage: SmartShiftAutoDisengage::Permanent,
                tunable_torque: None,
            }),
            ..live_facts()
        };
        let status = smartshift_of(&smartshift_edit(
            &Config::default(),
            KEY,
            Some(&facts),
            true,
        ));
        assert_eq!(
            status.auto_disengage,
            SmartShiftAutoDisengage::Threshold(SMARTSHIFT_AUTO_DISENGAGE_DEFAULT)
        );
        assert!(smartshift_enabled(status));
    }

    #[test]
    fn smartshift_on_from_free_spin_ratchets_and_keeps_the_saved_threshold() {
        let threshold = SmartShiftThreshold::try_new(30).expect("in range");
        let facts = Facts {
            smartshift: Reading::Ready(SmartShiftStatus {
                mode: SmartShiftMode::Free,
                auto_disengage: SmartShiftAutoDisengage::Threshold(threshold),
                tunable_torque: None,
            }),
            ..live_facts()
        };
        let status = smartshift_of(&smartshift_edit(
            &Config::default(),
            KEY,
            Some(&facts),
            true,
        ));
        assert_eq!(status.mode, SmartShiftMode::Ratchet);
        assert_eq!(
            status.auto_disengage,
            SmartShiftAutoDisengage::Threshold(threshold)
        );

        // Off while already free-spinning leaves the wheel free-spinning.
        let off = smartshift_of(&smartshift_edit(
            &Config::default(),
            KEY,
            Some(&facts),
            false,
        ));
        assert_eq!(off.mode, SmartShiftMode::Free);
    }

    #[test]
    fn smartshift_edits_start_from_the_saved_state_when_the_wheel_is_unread() {
        let mut config = Config::default();
        config.set_smartshift(
            KEY,
            SmartShift {
                mode: WheelMode::Ratchet,
                auto_disengage: SmartShiftAutoDisengage::Threshold(
                    SmartShiftThreshold::try_new(25).expect("in range"),
                ),
                tunable_torque: Some(TunableTorque::MAX),
            },
        );
        let status = smartshift_of(&smartshift_edit(&config, KEY, None, false));
        assert_eq!(status.tunable_torque, Some(TunableTorque::MAX));
        assert_eq!(status.auto_disengage, SmartShiftAutoDisengage::Permanent);

        let fresh = smartshift_of(&smartshift_edit(&Config::default(), KEY, None, true));
        assert_eq!(fresh.mode, SmartShiftMode::Ratchet);
    }

    #[test]
    fn a_threshold_below_the_config_floor_is_replaced_before_it_is_saved() {
        let low = SmartShiftStatus {
            mode: SmartShiftMode::Ratchet,
            auto_disengage: SmartShiftAutoDisengage::Threshold(
                SmartShiftThreshold::try_new(4).expect("in the firmware range"),
            ),
            tunable_torque: None,
        };
        assert!(!SmartShift::accepts_auto_disengage(low.auto_disengage));

        let mut config = Config::default();
        apply(&mut config, KEY, Edit::SmartShift(low)).expect("applies");
        let saved = config.devices[KEY].smartshift.expect("saved");
        assert!(SmartShift::accepts_auto_disengage(saved.auto_disengage));
        assert_eq!(
            saved.auto_disengage,
            SmartShiftAutoDisengage::Threshold(SMARTSHIFT_AUTO_DISENGAGE_DEFAULT)
        );

        let facts = Facts {
            smartshift: Reading::Ready(low),
            ..live_facts()
        };
        let edit = smartshift_edit(&Config::default(), KEY, Some(&facts), true);
        assert!(SmartShift::accepts_auto_disengage(
            smartshift_of(&edit).auto_disengage
        ));
    }

    #[test]
    fn a_smartshift_edit_is_saved_per_device() {
        let mut config = Config::default();
        let edit = smartshift_edit(&config, KEY, Some(&live_facts()), false);
        let status = smartshift_of(&edit);
        apply(&mut config, KEY, edit).expect("applies");
        assert_eq!(
            config.devices[KEY].smartshift,
            Some(SmartShift::from(status))
        );
    }

    #[test]
    fn a_choice_index_past_the_menu_or_on_a_non_popup_row_makes_no_edit() {
        let page = Page::build(&Config::default(), KEY, Some(&live_facts()));
        let back = page.row(RowId::Button(Button::Back)).expect("row");
        assert_eq!(back.choice_edit(10_000), None);
        assert_eq!(page.row(RowId::Dpi).expect("row").choice_edit(0), None);
    }
}
