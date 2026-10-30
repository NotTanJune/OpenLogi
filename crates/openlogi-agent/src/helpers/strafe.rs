//! Strafe's settings, kept in Strafe's own defaults domain, the one store both
//! the menu and the helper read. Values carry over from the standalone app.

use std::process::Command;

const DOMAIN: &str = "com.rileycx.strafe";
const SPEED_KEY: &str = "transitionSpeed";
const HOTKEYS_KEY: &str = "spaceHotkeysEnabled";

/// Strafe's `TransitionSpeed`, by its stored raw value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Speed {
    Instant,
    Quick,
    Smooth,
}

impl Speed {
    pub const ALL: [Self; 3] = [Self::Instant, Self::Quick, Self::Smooth];

    const fn raw(self) -> i64 {
        match self {
            Self::Instant => 0,
            Self::Quick => 1,
            Self::Smooth => 2,
        }
    }

    fn from_raw(raw: i64) -> Option<Self> {
        Self::ALL.into_iter().find(|speed| speed.raw() == raw)
    }

    pub const fn title(self) -> &'static str {
        match self {
            Self::Instant => "Instant (~40 ms)",
            Self::Quick => "Quick (~80 ms)",
            Self::Smooth => "Smooth (~110 ms)",
        }
    }
}

fn parse_int(output: &str) -> Option<i64> {
    output.trim().parse().ok()
}

fn parse_bool(output: &str) -> Option<bool> {
    match output.trim() {
        "1" | "true" | "YES" => Some(true),
        "0" | "false" | "NO" => Some(false),
        _ => None,
    }
}

fn read(key: &str) -> Option<String> {
    let output = Command::new("/usr/bin/defaults")
        .args(["read", DOMAIN, key])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn write(key: &str, kind: &str, value: &str) -> std::io::Result<()> {
    let status = Command::new("/usr/bin/defaults")
        .args(["write", DOMAIN, key, kind, value])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "defaults write {key} failed: {status}"
        )))
    }
}

/// The stored speed. Unset or unknown values read as Instant, as Strafe's
/// `TransitionSpeed.stored` reads them.
pub fn speed() -> Speed {
    read(SPEED_KEY)
        .and_then(|output| parse_int(&output))
        .and_then(Speed::from_raw)
        .unwrap_or(Speed::Instant)
}

pub fn set_speed(speed: Speed) -> std::io::Result<()> {
    write(SPEED_KEY, "-int", &speed.raw().to_string())
}

/// Whether the Ctrl+Opt+Arrow hotkeys are on; Strafe's default is on.
pub fn hotkeys() -> bool {
    read(HOTKEYS_KEY)
        .and_then(|output| parse_bool(&output))
        .unwrap_or(true)
}

pub fn set_hotkeys(enabled: bool) -> std::io::Result<()> {
    write(HOTKEYS_KEY, "-bool", if enabled { "true" } else { "false" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_output_parses() {
        assert_eq!(parse_int("2\n"), Some(2));
        assert_eq!(parse_int(""), None);
        assert_eq!(parse_bool("0\n"), Some(false));
        assert_eq!(parse_bool("1\n"), Some(true));
        assert_eq!(parse_bool("maybe"), None);
    }

    #[test]
    fn every_speed_survives_a_round_trip_through_its_raw_value() {
        for speed in Speed::ALL {
            assert_eq!(Speed::from_raw(speed.raw()), Some(speed));
        }
        assert_eq!(Speed::from_raw(9), None);
    }
}
