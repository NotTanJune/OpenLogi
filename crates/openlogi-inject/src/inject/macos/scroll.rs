//! Synthetic scroll on macOS: the one-tick scroll actions, the fractional wheel, and the continuous smooth-scroll phases.

use crate::inject::{QuantizedScroll, ScrollQuantizer, SmoothScrollPhase};
use core_graphics::event::{CGEvent, CGEventTapLocation, EventField, ScrollEventUnit};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use openlogi_core::scroll::ScrollDelta;
use std::sync::{LazyLock, Mutex};

use super::tag_synthetic;

static PIXEL_SCROLL_QUANTIZER: LazyLock<Mutex<ScrollQuantizer>> =
    LazyLock::new(|| Mutex::new(ScrollQuantizer::default()));
static SMOOTH_SCROLL_QUANTIZER: LazyLock<Mutex<ScrollQuantizer>> =
    LazyLock::new(|| Mutex::new(ScrollQuantizer::default()));
static WHEEL_TICK_QUANTIZER: LazyLock<Mutex<ScrollQuantizer>> =
    LazyLock::new(|| Mutex::new(ScrollQuantizer::default()));

// `core-graphics` 0.25 does not expose these `CGEventTypes.h` fields.
const SCROLL_PHASE: u32 = 99; // kCGScrollWheelEventScrollPhase
const MOMENTUM_PHASE: u32 = 123; // kCGScrollWheelEventMomentumPhase

/// Post a synthetic scroll event for one tick in direction `(dx, dy)`. Unit
/// direction (-1/0/1) scaled by the fixed "one tick" pixel magnitude the
/// four `Scroll*`/`HorizontalScroll*` actions have always used.
pub(super) fn dispatch_scroll(dx: i8, dy: i8) {
    let Ok(src) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
        tracing::warn!("CGEventSource::new failed for scroll");
        return;
    };
    let v = i32::from(dy) * 3;
    let h = i32::from(dx) * 3;
    let Ok(ev) = CGEvent::new_scroll_event(src, ScrollEventUnit::PIXEL, 2, v, h, 0) else {
        tracing::warn!("CGEvent::new_scroll_event failed");
        return;
    };
    tag_synthetic(&ev);
    ev.post(CGEventTapLocation::HID);
}

pub(in crate::inject) fn post_scroll(delta: ScrollDelta) {
    if let ScrollDelta::WheelTicks { x, y } = delta {
        if let Some(event) = wheel_scroll_event(x, y, wheel_ticks(x, y)) {
            event.post(CGEventTapLocation::HID);
        }
        return;
    }

    let Ok(mut quantizer) = PIXEL_SCROLL_QUANTIZER.lock() else {
        tracing::warn!("macOS scroll quantizer mutex poisoned");
        return;
    };
    let delta = quantizer.quantize(delta, 1.0);
    drop(quantizer);
    if delta == QuantizedScroll::default() {
        return;
    }

    let Ok(src) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
        tracing::warn!("CGEventSource::new failed for precise scroll");
        return;
    };
    let Ok(ev) = CGEvent::new_scroll_event(src, ScrollEventUnit::PIXEL, 2, delta.y, delta.x, 0)
    else {
        tracing::warn!("CGEvent::new_scroll_event failed for precise scroll");
        return;
    };
    set_continuous_scroll_fields(&ev, delta);
    tag_synthetic(&ev);
    ev.post(CGEventTapLocation::HID);
}

pub(in crate::inject) fn post_smooth_scroll(delta: ScrollDelta, _phase: SmoothScrollPhase) {
    if let ScrollDelta::WheelTicks { x, y } = delta {
        if x == 0.0 && y == 0.0 {
            return;
        }
        if let Some(event) = wheel_scroll_event(x, y, wheel_ticks(x, y)) {
            event.post(CGEventTapLocation::HID);
        }
        return;
    }
    post_continuous_scroll(delta, None);
}

/// Wheel ticks become pixels here: a phase declares a trackpad gesture, and a
/// gesture is continuous input. Stamping one on a non-continuous line event
/// built a hybrid that Chromium scrolled as one animated line jump per
/// thumb-wheel report.
pub(in crate::inject) fn post_phased_scroll(delta: ScrollDelta, phase: SmoothScrollPhase) {
    post_continuous_scroll(gesture_points(delta), Some(phase));
}

/// Points one wheel tick scrolls inside a phased gesture: the pixels-per-line
/// step Chromium and WebKit give one line of a non-continuous wheel event, so
/// a gesture scrolls there as far as the line events it replaced.
const POINTS_PER_GESTURE_TICK: f64 = 40.0;

/// A gesture frame's distance in points.
fn gesture_points(delta: ScrollDelta) -> ScrollDelta {
    match delta {
        ScrollDelta::WheelTicks { x, y } => {
            ScrollDelta::pixels(x * POINTS_PER_GESTURE_TICK, y * POINTS_PER_GESTURE_TICK)
        }
        pixels @ ScrollDelta::Pixels { .. } => pixels,
    }
}

/// Post one continuous pixel event, phaseless (`None`) or stamped with a
/// gesture phase. `delta` is in pixels.
///
/// Phaseless is the default for smoothed wheel output: continuous events with
/// zeroed gesture/momentum phases scroll everywhere a wheel does, while a
/// Began/Changed/Ended stream declares a trackpad gesture — which switches
/// AppKit/WebKit into gesture handling (rubber-band overscroll appears on
/// wheel scrolls, and WebKit's gesture latching can starve sites that scroll
/// from JS wheel handlers). A phase is stamped only where a gesture is the
/// point, such as a horizontal swipe AppKit's recognisers must see.
fn post_continuous_scroll(delta: ScrollDelta, phase: Option<SmoothScrollPhase>) {
    let Ok(mut quantizer) = SMOOTH_SCROLL_QUANTIZER.lock() else {
        tracing::warn!("macOS smooth-scroll quantizer mutex poisoned");
        return;
    };
    let delta = quantizer.quantize(delta, 1.0);
    drop(quantizer);

    // Sub-pixel frames quantize to zero; posting a phaseless one would only
    // flood the event stream. A phased zero-distance frame still carries its
    // phase, and a gesture's Ended or Cancelled must always arrive.
    if phase.is_none() && delta == QuantizedScroll::default() {
        return;
    }
    if let Some(event) = continuous_scroll_event(delta, phase) {
        event.post(CGEventTapLocation::HID);
    }
}

/// Build (without posting) one tagged continuous pixel event.
fn continuous_scroll_event(
    delta: QuantizedScroll,
    phase: Option<SmoothScrollPhase>,
) -> Option<CGEvent> {
    let Ok(src) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
        tracing::warn!("CGEventSource::new failed for smooth scroll");
        return None;
    };
    let Ok(ev) = CGEvent::new_scroll_event(src, ScrollEventUnit::PIXEL, 2, delta.y, delta.x, 0)
    else {
        tracing::warn!("CGEvent::new_scroll_event failed for smooth scroll");
        return None;
    };
    set_continuous_scroll_fields(&ev, delta);
    set_scroll_phase(&ev, phase);
    tag_synthetic(&ev);
    Some(ev)
}

/// The whole-line tick count for one wheel event, carrying the fraction to the
/// next event so the ticks of a stream add up to its distance.
fn wheel_ticks(x: f64, y: f64) -> QuantizedScroll {
    let Ok(mut quantizer) = WHEEL_TICK_QUANTIZER.lock() else {
        tracing::warn!("macOS wheel-tick quantizer mutex poisoned");
        return QuantizedScroll::default();
    };
    quantizer.quantize(ScrollDelta::wheel_ticks(x, y), 1.0)
}

/// Build (without posting) one tagged, non-continuous wheel event whose signed
/// 16.16 line distance keeps the fractional part instead of rounding to whole
/// lines.
///
/// `ticks` fills the integer line delta, which a device's wheel events always
/// carry: Chromium reads it — not the fixed-point distance — as the event's
/// `wheel_ticks` (`WebMouseWheelEventBuilder::Build`), and Blink derives the
/// legacy `wheelDelta` VS Code scrolls by from it, so a zero there leaves VS
/// Code still. It is written before the fixed-point delta so the fractional
/// distance every other reader uses stays the canonical one.
fn wheel_scroll_event(x: f64, y: f64, ticks: QuantizedScroll) -> Option<CGEvent> {
    let Ok(src) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
        tracing::warn!("CGEventSource::new failed for wheel scroll");
        return None;
    };
    let Ok(ev) = CGEvent::new_scroll_event(src, ScrollEventUnit::LINE, 2, 0, 0, 0) else {
        tracing::warn!("CGEvent::new_scroll_event failed for wheel scroll");
        return None;
    };
    ev.set_integer_value_field(EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_1, ticks.y.into());
    ev.set_integer_value_field(EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_2, ticks.x.into());
    ev.set_double_value_field(EventField::SCROLL_WHEEL_EVENT_FIXED_POINT_DELTA_AXIS_1, y);
    ev.set_double_value_field(EventField::SCROLL_WHEEL_EVENT_FIXED_POINT_DELTA_AXIS_2, x);
    ev.set_integer_value_field(EventField::SCROLL_WHEEL_EVENT_IS_CONTINUOUS, 0);
    tag_synthetic(&ev);
    Some(ev)
}

fn set_scroll_phase(event: &CGEvent, phase: Option<SmoothScrollPhase>) {
    if let Some(phase) = phase {
        event.set_integer_value_field(SCROLL_PHASE, scroll_phase_value(phase));
        event.set_integer_value_field(MOMENTUM_PHASE, 0);
    }
}

const fn scroll_phase_value(phase: SmoothScrollPhase) -> i64 {
    match phase {
        SmoothScrollPhase::Began => 1,
        SmoothScrollPhase::Changed => 2,
        SmoothScrollPhase::Ended => 4,
        SmoothScrollPhase::Cancelled => 8,
    }
}

fn set_continuous_scroll_fields(event: &CGEvent, delta: QuantizedScroll) {
    event.set_integer_value_field(EventField::SCROLL_WHEEL_EVENT_IS_CONTINUOUS, 1);
    set_continuous_axis(
        event,
        delta.y,
        EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_1,
        EventField::SCROLL_WHEEL_EVENT_FIXED_POINT_DELTA_AXIS_1,
        EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1,
    );
    set_continuous_axis(
        event,
        delta.x,
        EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_2,
        EventField::SCROLL_WHEEL_EVENT_FIXED_POINT_DELTA_AXIS_2,
        EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2,
    );
}

fn set_continuous_axis(
    event: &CGEvent,
    points: i32,
    line_field: u32,
    fixed_field: u32,
    point_field: u32,
) {
    const POINTS_PER_LINE: i64 = 10;
    const FIXED_POINT_SCALE: i64 = 1 << 16;
    let points = i64::from(points);
    // Write order is load-bearing: a scroll CGEvent keeps one canonical
    // distance, and writing the coarse integer *line* field makes it the
    // canonical value — the point delta is then re-derived from it at 8
    // points per line, discarding pixel precision (measured on macOS 15.7:
    // frames under 10 points arrived as zero and the rest quantized to
    // 8-point steps). Writing the point delta last keeps it authoritative.
    event.set_integer_value_field(line_field, points / POINTS_PER_LINE);
    event.set_integer_value_field(fixed_field, points * FIXED_POINT_SCALE / POINTS_PER_LINE);
    event.set_integer_value_field(point_field, points);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn horizontal(x: i32) -> QuantizedScroll {
        QuantizedScroll { x, y: 0 }
    }

    /// Smoothed wheel output must stay phaseless: a phase declares a trackpad
    /// gesture to AppKit/WebKit.
    #[test]
    fn smoothed_wheel_events_carry_no_phase() {
        let event = continuous_scroll_event(horizontal(10), None).expect("a scroll event");
        assert_eq!(event.get_integer_value_field(SCROLL_PHASE), 0);
        assert_eq!(
            event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_IS_CONTINUOUS),
            1
        );
    }

    /// A phased gesture's phases reach the event, terminal zero-distance
    /// frames included, or swipe recognisers never see a gesture.
    #[test]
    fn phased_gesture_events_carry_every_phase() {
        for (phase, delta, expected) in [
            (SmoothScrollPhase::Began, horizontal(10), 1),
            (SmoothScrollPhase::Changed, horizontal(-10), 2),
            (SmoothScrollPhase::Ended, horizontal(0), 4),
            (SmoothScrollPhase::Cancelled, horizontal(0), 8),
        ] {
            let event = continuous_scroll_event(delta, Some(phase)).expect("a scroll event");
            assert_eq!(
                event.get_integer_value_field(SCROLL_PHASE),
                expected,
                "{phase:?}"
            );
            assert_eq!(event.get_integer_value_field(MOMENTUM_PHASE), 0);
            assert_eq!(
                event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA),
                crate::SYNTHETIC_EVENT_USER_DATA
            );
        }
    }

    #[test]
    fn wheel_event_preserves_fractional_distance_without_a_phase() {
        let event =
            wheel_scroll_event(-0.025, 0.05, QuantizedScroll::default()).expect("a wheel event");
        let y =
            event.get_double_value_field(EventField::SCROLL_WHEEL_EVENT_FIXED_POINT_DELTA_AXIS_1);
        let x =
            event.get_double_value_field(EventField::SCROLL_WHEEL_EVENT_FIXED_POINT_DELTA_AXIS_2);
        assert!((y - 0.05).abs() < 1.0e-4, "{y}");
        assert!((x + 0.025).abs() < 1.0e-4, "{x}");
        assert_eq!(event.get_integer_value_field(SCROLL_PHASE), 0);
        assert_eq!(
            event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA),
            crate::SYNTHETIC_EVENT_USER_DATA
        );
    }

    /// A gesture is continuous input: its wheel ticks become points, in both
    /// directions, while pixel distance passes through untouched.
    #[test]
    fn gesture_wheel_ticks_become_points() {
        assert_eq!(
            gesture_points(ScrollDelta::wheel_ticks(0.5, -0.25)),
            ScrollDelta::pixels(20.0, -10.0)
        );
        assert_eq!(
            gesture_points(ScrollDelta::wheel_ticks(-0.5, 0.0)),
            ScrollDelta::pixels(-20.0, 0.0)
        );
        assert_eq!(
            gesture_points(ScrollDelta::pixels(3.0, -4.0)),
            ScrollDelta::pixels(3.0, -4.0)
        );
    }

    /// The event a gesture frame of wheel ticks becomes: continuous, carrying
    /// its points and its phase.
    #[test]
    fn gesture_frames_are_continuous_point_events() {
        let ScrollDelta::Pixels { x, .. } = gesture_points(ScrollDelta::wheel_ticks(-0.75, 0.0))
        else {
            panic!("gesture distance is in points");
        };
        assert!((x + 30.0).abs() < f64::EPSILON, "{x}");
        let event = continuous_scroll_event(horizontal(-30), Some(SmoothScrollPhase::Changed))
            .expect("a scroll event");
        assert_eq!(
            event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_IS_CONTINUOUS),
            1
        );
        assert_eq!(
            event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2),
            -30
        );
        assert_eq!(
            event.get_integer_value_field(SCROLL_PHASE),
            scroll_phase_value(SmoothScrollPhase::Changed)
        );
    }

    /// The integer line delta carries the ticks Chromium reads, while the
    /// fixed-point distance written after it keeps its fraction.
    #[test]
    fn wheel_event_carries_integer_ticks_beside_fractional_distance() {
        // Neither the rounded (-1, 2) nor the truncated (0, 1) distance, so
        // the test fails if writing the fixed-point fields replaces the
        // carried ticks with ones derived from the distance.
        let ticks = QuantizedScroll { x: -2, y: 3 };
        let event = wheel_scroll_event(-0.75, 1.6, ticks).expect("a wheel event");
        assert_eq!(
            event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_1),
            3
        );
        assert_eq!(
            event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_2),
            -2
        );
        let y =
            event.get_double_value_field(EventField::SCROLL_WHEEL_EVENT_FIXED_POINT_DELTA_AXIS_1);
        let x =
            event.get_double_value_field(EventField::SCROLL_WHEEL_EVENT_FIXED_POINT_DELTA_AXIS_2);
        assert!((y - 1.6).abs() < 1.0e-4, "{y}");
        assert!((x + 0.75).abs() < 1.0e-4, "{x}");
    }
}
