//! The mice the agent reports, each resolved to the config key its settings
//! live under.
//!
//! The key is decided the way the upstream GUI decides it
//! (`state/devices.rs::build_device_list`): [`Config::resolve_device_key`] over
//! the device's [`DeviceStableId`] and, when it is online, its own identity.
//! The agent resolves the same key, so a setting written under it is the one
//! the agent applies.
//!
//! Not mirrored: the upstream GUI also lists devices that were seen before and
//! are absent from this snapshot, and folds a route into its device's entry
//! (`Config::adopt_route`) on every sighting. A sleeping Bolt mouse stays in the
//! receiver's inventory as offline, which is the case this app needs, and
//! resolution answers the same key with or without the adoption.

use openlogi_core::config::Config;
use openlogi_core::device::{
    BatteryInfo, BatteryLevel, BatteryStatus, Capabilities, DeviceInventory, DeviceKind,
    PairedDevice, prettify_codename,
};
use openlogi_core::device_order::{DeviceIdentity, DeviceStableId};
use openlogi_core::hid::DeviceRoute;

use crate::model::{Facts, Reading};

/// One mouse, as the page header and the write paths need it.
#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    /// The key its settings are stored under in `config.toml`.
    pub key: String,
    /// Whether `key` names one physical device. A transient key (a device that
    /// reports no identity) must not be written to the file.
    pub persistent: bool,
    /// The name to show.
    pub name: String,
    /// The route the agent reaches it by, when it has one.
    pub route: Option<DeviceRoute>,
    /// The route's key in the config's link index.
    pub route_key: String,
    /// Whether it answered at the last enumeration.
    pub online: bool,
    /// Its last battery reading.
    pub battery: Option<BatteryInfo>,
    /// What it announced it can do, when it was probed.
    pub capabilities: Option<Capabilities>,
}

impl Device {
    /// The mice in `inventories`, in the stable order the upstream carousel
    /// uses. Keyboards, receivers and other kinds belong to features this app
    /// does not have.
    #[must_use]
    pub fn list(config: &Config, inventories: &[DeviceInventory]) -> Vec<Self> {
        let mut devices: Vec<(DeviceStableId, Self)> = inventories
            .iter()
            .flat_map(|inventory| {
                inventory
                    .paired
                    .iter()
                    .filter(|paired| is_mouse(paired))
                    .map(move |paired| Self::resolve(config, inventory, paired))
            })
            .collect();
        devices.sort_by(|(left, _), (right, _)| left.cmp(right));
        devices.into_iter().map(|(_, device)| device).collect()
    }

    fn resolve(
        config: &Config,
        inventory: &DeviceInventory,
        paired: &PairedDevice,
    ) -> (DeviceStableId, Self) {
        let route = DeviceRoute::for_slot(inventory, paired.slot);
        let (serial, unit_id) = paired.model_info.as_ref().map_or((None, [0; 4]), |model| {
            (model.serial_number.as_deref(), model.unit_id)
        });
        let stable = DeviceStableId::from_parts(route.as_ref(), paired.slot, serial, unit_id);
        let identity = DeviceIdentity::from_parts(serial, unit_id);
        let (key, persistent) = config
            .resolve_device_key(&stable, paired.online.then_some(&identity))
            .map_or_else(
                || (stable.runtime_key(), false),
                |key| (key.into_string(), true),
            );
        let name = paired.codename.as_deref().map_or_else(
            || format!("Receiver slot {}", paired.slot),
            prettify_codename,
        );
        let device = Self {
            key,
            persistent,
            name,
            route,
            route_key: stable.route_key(),
            online: paired.online,
            battery: paired.battery.clone(),
            capabilities: paired.capabilities,
        };
        (stable, device)
    }

    /// The facts the page can state without asking the device anything: the
    /// route and capabilities, with the live readings unavailable. The agent's
    /// `facts` call fills the readings for an online device.
    #[must_use]
    pub fn facts(&self) -> Facts {
        Facts {
            route_key: Some(self.route_key.clone()),
            capabilities: self.capabilities,
            dpi: Reading::Unavailable,
            smartshift: Reading::Unavailable,
        }
    }

    /// The battery as text for the header, or `None` when the device reports
    /// none. A percentage wins; a device that reports only a level shows it.
    #[must_use]
    pub fn battery_label(&self) -> Option<String> {
        let battery = self.battery.as_ref()?;
        let charge = match (battery.percentage, battery.level) {
            (Some(percentage), _) => format!("{percentage}%"),
            (None, BatteryLevel::Critical) => "Critical".to_owned(),
            (None, BatteryLevel::Low) => "Low".to_owned(),
            (None, BatteryLevel::Good) => "Good".to_owned(),
            (None, BatteryLevel::Full) => "Full".to_owned(),
            (None, BatteryLevel::Unknown) => return None,
        };
        let charging = matches!(
            battery.status,
            BatteryStatus::Charging | BatteryStatus::ChargingSlow
        );
        Some(if charging {
            format!("{charge}, charging")
        } else {
            charge
        })
    }
}

/// A mouse by kind, or by what it announced: a misread kind must not hide a
/// device that reports adjustable DPI (upstream issue #127).
fn is_mouse(paired: &PairedDevice) -> bool {
    matches!(paired.kind, DeviceKind::Mouse | DeviceKind::Trackball)
        || paired
            .capabilities
            .is_some_and(|caps| caps.pointer || caps.pointer_scaling)
}

#[cfg(test)]
mod tests {
    use openlogi_core::device::{DeviceModelInfo, DeviceTransports, ReceiverInfo};

    use super::*;

    fn receiver(unique_id: &str) -> ReceiverInfo {
        ReceiverInfo {
            name: "Logi Bolt Receiver".to_owned(),
            vendor_id: 0x046d,
            product_id: 0xc548,
            unique_id: Some(unique_id.to_owned()),
        }
    }

    fn model_info(unit_id: [u8; 4]) -> DeviceModelInfo {
        DeviceModelInfo {
            entity_count: 1,
            serial_number: None,
            unit_id,
            transports: DeviceTransports::default(),
            model_ids: [0xb034, 0, 0],
            extended_model_id: 2,
        }
    }

    fn paired(slot: u8, kind: DeviceKind, codename: Option<&str>, unit: [u8; 4]) -> PairedDevice {
        PairedDevice {
            slot,
            codename: codename.map(str::to_owned),
            wpid: Some(0xb034),
            kind,
            online: true,
            battery: None,
            model_info: Some(model_info(unit)),
            capabilities: None,
            settings_revision: 0,
        }
    }

    fn inventory(paired: Vec<PairedDevice>) -> DeviceInventory {
        DeviceInventory {
            receiver: receiver("RECEIVER-1"),
            paired,
        }
    }

    #[test]
    fn only_mice_are_listed_and_a_misread_kind_is_rescued_by_capabilities() {
        let mut misread = paired(3, DeviceKind::Unknown, Some("MX Anywhere"), [3; 4]);
        misread.capabilities = Some(Capabilities {
            pointer: true,
            ..Capabilities::default()
        });
        let inventories = [inventory(vec![
            paired(1, DeviceKind::Mouse, Some("MX Master 3S"), [1; 4]),
            paired(2, DeviceKind::Keyboard, Some("MX Keys"), [2; 4]),
            misread,
        ])];
        let devices = Device::list(&Config::default(), &inventories);
        let names: Vec<_> = devices.iter().map(|device| device.name.as_str()).collect();
        assert_eq!(names, ["MX Master 3S", "MX Anywhere"]);
    }

    #[test]
    fn the_key_is_the_identity_the_agent_resolves() {
        let config = Config::default();
        let inventories = [inventory(vec![paired(
            1,
            DeviceKind::Mouse,
            Some("MX Master 3S"),
            [0x6b, 0xe9, 0xd3, 0x00],
        )])];
        let [device] = Device::list(&config, &inventories)
            .try_into()
            .expect("one mouse");
        assert_eq!(device.key, "unit:6be9d300");
        assert!(device.persistent);
        assert_eq!(device.route_key, "receiver:receiver-1:slot:1");
        assert!(device.route.is_some());
    }

    #[test]
    fn a_legacy_entry_holding_the_settings_keeps_its_key() {
        // Right after an upgrade a receiver mouse's settings still live under
        // the route-derived key; answering the identity key would show and
        // write defaults while the agent keeps applying the legacy entry.
        let mut config = Config::default();
        config.set_dpi(
            "receiver:receiver-1:slot:1",
            openlogi_core::hid::Dpi::new(800),
        );
        let inventories = [inventory(vec![paired(
            1,
            DeviceKind::Mouse,
            Some("MX Master 3S"),
            [0x6b, 0xe9, 0xd3, 0x00],
        )])];
        let [device] = Device::list(&config, &inventories)
            .try_into()
            .expect("one mouse");
        assert_eq!(device.key, "receiver:receiver-1:slot:1");
    }

    #[test]
    fn a_sleeping_device_is_keyed_by_its_receiver_slot() {
        // Asleep, a device's identity cannot be read, so its settings are
        // reached through the receiver and slot it occupies.
        let mut asleep = paired(
            1,
            DeviceKind::Mouse,
            Some("Mouse"),
            [0x6b, 0xe9, 0xd3, 0x00],
        );
        asleep.online = false;
        let inventories = [inventory(vec![asleep])];
        let [device] = Device::list(&Config::default(), &inventories)
            .try_into()
            .expect("one");
        assert_eq!(device.key, "receiver:receiver-1:slot:1");
        assert!(device.persistent);
        assert!(!device.online);
    }

    #[test]
    fn shouted_codenames_are_tidied_and_a_missing_one_names_the_slot() {
        let inventories = [inventory(vec![
            paired(
                1,
                DeviceKind::Mouse,
                Some("G502 LIGHTSPEED WIRELESS MOUSE"),
                [1; 4],
            ),
            paired(2, DeviceKind::Mouse, None, [2; 4]),
        ])];
        let devices = Device::list(&Config::default(), &inventories);
        assert_eq!(devices[0].name, "G502 Lightspeed Wireless Mouse");
        assert_eq!(devices[1].name, "Receiver slot 2");
    }

    #[test]
    fn facts_carry_the_route_and_capabilities_but_no_live_readings() {
        let mut mouse = paired(1, DeviceKind::Mouse, Some("MX"), [1; 4]);
        mouse.capabilities = Some(Capabilities {
            buttons: true,
            ..Capabilities::default()
        });
        let inventories = [inventory(vec![mouse])];
        let [device] = Device::list(&Config::default(), &inventories)
            .try_into()
            .expect("one");
        let facts = device.facts();
        assert_eq!(
            facts.route_key.as_deref(),
            Some("receiver:receiver-1:slot:1")
        );
        assert!(facts.capabilities.is_some_and(|caps| caps.buttons));
        assert_eq!(facts.dpi, Reading::Unavailable);
        assert_eq!(facts.smartshift, Reading::Unavailable);
    }

    #[test]
    fn battery_text_prefers_the_percentage_and_notes_charging() {
        let device_with = |battery: Option<BatteryInfo>| {
            let mut mouse = paired(1, DeviceKind::Mouse, Some("MX"), [1; 4]);
            mouse.battery = battery;
            let inventories = [inventory(vec![mouse])];
            Device::list(&Config::default(), &inventories).remove(0)
        };
        let reading = |percentage, level, status| {
            Some(BatteryInfo {
                percentage,
                level,
                status,
            })
        };
        assert_eq!(device_with(None).battery_label(), None);
        assert_eq!(
            device_with(reading(
                Some(82),
                BatteryLevel::Good,
                BatteryStatus::Discharging
            ))
            .battery_label()
            .as_deref(),
            Some("82%")
        );
        assert_eq!(
            device_with(reading(
                Some(40),
                BatteryLevel::Good,
                BatteryStatus::Charging
            ))
            .battery_label()
            .as_deref(),
            Some("40%, charging")
        );
        assert_eq!(
            device_with(reading(None, BatteryLevel::Low, BatteryStatus::Discharging))
                .battery_label()
                .as_deref(),
            Some("Low")
        );
        assert_eq!(
            device_with(reading(None, BatteryLevel::Unknown, BatteryStatus::Unknown))
                .battery_label(),
            None
        );
    }
}
