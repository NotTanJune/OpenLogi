//! The window's AppKit views.
//!
//! Only standard controls, the system font and semantic system colours are
//! used (`labelColor`, `secondaryLabelColor`), with no custom drawing, so light
//! and dark mode, the accent colour and the accessibility settings all follow
//! macOS. This module draws a [`Layout`] and updates the controls it drew; what
//! a change *means* lives in `ui.rs`.
//!
//! Every control sends [`CHANGED`] to one target, with a tag that indexes
//! [`Rendered::rows`] (or [`DEVICE_TAG`] for the device picker).

#![expect(
    unsafe_code,
    reason = "objc2 calls AppKit marks unsafe: window creation, control targets and actions"
)]

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{MainThreadMarker, sel};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSButton, NSColor, NSControl, NSControlStateValueOff,
    NSControlStateValueOn, NSFont, NSFontWeightRegular, NSFontWeightSemibold, NSLayoutAttribute,
    NSLayoutConstraint, NSLayoutConstraintOrientation, NSLayoutPriorityDefaultLow, NSPopUpButton,
    NSScrollView, NSSlider, NSStackView, NSStackViewDistribution, NSSwitch, NSTextAlignment,
    NSTextField, NSUserInterfaceLayoutOrientation, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{
    NSArray, NSEdgeInsets, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};

use openlogi_simple::layout::{Header, Layout, RowSpec, SectionSpec, Target, Widget, value_text};

/// The action every row control sends.
fn changed() -> Sel {
    sel!(controlChanged:)
}

/// The action of the Retry button.
fn retry_action() -> Sel {
    sel!(retry:)
}
/// The tag of the device picker; row controls carry their row index.
pub const DEVICE_TAG: isize = -1;

/// The window's fixed width in points. It resizes vertically only.
const WINDOW_WIDTH: f64 = 460.0;
/// The gap between the window edge and the content.
const MARGIN: f64 = 20.0;
/// The width of a row's control, so every control's right edge aligns.
const CONTROL_WIDTH: f64 = 220.0;
/// The width of a slider's value label.
const VALUE_WIDTH: f64 = 72.0;

/// Bring `window` to the front, restoring it if it was minimised, and make the
/// app active.
pub fn present(mtm: MainThreadMarker, window: &NSWindow) {
    if window.isMiniaturized() {
        window.deminiaturize(None);
    }
    window.makeKeyAndOrderFront(None);
    let app = NSApplication::sharedApplication(mtm);
    // `activate` is macOS 14; the bundles declare 13, where the older call is
    // the only one.
    if app.respondsToSelector(sel!(activate)) {
        app.activate();
    } else {
        #[expect(deprecated, reason = "the only activation call on macOS 13")]
        app.activateIgnoringOtherApps(true);
    }
}

/// The window and the views that outlive a redraw.
pub struct Shell {
    /// The window.
    pub window: Retained<NSWindow>,
    top: Retained<NSStackView>,
    banner_row: Retained<NSStackView>,
    banner: Retained<NSTextField>,
    retry: Retained<NSButton>,
    notice: Retained<NSTextField>,
    content: Retained<NSStackView>,
}

/// One drawn row: what it edits and the control to read or update.
pub struct BuiltRow {
    /// What the row edits.
    pub target: Target,
    /// The control.
    pub handle: Handle,
}

/// The control drawn for a row.
pub enum Handle {
    /// A pop-up button.
    Popup(Retained<NSPopUpButton>),
    /// A switch.
    Switch(Retained<NSSwitch>),
    /// A slider and its value label.
    Slider {
        /// The slider.
        slider: Retained<NSSlider>,
        /// The label showing its value.
        value: Retained<NSTextField>,
        /// The unit written after the value.
        unit: &'static str,
    },
    /// A read-only label.
    Text(Retained<NSTextField>),
}

/// What [`Shell::fill`] drew.
pub struct Rendered {
    /// The rows in layout order; a control's tag indexes this.
    pub rows: Vec<BuiltRow>,
    /// The device picker, when more than one mouse is listed.
    pub device_popup: Option<Retained<NSPopUpButton>>,
    /// The battery label of the header.
    pub status: Option<Retained<NSTextField>>,
}

impl Shell {
    /// Build the window, empty. `target` receives every control's action and
    /// must be kept alive by the caller, since AppKit holds targets weakly.
    pub fn new(mtm: MainThreadMarker, target: &AnyObject) -> Self {
        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable
            | NSWindowStyleMask::Resizable;
        // SAFETY: a plain designated initializer with a valid rect, style and
        // backing store, called on the main thread (`mtm`).
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                mtm.alloc(),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WINDOW_WIDTH, 640.0)),
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: with the default (true) the window releases itself on close,
        // and `Retained` would release it a second time.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&NSString::from_str("OpenLogi"));
        window.setContentMinSize(NSSize::new(WINDOW_WIDTH, 260.0));
        window.setContentMaxSize(NSSize::new(WINDOW_WIDTH, 4000.0));

        let banner = NSTextField::wrappingLabelWithString(&NSString::from_str(""), mtm);
        banner.setPreferredMaxLayoutWidth(WINDOW_WIDTH - 2.0 * MARGIN);
        banner.setTextColor(Some(&NSColor::labelColor()));
        // SAFETY: `target` is a live object implementing `retry:`, kept alive
        // by the caller; the button holds it weakly.
        let retry = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str("Retry"),
                Some(target),
                Some(retry_action()),
                mtm,
            )
        };
        let banner_row = stack(mtm, NSUserInterfaceLayoutOrientation::Horizontal, 12.0);
        banner_row.setAlignment(NSLayoutAttribute::CenterY);
        banner_row.addArrangedSubview(&banner);
        banner_row.addArrangedSubview(&retry);
        banner_row.setHidden(true);

        let notice = NSTextField::wrappingLabelWithString(&NSString::from_str(""), mtm);
        notice.setPreferredMaxLayoutWidth(WINDOW_WIDTH - 2.0 * MARGIN);
        notice.setTextColor(Some(&NSColor::secondaryLabelColor()));
        notice.setHidden(true);

        let top = stack(mtm, NSUserInterfaceLayoutOrientation::Vertical, 8.0);
        top.setAlignment(NSLayoutAttribute::Leading);
        top.setEdgeInsets(NSEdgeInsets {
            top: 16.0,
            left: MARGIN,
            bottom: 4.0,
            right: MARGIN,
        });
        top.addArrangedSubview(&banner_row);
        top.addArrangedSubview(&notice);
        top.setHidden(true);

        let content = stack(mtm, NSUserInterfaceLayoutOrientation::Vertical, 22.0);
        content.setAlignment(NSLayoutAttribute::Leading);
        content.setEdgeInsets(NSEdgeInsets {
            top: MARGIN,
            left: MARGIN,
            bottom: MARGIN,
            right: MARGIN,
        });
        content.setTranslatesAutoresizingMaskIntoConstraints(false);

        let scroll = NSScrollView::new(mtm);
        scroll.setHasVerticalScroller(true);
        scroll.setAutohidesScrollers(true);
        scroll.setDrawsBackground(false);
        scroll.setDocumentView(Some(&content));
        // Pinned to the clip view's top and sides, so a short page sits at the
        // top and the document grows downward and scrolls.
        let clip = scroll.contentView();
        activate(&[
            &*content
                .topAnchor()
                .constraintEqualToAnchor(&clip.topAnchor()),
            &*content
                .leadingAnchor()
                .constraintEqualToAnchor(&clip.leadingAnchor()),
            &*content
                .trailingAnchor()
                .constraintEqualToAnchor(&clip.trailingAnchor()),
        ]);

        let root = stack(mtm, NSUserInterfaceLayoutOrientation::Vertical, 0.0);
        root.setAlignment(NSLayoutAttribute::Width);
        root.setDistribution(NSStackViewDistribution::Fill);
        top.setContentHuggingPriority_forOrientation(
            750.0,
            NSLayoutConstraintOrientation::Vertical,
        );
        scroll.setContentHuggingPriority_forOrientation(
            NSLayoutPriorityDefaultLow,
            NSLayoutConstraintOrientation::Vertical,
        );
        root.addArrangedSubview(&top);
        root.addArrangedSubview(&scroll);
        window.setContentView(Some(&root));

        Self {
            window,
            top,
            banner_row,
            banner,
            retry,
            notice,
            content,
        }
    }

    /// Show or clear the agent banner. `retry` offers the Retry button.
    pub fn set_banner(&self, banner: Option<(&str, bool)>) {
        match banner {
            Some((message, retry)) => {
                self.banner.setStringValue(&NSString::from_str(message));
                self.retry.setHidden(!retry);
                self.retry.setEnabled(true);
                self.banner_row.setHidden(false);
            }
            None => self.banner_row.setHidden(true),
        }
        self.sync_top();
    }

    /// Show or clear the short inline message.
    pub fn set_notice(&self, notice: Option<&str>) {
        match notice {
            Some(message) => {
                self.notice.setStringValue(&NSString::from_str(message));
                self.notice.setHidden(false);
            }
            None => self.notice.setHidden(true),
        }
        self.sync_top();
    }

    /// Grey out Retry while a retry runs.
    pub fn set_retry_enabled(&self, enabled: bool) {
        self.retry.setEnabled(enabled);
    }

    fn sync_top(&self) {
        self.top
            .setHidden(self.banner_row.isHidden() && self.notice.isHidden());
    }

    /// Replace the content with `layout`. `action_target` receives the controls'
    /// actions.
    pub fn fill(
        &self,
        mtm: MainThreadMarker,
        layout: &Layout,
        action_target: &AnyObject,
    ) -> Rendered {
        for view in &self.content.arrangedSubviews() {
            self.content.removeArrangedSubview(&view);
            view.removeFromSuperview();
        }
        let mut rendered = Rendered {
            rows: Vec::new(),
            device_popup: None,
            status: None,
        };
        if let Some(header) = &layout.header {
            let row = header_row(mtm, header, action_target, &mut rendered);
            self.add_full_width(&row);
        }
        if let Some(note) = &layout.note {
            let label = label(mtm, note);
            label.setTextColor(Some(&NSColor::secondaryLabelColor()));
            self.add_full_width(&label);
        }
        for section in &layout.sections {
            let view = section_view(mtm, section, action_target, &mut rendered);
            self.add_full_width(&view);
        }
        rendered
    }

    fn add_full_width(&self, view: &NSView) {
        self.content.addArrangedSubview(view);
        pin_width(view, &self.content, -2.0 * MARGIN);
    }

    /// The height the content wants, for sizing the window on first show.
    pub fn content_height(&self) -> f64 {
        self.content.layoutSubtreeIfNeeded();
        self.content.fittingSize().height
    }

    /// The height of the part of the window that is not the scrolling page.
    pub fn chrome_height(&self) -> f64 {
        self.top.fittingSize().height * f64::from(u8::from(!self.top.isHidden()))
    }
}

impl Handle {
    /// Show `widget` in this control and set whether it is enabled.
    pub fn set(&self, widget: &Widget, enabled: bool) {
        match (self, widget) {
            (Self::Popup(popup), Widget::Popup { selected, .. }) => {
                popup.selectItemAtIndex(index(*selected));
                popup.setEnabled(enabled);
            }
            (Self::Switch(switch), Widget::Switch(on)) => {
                switch.setState(state(*on));
                switch.setEnabled(enabled);
            }
            (
                Self::Slider {
                    slider,
                    value,
                    unit,
                },
                Widget::Slider { value: current, .. },
            ) => {
                slider.setDoubleValue(f64::from(*current));
                slider.setEnabled(enabled);
                value.setStringValue(&NSString::from_str(&value_text(*current, unit)));
            }
            (Self::Text(label), Widget::Text(text)) => {
                label.setStringValue(&NSString::from_str(text));
            }
            _ => {}
        }
    }

    /// Whether the control shows something other than `widget` and `enabled`.
    pub fn differs(&self, widget: &Widget, enabled: bool) -> bool {
        match (self, widget) {
            (Self::Popup(popup), Widget::Popup { selected, .. }) => {
                popup.indexOfSelectedItem() != index(*selected) || popup.isEnabled() != enabled
            }
            (Self::Switch(switch), Widget::Switch(on)) => {
                switch.state() != state(*on) || switch.isEnabled() != enabled
            }
            (Self::Slider { slider, .. }, Widget::Slider { value, .. }) => {
                (slider.doubleValue() - f64::from(*value)).abs() >= 0.5
                    || slider.isEnabled() != enabled
            }
            (Self::Text(label), Widget::Text(text)) => label.stringValue().to_string() != *text,
            _ => true,
        }
    }

    /// The popup's selected index, if this is a popup.
    pub fn popup_index(&self) -> Option<usize> {
        match self {
            Self::Popup(popup) => usize::try_from(popup.indexOfSelectedItem()).ok(),
            _ => None,
        }
    }

    /// The switch state, if this is a switch.
    pub fn switch_on(&self) -> Option<bool> {
        match self {
            Self::Switch(switch) => Some(switch.state() == NSControlStateValueOn),
            _ => None,
        }
    }

    /// The slider value, if this is a slider.
    pub fn slider_value(&self) -> Option<f32> {
        match self {
            // The slider range is a DPI range, well inside `f32`'s exact
            // integers, and the control hands back a double.
            Self::Slider { slider, .. } => Some(narrow(slider.doubleValue())),
            _ => None,
        }
    }

    /// Show `text` as a slider's value label, while it is dragged.
    pub fn set_slider_label(&self, text: &str) {
        if let Self::Slider { value, .. } = self {
            value.setStringValue(&NSString::from_str(text));
        }
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "a DPI-range slider value, exactly representable in f32"
)]
const fn narrow(value: f64) -> f32 {
    value as f32
}

fn state(on: bool) -> isize {
    if on {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    }
}

fn index(position: usize) -> isize {
    isize::try_from(position).unwrap_or(0)
}

fn stack(
    mtm: MainThreadMarker,
    orientation: NSUserInterfaceLayoutOrientation,
    spacing: f64,
) -> Retained<NSStackView> {
    let stack = NSStackView::new(mtm);
    stack.setOrientation(orientation);
    stack.setSpacing(spacing);
    // Fill, so the view with the lowest hugging priority (a row's label)
    // takes the spare width and the controls sit at the trailing edge.
    stack.setDistribution(NSStackViewDistribution::Fill);
    stack
}

fn label(mtm: MainThreadMarker, text: &str) -> Retained<NSTextField> {
    NSTextField::labelWithString(&NSString::from_str(text), mtm)
}

fn activate(constraints: &[&NSLayoutConstraint]) {
    NSLayoutConstraint::activateConstraints(&NSArray::from_slice(constraints));
}

/// `child`'s width is `parent`'s plus `constant`.
fn pin_width(child: &NSView, parent: &NSView, constant: f64) {
    activate(&[&*child
        .widthAnchor()
        .constraintEqualToAnchor_constant(&parent.widthAnchor(), constant)]);
}

fn fix_width(view: &NSView, width: f64) {
    activate(&[&*view.widthAnchor().constraintEqualToConstant(width)]);
}

fn header_row(
    mtm: MainThreadMarker,
    header: &Header,
    action_target: &AnyObject,
    rendered: &mut Rendered,
) -> Retained<NSStackView> {
    let row = stack(mtm, NSUserInterfaceLayoutOrientation::Horizontal, 12.0);
    row.setAlignment(NSLayoutAttribute::CenterY);

    let title: Retained<NSView> = if header.names.len() > 1 {
        let popup = popup(
            mtm,
            &header.names,
            header.selected,
            action_target,
            DEVICE_TAG,
        );
        rendered.device_popup = Some(popup.clone());
        Retained::into_super(Retained::into_super(Retained::into_super(popup)))
    } else {
        let name = label(mtm, header.names.first().map_or("", String::as_str));
        name.setFont(Some(&NSFont::systemFontOfSize_weight(
            15.0,
            // SAFETY: AppKit exports the weight as an immutable constant.
            unsafe { NSFontWeightSemibold },
        )));
        Retained::into_super(Retained::into_super(name))
    };
    title.setContentHuggingPriority_forOrientation(
        NSLayoutPriorityDefaultLow,
        NSLayoutConstraintOrientation::Horizontal,
    );
    row.addArrangedSubview(&title);

    let status = label(mtm, &header.status);
    status.setTextColor(Some(&NSColor::secondaryLabelColor()));
    status.setAlignment(NSTextAlignment::Right);
    row.addArrangedSubview(&status);
    rendered.status = Some(status);
    row
}

fn section_view(
    mtm: MainThreadMarker,
    section: &SectionSpec,
    action_target: &AnyObject,
    rendered: &mut Rendered,
) -> Retained<NSStackView> {
    let view = stack(mtm, NSUserInterfaceLayoutOrientation::Vertical, 10.0);
    view.setAlignment(NSLayoutAttribute::Leading);

    let title = label(mtm, section.title);
    title.setFont(Some(&NSFont::systemFontOfSize_weight(
        11.0,
        // SAFETY: AppKit exports the weight as an immutable constant.
        unsafe { NSFontWeightSemibold },
    )));
    title.setTextColor(Some(&NSColor::secondaryLabelColor()));
    view.addArrangedSubview(&title);
    view.setCustomSpacing_afterView(6.0, &title);

    for spec in &section.rows {
        let tag = index(rendered.rows.len());
        let (row, handle) = row_view(mtm, spec, action_target, tag);
        view.addArrangedSubview(&row);
        pin_width(&row, &view, 0.0);
        rendered.rows.push(BuiltRow {
            target: spec.target,
            handle,
        });
    }
    view
}

fn row_view(
    mtm: MainThreadMarker,
    spec: &RowSpec,
    action_target: &AnyObject,
    tag: isize,
) -> (Retained<NSStackView>, Handle) {
    let row = stack(mtm, NSUserInterfaceLayoutOrientation::Horizontal, 12.0);
    row.setAlignment(NSLayoutAttribute::CenterY);

    let name = label(mtm, &spec.label);
    name.setContentHuggingPriority_forOrientation(
        NSLayoutPriorityDefaultLow,
        NSLayoutConstraintOrientation::Horizontal,
    );
    row.addArrangedSubview(&name);

    let handle = match &spec.widget {
        Widget::Popup { items, selected } => {
            let popup = popup(mtm, items, *selected, action_target, tag);
            popup.setEnabled(spec.enabled);
            row.addArrangedSubview(&popup);
            Handle::Popup(popup)
        }
        Widget::Switch(on) => {
            let switch = NSSwitch::new(mtm);
            switch.setState(state(*on));
            switch.setEnabled(spec.enabled);
            wire(&switch, action_target, tag);
            row.addArrangedSubview(&switch);
            Handle::Switch(switch)
        }
        Widget::Slider {
            min,
            max,
            value,
            unit,
            ..
        } => {
            let slider = NSSlider::new(mtm);
            slider.setMinValue(f64::from(*min));
            slider.setMaxValue(f64::from(*max));
            slider.setDoubleValue(f64::from(*value));
            slider.setEnabled(spec.enabled);
            wire(&slider, action_target, tag);

            let text = label(mtm, &value_text(*value, unit));
            text.setFont(Some(&NSFont::monospacedDigitSystemFontOfSize_weight(
                13.0,
                // SAFETY: AppKit exports the weight as an immutable constant.
                unsafe { NSFontWeightRegular },
            )));
            text.setAlignment(NSTextAlignment::Right);
            fix_width(&text, VALUE_WIDTH);

            let pair = stack(mtm, NSUserInterfaceLayoutOrientation::Horizontal, 8.0);
            pair.setAlignment(NSLayoutAttribute::CenterY);
            pair.addArrangedSubview(&slider);
            pair.addArrangedSubview(&text);
            fix_width(&pair, CONTROL_WIDTH);
            row.addArrangedSubview(&pair);
            Handle::Slider {
                slider,
                value: text,
                unit,
            }
        }
        Widget::Text(text) => {
            let text = label(mtm, text);
            text.setTextColor(Some(&NSColor::secondaryLabelColor()));
            text.setAlignment(NSTextAlignment::Right);
            row.addArrangedSubview(&text);
            Handle::Text(text)
        }
    };
    (row, handle)
}

fn popup(
    mtm: MainThreadMarker,
    items: &[String],
    selected: usize,
    action_target: &AnyObject,
    tag: isize,
) -> Retained<NSPopUpButton> {
    let popup = NSPopUpButton::initWithFrame_pullsDown(mtm.alloc(), NSRect::ZERO, false);
    // `addItemWithTitle:` replaces an item that has the same title, which
    // would shift every index after it, so repeated titles are made distinct.
    let mut seen: Vec<String> = Vec::with_capacity(items.len());
    for item in items {
        let mut title = item.clone();
        let mut copy = 1;
        while seen.contains(&title) {
            copy += 1;
            title = format!("{item} ({copy})");
        }
        popup.addItemWithTitle(&NSString::from_str(&title));
        seen.push(title);
    }
    popup.selectItemAtIndex(index(selected));
    wire(&popup, action_target, tag);
    fix_width(&popup, CONTROL_WIDTH);
    popup
}

fn wire(control: &NSControl, action_target: &AnyObject, tag: isize) {
    control.setTag(tag);
    // SAFETY: `action_target` is a live object implementing `controlChanged:`;
    // the control holds it weakly and the caller keeps it alive for the
    // window's lifetime.
    unsafe {
        control.setTarget(Some(action_target));
        control.setAction(Some(changed()));
    }
}
