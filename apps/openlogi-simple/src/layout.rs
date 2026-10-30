//! The window's content as plain data, with no AppKit in it.
//!
//! [`Layout::build`] turns the state the window holds (agent link, devices,
//! the selected device's facts, the saved config, the desktop settings) into a
//! header and sections of [`RowSpec`]s. The window draws that and, when only
//! values changed ([`Layout::same_shape`]), updates the controls in place
//! instead of rebuilding them under the user's pointer.

use openlogi_core::config::Config;
use openlogi_core::hid::Dpi;

use crate::agent::AgentState;
use crate::controller::{DesktopRow, DesktopSettings, agent_notice};
use crate::desktop::{Delay, Speed};
use crate::devices::Device;
use crate::model::{Control, Facts, Page, RowId};

/// What a control edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// A row of the device page.
    Row(RowId),
    /// A row of the Desktop section.
    Desktop(DesktopRow),
}

/// The control to draw for a row.
#[derive(Clone, Debug, PartialEq)]
pub enum Widget {
    /// A pop-up menu.
    Popup {
        /// The item titles in order.
        items: Vec<String>,
        /// The index of the current item.
        selected: usize,
    },
    /// An on/off switch.
    Switch(bool),
    /// A slider with a value label.
    Slider {
        /// The lowest value.
        min: f32,
        /// The highest value.
        max: f32,
        /// The smallest gap between supported values.
        step: f32,
        /// The current value.
        value: f32,
        /// The unit written after the value.
        unit: &'static str,
    },
    /// Read-only text.
    Text(String),
}

/// One labelled control.
#[derive(Clone, Debug, PartialEq)]
pub struct RowSpec {
    /// What editing it means.
    pub target: Target,
    /// The text beside the control.
    pub label: String,
    /// The control.
    pub widget: Widget,
    /// Whether the user can change it.
    pub enabled: bool,
}

/// A titled group of rows.
#[derive(Clone, Debug, PartialEq)]
pub struct SectionSpec {
    /// The heading.
    pub title: &'static str,
    /// The rows, top to bottom.
    pub rows: Vec<RowSpec>,
}

/// The device line at the top.
#[derive(Clone, Debug, PartialEq)]
pub struct Header {
    /// The names of the mice, in the order the picker lists them. One name is
    /// shown as a plain label, more as a picker.
    pub names: Vec<String>,
    /// The index of the selected mouse.
    pub selected: usize,
    /// The battery, or "Not connected" for a mouse that is away.
    pub status: String,
}

/// Everything the window shows below the banners.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    /// The device line, when a mouse is selected.
    pub header: Option<Header>,
    /// A line in place of the device sections when there is no mouse to show.
    pub note: Option<String>,
    /// The sections, device ones first and Desktop last.
    pub sections: Vec<SectionSpec>,
}

/// The state a layout is built from.
pub struct View<'a> {
    /// The agent link.
    pub agent: AgentState,
    /// The mice the agent reported.
    pub devices: &'a [Device],
    /// The index of the selected mouse in `devices`.
    pub selected: usize,
    /// What is known about the selected mouse.
    pub facts: &'a Facts,
    /// The saved configuration.
    pub config: &'a Config,
    /// The Desktop section's stored values.
    pub desktop: DesktopSettings,
    /// Whether the first load is still running.
    pub loading: bool,
}

impl Layout {
    /// The layout for `view`.
    ///
    /// Without a connection the device sections are left out rather than shown
    /// stale; the Desktop section does not depend on the agent, so it stays,
    /// and is disabled only when the agent speaks another protocol.
    #[must_use]
    pub fn build(view: &View<'_>) -> Self {
        let writable = agent_notice(view.agent).is_none_or(|notice| notice.writable);
        let mut layout = Self {
            header: None,
            note: None,
            sections: Vec::new(),
        };
        if view.agent == AgentState::Connected {
            match view.devices.get(view.selected) {
                Some(device) => {
                    layout.header = Some(header(view.devices, view.selected));
                    let page = Page::build(view.config, &device.key, Some(view.facts));
                    layout.sections.extend(page.sections.iter().map(|section| {
                        SectionSpec {
                            title: section.title,
                            rows: section
                                .rows
                                .iter()
                                .map(|row| RowSpec {
                                    target: Target::Row(row.id),
                                    label: row.label.clone(),
                                    widget: widget(row.id, &row.control),
                                    enabled: writable,
                                })
                                .collect(),
                        }
                    }));
                }
                None if !view.loading => {
                    layout.note = Some("No mouse found".to_owned());
                }
                None => {}
            }
        }
        layout
            .sections
            .push(desktop_section(view.desktop, writable));
        layout
    }

    /// Whether `other` has the same controls in the same places, so only
    /// values differ and the drawn controls can be updated in place.
    #[must_use]
    pub fn same_shape(&self, other: &Self) -> bool {
        let header_shape = |header: &Option<Header>| {
            header
                .as_ref()
                .map(|header| (header.names.clone(), header.selected))
        };
        header_shape(&self.header) == header_shape(&other.header)
            && self.note.is_some() == other.note.is_some()
            && self.sections.len() == other.sections.len()
            && self
                .sections
                .iter()
                .zip(&other.sections)
                .all(|(left, right)| {
                    left.title == right.title
                        && left.rows.len() == right.rows.len()
                        && left.rows.iter().zip(&right.rows).all(|(a, b)| {
                            a.target == b.target
                                && a.label == b.label
                                && widget_shape_eq(&a.widget, &b.widget)
                        })
                })
    }
}

fn widget_shape_eq(left: &Widget, right: &Widget) -> bool {
    match (left, right) {
        (Widget::Popup { items: a, .. }, Widget::Popup { items: b, .. }) => a == b,
        (Widget::Switch(_), Widget::Switch(_)) | (Widget::Text(_), Widget::Text(_)) => true,
        (
            Widget::Slider {
                min: a_min,
                max: a_max,
                step: a_step,
                unit: a_unit,
                ..
            },
            Widget::Slider {
                min: b_min,
                max: b_max,
                step: b_step,
                unit: b_unit,
                ..
            },
        ) => a_min == b_min && a_max == b_max && a_step == b_step && a_unit == b_unit,
        _ => false,
    }
}

fn header(devices: &[Device], selected: usize) -> Header {
    let status = devices.get(selected).map_or_else(String::new, |device| {
        device
            .battery_label()
            .or_else(|| (!device.online).then(|| "Not connected".to_owned()))
            .unwrap_or_default()
    });
    Header {
        names: devices.iter().map(|device| device.name.clone()).collect(),
        selected,
        status,
    }
}

fn widget(id: RowId, control: &Control) -> Widget {
    match control {
        Control::Popup { choices, selected } => Widget::Popup {
            items: choices.iter().map(|choice| choice.label.clone()).collect(),
            selected: *selected,
        },
        Control::Switch(on) => Widget::Switch(*on),
        Control::Slider {
            min,
            max,
            step,
            value,
        } => Widget::Slider {
            min: *min,
            max: *max,
            step: *step,
            value: *value,
            unit: if id == RowId::Dpi { "DPI" } else { "" },
        },
        Control::Text(text) => Widget::Text(text.clone()),
    }
}

fn desktop_section(settings: DesktopSettings, writable: bool) -> SectionSpec {
    let row = |row, label: &str, widget, enabled| RowSpec {
        target: Target::Desktop(row),
        label: label.to_owned(),
        widget,
        enabled: writable && enabled,
    };
    SectionSpec {
        title: "Desktop",
        rows: vec![
            row(
                DesktopRow::Speed,
                "Space switching speed",
                Widget::Popup {
                    items: Speed::ALL
                        .into_iter()
                        .map(|speed| speed.title().to_owned())
                        .collect(),
                    selected: Speed::ALL
                        .into_iter()
                        .position(|speed| speed == settings.speed)
                        .unwrap_or(0),
                },
                true,
            ),
            row(
                DesktopRow::Hotkeys,
                "Ctrl+Option+Arrow hotkeys",
                Widget::Switch(settings.hotkeys),
                true,
            ),
            row(
                DesktopRow::AutoRaise,
                "Auto-raise windows",
                Widget::Switch(settings.autoraise.enabled),
                true,
            ),
            row(
                DesktopRow::Delay,
                "Auto-raise delay",
                Widget::Popup {
                    items: Delay::ALL
                        .into_iter()
                        .map(|delay| delay.title().to_owned())
                        .collect(),
                    selected: Delay::ALL
                        .into_iter()
                        .position(|delay| delay == settings.autoraise.delay)
                        .unwrap_or(0),
                },
                settings.autoraise.enabled,
            ),
        ],
    }
}

/// The text a DPI slider's value label shows for `value`.
#[must_use]
pub fn value_text(value: f32, unit: &str) -> String {
    let number = Dpi::from_rounded(value).into_inner();
    if unit.is_empty() {
        number.to_string()
    } else {
        format!("{number} {unit}")
    }
}

#[cfg(test)]
mod tests {
    use openlogi_core::device::Capabilities;
    use openlogi_core::hid::{DeviceRoute, DpiCapabilities, DpiInfo};
    use openlogi_ipc::client::ProtocolSkew;

    use crate::desktop::AutoRaiseSettings;
    use crate::model::Reading;

    use super::*;

    fn device(key: &str, name: &str, online: bool) -> Device {
        Device {
            key: key.to_owned(),
            persistent: true,
            name: name.to_owned(),
            route: Some(DeviceRoute::Bolt {
                receiver_uid: "R".to_owned(),
                slot: 1,
            }),
            route_key: "bolt:R:1".to_owned(),
            online,
            battery: None,
            capabilities: Some(Capabilities {
                buttons: true,
                pointer: true,
                thumbwheel: true,
                ..Capabilities::default()
            }),
        }
    }

    fn desktop(autoraise: bool) -> DesktopSettings {
        DesktopSettings {
            speed: Speed::Quick,
            hotkeys: true,
            autoraise: AutoRaiseSettings {
                enabled: autoraise,
                delay: Delay::Medium,
            },
        }
    }

    fn facts() -> Facts {
        Facts {
            route_key: Some("bolt:R:1".to_owned()),
            capabilities: device("k", "n", true).capabilities,
            dpi: Reading::Ready(DpiInfo {
                current: Dpi::new(1600),
                capabilities: DpiCapabilities::new(vec![800, 1600, 3200]).expect("a list"),
            }),
            smartshift: Reading::Unsupported,
        }
    }

    fn build(agent: AgentState, devices: &[Device], desktop: DesktopSettings) -> Layout {
        let config = Config::default();
        let facts = facts();
        Layout::build(&View {
            agent,
            devices,
            selected: 0,
            facts: &facts,
            config: &config,
            desktop,
            loading: false,
        })
    }

    fn titles(layout: &Layout) -> Vec<&'static str> {
        layout
            .sections
            .iter()
            .map(|section| section.title)
            .collect()
    }

    fn row(layout: &Layout, target: Target) -> &RowSpec {
        layout
            .sections
            .iter()
            .flat_map(|section| &section.rows)
            .find(|row| row.target == target)
            .expect("the row exists")
    }

    #[test]
    fn a_connected_mouse_gets_a_header_its_sections_and_the_desktop_section() {
        let layout = build(
            AgentState::Connected,
            &[device("unit:aa", "MX Master 3S", true)],
            desktop(true),
        );
        let header = layout.header.as_ref().expect("a header");
        assert_eq!(header.names, ["MX Master 3S"]);
        assert_eq!(
            titles(&layout),
            ["Buttons", "Gesture button", "Pointer and scroll", "Desktop"]
        );
        let dpi = row(&layout, Target::Row(RowId::Dpi));
        assert!(
            matches!(&dpi.widget, Widget::Slider { value, unit: "DPI", .. } if (*value - 1600.0).abs() < f32::EPSILON)
        );
    }

    #[test]
    fn an_away_mouse_says_so_in_the_header() {
        let layout = build(
            AgentState::Connected,
            &[device("unit:aa", "MX Master 3S", false)],
            desktop(true),
        );
        assert_eq!(layout.header.expect("a header").status, "Not connected");
    }

    #[test]
    fn without_the_agent_no_device_value_is_shown_but_desktop_stays() {
        let layout = build(
            AgentState::NotRunning,
            &[device("unit:aa", "MX Master 3S", true)],
            desktop(true),
        );
        assert!(layout.header.is_none());
        assert_eq!(titles(&layout), ["Desktop"]);
        assert!(row(&layout, Target::Desktop(DesktopRow::Speed)).enabled);
    }

    #[test]
    fn a_version_mismatch_disables_every_control() {
        let skew = AgentState::VersionSkew(ProtocolSkew::AgentNewer { agent: 99 });
        let layout = build(skew, &[], desktop(true));
        assert!(
            layout
                .sections
                .iter()
                .flat_map(|section| &section.rows)
                .all(|row| !row.enabled)
        );
    }

    #[test]
    fn connected_with_no_mouse_says_so() {
        let layout = build(AgentState::Connected, &[], desktop(true));
        assert_eq!(layout.note.as_deref(), Some("No mouse found"));
        assert_eq!(titles(&layout), ["Desktop"]);
    }

    #[test]
    fn the_delay_follows_auto_raise() {
        let on = build(AgentState::NotRunning, &[], desktop(true));
        let off = build(AgentState::NotRunning, &[], desktop(false));
        assert!(row(&on, Target::Desktop(DesktopRow::Delay)).enabled);
        assert!(!row(&off, Target::Desktop(DesktopRow::Delay)).enabled);
        assert!(row(&off, Target::Desktop(DesktopRow::AutoRaise)).enabled);
    }

    #[test]
    fn desktop_popups_select_the_stored_values() {
        let layout = build(AgentState::NotRunning, &[], desktop(true));
        let Widget::Popup { items, selected } =
            &row(&layout, Target::Desktop(DesktopRow::Speed)).widget
        else {
            panic!("speed is a pop-up");
        };
        assert_eq!(items.len(), Speed::ALL.len());
        assert_eq!(items[*selected], Speed::Quick.title());
        let Widget::Popup { items, selected } =
            &row(&layout, Target::Desktop(DesktopRow::Delay)).widget
        else {
            panic!("delay is a pop-up");
        };
        assert_eq!(items[*selected], Delay::Medium.title());
    }

    #[test]
    fn only_a_value_change_keeps_the_shape() {
        let devices = [device("unit:aa", "MX Master 3S", true)];
        let before = build(AgentState::Connected, &devices, desktop(true));

        let mut moved = facts();
        moved.dpi = Reading::Ready(DpiInfo {
            current: Dpi::new(3200),
            capabilities: DpiCapabilities::new(vec![800, 1600, 3200]).expect("a list"),
        });
        let config = Config::default();
        let after = Layout::build(&View {
            agent: AgentState::Connected,
            devices: &devices,
            selected: 0,
            facts: &moved,
            config: &config,
            desktop: desktop(false),
            loading: false,
        });
        assert_ne!(before, after);
        assert!(before.same_shape(&after), "values only");

        let two = [
            device("unit:aa", "MX Master 3S", true),
            device("unit:bb", "MX Anywhere 3S", true),
        ];
        let picker = build(AgentState::Connected, &two, desktop(true));
        assert!(!before.same_shape(&picker), "a second mouse adds a picker");
        let down = build(AgentState::NotRunning, &devices, desktop(true));
        assert!(!before.same_shape(&down), "losing the agent drops sections");
    }

    #[test]
    fn the_dpi_label_rounds_and_carries_its_unit() {
        assert_eq!(value_text(1600.4, "DPI"), "1600 DPI");
        assert_eq!(value_text(3.0, ""), "3");
    }
}
