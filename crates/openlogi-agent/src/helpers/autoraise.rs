//! AutoRaise's settings, kept in AutoRaise's own config file, the one store
//! both the menu and the helper read.

use std::fmt::Write as _;
use std::path::PathBuf;

/// How long the pointer must rest on a window before AutoRaise raises it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delay {
    Instant,
    Short,
    Medium,
    Long,
}

impl Delay {
    pub const ALL: [Self; 4] = [Self::Instant, Self::Short, Self::Medium, Self::Long];

    /// AutoRaise's `delay`, in units of [`POLL_MILLIS`].
    const fn polls(self) -> u32 {
        match self {
            Self::Instant => 1,
            Self::Short => 2,
            Self::Medium => 4,
            Self::Long => 8,
        }
    }

    fn from_polls(polls: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|delay| delay.polls() == polls)
    }

    pub const fn title(self) -> &'static str {
        match self {
            Self::Instant => "Instant",
            Self::Short => "Short (100 ms)",
            Self::Medium => "Medium (200 ms)",
            Self::Long => "Long (400 ms)",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub enabled: bool,
    pub delay: Delay,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            delay: Delay::Short,
        }
    }
}

const POLL_MILLIS: u32 = 50;
/// A comment line, so AutoRaise itself ignores it; only the supervisor reads it.
const DISABLED_MARKER: &str = "#openlogi-disabled";
/// OpenLogi's own windows: the Actions Ring must never take focus.
const IGNORE_APPS: &str = "OpenLogi,OpenLogi Overlay";

fn parse(text: &str) -> Settings {
    let mut settings = Settings::default();
    for line in text.lines().map(str::trim) {
        if line == DISABLED_MARKER {
            settings.enabled = false;
        } else if let Some(value) = line.strip_prefix("delay=") {
            settings.delay = value
                .trim()
                .parse()
                .ok()
                .and_then(Delay::from_polls)
                .unwrap_or(Delay::Short);
        }
    }
    settings
}

fn render(settings: Settings) -> String {
    let mut text =
        String::from("# Managed by OpenLogi's menu-bar menu; edits here are overwritten.\n");
    if !settings.enabled {
        text.push_str(DISABLED_MARKER);
        text.push('\n');
    }
    let _ = write!(
        text,
        "pollMillis={POLL_MILLIS}\ndelay={}\nrequireMouseStop=true\nignoreApps=\"{IGNORE_APPS}\"\n",
        settings.delay.polls()
    );
    text
}

fn path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config/AutoRaise/config"))
}

/// The stored settings; the defaults when nothing is stored yet.
pub fn load() -> Settings {
    path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map_or_else(Settings::default, |text| parse(&text))
}

/// Store `settings`, and make sure AutoRaise's file exists before its first run.
pub fn save(settings: Settings) -> std::io::Result<()> {
    let path = path().ok_or_else(|| std::io::Error::other("HOME is not set"))?;
    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory)?;
    }
    std::fs::write(path, render(settings))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_setting_survives_a_round_trip() {
        for enabled in [true, false] {
            for delay in Delay::ALL {
                let settings = Settings { enabled, delay };
                assert_eq!(parse(&render(settings)), settings);
            }
        }
    }

    #[test]
    fn an_empty_or_foreign_file_reads_as_the_defaults() {
        assert_eq!(parse(""), Settings::default());
        assert_eq!(parse("delay=3\nwarpX=0.5\n").delay, Delay::Short);
    }

    #[test]
    fn openlogi_windows_never_take_focus() {
        assert!(render(Settings::default()).contains("ignoreApps=\"OpenLogi,OpenLogi Overlay\""));
    }
}
