//! The menu-bar choices for the bundled helpers, encoded in menu-item tags.

use tracing::warn;

use super::autoraise::{self, Delay};
use super::strafe::{self, Speed};
use super::{Helper, restart};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Speed(Speed),
    ToggleHotkeys,
    ToggleAutoRaise,
    Delay(Delay),
}

const SPEED_BASE: isize = 100;
const HOTKEYS: isize = 200;
const AUTORAISE: isize = 300;
const DELAY_BASE: isize = 400;

impl Choice {
    pub fn tag(self) -> isize {
        match self {
            Self::Speed(speed) => SPEED_BASE + index_of(&Speed::ALL, &speed),
            Self::ToggleHotkeys => HOTKEYS,
            Self::ToggleAutoRaise => AUTORAISE,
            Self::Delay(delay) => DELAY_BASE + index_of(&Delay::ALL, &delay),
        }
    }

    pub fn from_tag(tag: isize) -> Option<Self> {
        match tag {
            HOTKEYS => Some(Self::ToggleHotkeys),
            AUTORAISE => Some(Self::ToggleAutoRaise),
            _ => nth(&Speed::ALL, tag - SPEED_BASE)
                .map(Self::Speed)
                .or_else(|| nth(&Delay::ALL, tag - DELAY_BASE).map(Self::Delay)),
        }
    }
}

fn index_of<T: PartialEq>(all: &[T], item: &T) -> isize {
    all.iter()
        .position(|candidate| candidate == item)
        .and_then(|index| isize::try_from(index).ok())
        .unwrap_or(isize::MIN)
}

fn nth<T: Copy>(all: &[T], index: isize) -> Option<T> {
    usize::try_from(index)
        .ok()
        .and_then(|index| all.get(index).copied())
}

/// Store `choice` in its tool's settings and restart that helper.
pub fn apply(choice: Choice) {
    let (result, helper) = match choice {
        Choice::Speed(speed) => (strafe::set_speed(speed), Helper::Strafe),
        Choice::ToggleHotkeys => (strafe::set_hotkeys(!strafe::hotkeys()), Helper::Strafe),
        Choice::ToggleAutoRaise => {
            let mut settings = autoraise::load();
            settings.enabled = !settings.enabled;
            (autoraise::save(settings), Helper::AutoRaise)
        }
        Choice::Delay(delay) => {
            let mut settings = autoraise::load();
            settings.delay = delay;
            (autoraise::save(settings), Helper::AutoRaise)
        }
    };
    match result {
        Ok(()) => restart(helper),
        Err(error) => warn!(%error, ?choice, "could not store the helper setting"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_choice_survives_its_tag() {
        let choices = Speed::ALL
            .into_iter()
            .map(Choice::Speed)
            .chain([Choice::ToggleHotkeys, Choice::ToggleAutoRaise])
            .chain(Delay::ALL.into_iter().map(Choice::Delay));
        for choice in choices {
            assert_eq!(Choice::from_tag(choice.tag()), Some(choice));
        }
        for unknown in [0, 99, 103, 250, 404, -1] {
            assert_eq!(Choice::from_tag(unknown), None, "tag {unknown}");
        }
    }
}
