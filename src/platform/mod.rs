//! macOS integration.
//!
//! * window: level above normal windows, all Spaces + over full-screen apps,
//!   transparent background, native background blur (the same private
//!   CoreGraphics call iTerm2/Alacritty use).
//! * screens: which screen the mouse is on, its menu-bar-free area.
//! * tray: menu bar status item.
//! * notify: Notification Center banners.

pub mod notify;
pub mod present;
pub mod tray;

use objc2::encode::{Encode, Encoding};
use objc2::runtime::{AnyObject, Bool};
use objc2::{class, msg_send};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CGPoint {
    pub x: f64,
    pub y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CGSize {
    pub width: f64,
    pub height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CGRect {
    pub origin: CGPoint,
    pub size: CGSize,
}

unsafe impl Encode for CGPoint {
    const ENCODING: Encoding = Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
}
unsafe impl Encode for CGSize {
    const ENCODING: Encoding = Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
}
unsafe impl Encode for CGRect {
    const ENCODING: Encoding = Encoding::Struct("CGRect", &[CGPoint::ENCODING, CGSize::ENCODING]);
}

impl CGRect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { origin: CGPoint { x, y }, size: CGSize { width: w, height: h } }
    }
    pub fn contains(&self, p: CGPoint) -> bool {
        p.x >= self.origin.x
            && p.x < self.origin.x + self.size.width
            && p.y >= self.origin.y
            && p.y < self.origin.y + self.size.height
    }
    pub fn max_y(&self) -> f64 {
        self.origin.y + self.size.height
    }
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGSMainConnectionID() -> i32;
    fn CGSSetWindowBackgroundBlurRadius(cid: i32, wid: u32, radius: i32) -> i32;
}

const NS_STATUS_WINDOW_LEVEL: isize = 25;
const BEHAVIOR_CAN_JOIN_ALL_SPACES: usize = 1 << 0;
const BEHAVIOR_STATIONARY: usize = 1 << 4;
const BEHAVIOR_IGNORES_CYCLE: usize = 1 << 6;
const BEHAVIOR_FULLSCREEN_AUXILIARY: usize = 1 << 8;

fn ns_window(w: &Window) -> Option<*mut AnyObject> {
    let handle = w.window_handle().ok()?;
    if let RawWindowHandle::AppKit(h) = handle.as_raw() {
        let view = h.ns_view.as_ptr() as *mut AnyObject;
        let win: *mut AnyObject = unsafe { msg_send![view, window] };
        if !win.is_null() {
            return Some(win);
        }
    }
    None
}

fn ns_app() -> *mut AnyObject {
    unsafe { msg_send![class!(NSApplication), sharedApplication] }
}

/// One-time window setup: floating level, every Space, over full-screen apps, clear background.
pub fn setup_window(w: &Window) {
    setup_window_level(w, NS_STATUS_WINDOW_LEVEL, false);
}

/// Floating window on every Space with a clear background, at `level`.
pub fn setup_window_level(w: &Window, level: isize, movable: bool) {
    let Some(win) = ns_window(w) else { return };
    unsafe {
        let _: () = msg_send![win, setLevel: level];
        let behavior = BEHAVIOR_CAN_JOIN_ALL_SPACES
            | BEHAVIOR_STATIONARY
            | BEHAVIOR_IGNORES_CYCLE
            | BEHAVIOR_FULLSCREEN_AUXILIARY;
        let _: () = msg_send![win, setCollectionBehavior: behavior];
        let _: () = msg_send![win, setHasShadow: Bool::NO];
        let _: () = msg_send![win, setOpaque: Bool::NO];
        let clear: *mut AnyObject = msg_send![class!(NSColor), clearColor];
        let _: () = msg_send![win, setBackgroundColor: clear];
        // NSWindowAnimationBehaviorNone — we animate ourselves.
        let _: () = msg_send![win, setAnimationBehavior: 2isize];
        let _: () = msg_send![win, setHidesOnDeactivate: Bool::NO];
        let _: () = msg_send![win, setMovable: if movable { Bool::YES } else { Bool::NO }];
    }
}

pub fn set_blur(w: &Window, radius: u32) {
    let Some(win) = ns_window(w) else { return };
    unsafe {
        let num: isize = msg_send![win, windowNumber];
        CGSSetWindowBackgroundBlurRadius(CGSMainConnectionID(), num as u32, radius as i32);
    }
}

/// Set the window frame in Cocoa global coordinates (origin bottom-left).
pub fn set_frame(w: &Window, r: CGRect) {
    let Some(win) = ns_window(w) else { return };
    unsafe {
        let _: () = msg_send![win, setFrame: r, display: Bool::YES];
    }
}

pub fn show(w: &Window) {
    let Some(win) = ns_window(w) else { return };
    unsafe {
        let app = ns_app();
        // macOS 14+: cooperative activation (`activate`); older: ignoringOtherApps.
        let modern: Bool = msg_send![app, respondsToSelector: objc2::sel!(activate)];
        if modern.as_bool() {
            let _: () = msg_send![app, activate];
        }
        let _: () = msg_send![app, activateIgnoringOtherApps: Bool::YES];
        let me: *mut AnyObject = msg_send![class!(NSRunningApplication), currentApplication];
        if !me.is_null() {
            // NSApplicationActivateAllWindows
            let _: Bool = msg_send![me, activateWithOptions: 1usize];
        }
        let _: () = msg_send![win, makeKeyAndOrderFront: std::ptr::null_mut::<AnyObject>()];
        let _: () = msg_send![win, makeKeyWindow];
        // Make sure the content view (winit's) receives key events.
        let view: *mut AnyObject = msg_send![win, contentView];
        if !view.is_null() {
            let _: Bool = msg_send![win, makeFirstResponder: view];
        }
    }
}

pub fn order_out(w: &Window) {
    let Some(win) = ns_window(w) else { return };
    unsafe {
        let _: () = msg_send![win, orderOut: std::ptr::null_mut::<AnyObject>()];
    }
}

/// Give focus back to the previously active app.
pub fn hide_app() {
    unsafe {
        let _: () = msg_send![ns_app(), hide: std::ptr::null_mut::<AnyObject>()];
    }
}

pub fn app_is_active() -> bool {
    unsafe {
        let b: Bool = msg_send![ns_app(), isActive];
        b.as_bool()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ScreenInfo {
    pub frame: CGRect,
    pub visible: CGRect,
    pub scale: f64,
    /// Height of the menu bar area on this screen (0 if hidden / full-screen).
    pub menubar: f64,
}

fn screen_info(s: *mut AnyObject) -> ScreenInfo {
    unsafe {
        let frame: CGRect = msg_send![s, frame];
        let visible: CGRect = msg_send![s, visibleFrame];
        let scale: f64 = msg_send![s, backingScaleFactor];
        ScreenInfo { frame, visible, scale, menubar: (frame.max_y() - visible.max_y()).max(0.0) }
    }
}

/// The screen under the mouse (or the primary screen).
pub fn target_screen(follow_mouse: bool) -> Option<ScreenInfo> {
    unsafe {
        let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
        if screens.is_null() {
            return None;
        }
        let count: usize = msg_send![screens, count];
        if count == 0 {
            return None;
        }
        if follow_mouse {
            let mouse: CGPoint = msg_send![class!(NSEvent), mouseLocation];
            for i in 0..count {
                let s: *mut AnyObject = msg_send![screens, objectAtIndex: i];
                let info = screen_info(s);
                if info.frame.contains(mouse) {
                    return Some(info);
                }
            }
        }
        let s: *mut AnyObject = msg_send![screens, objectAtIndex: 0usize];
        Some(screen_info(s))
    }
}

/// CFBundleIdentifier when running from a .app bundle.
pub fn bundle_id() -> Option<String> {
    unsafe {
        let bundle: *mut AnyObject = msg_send![class!(NSBundle), mainBundle];
        if bundle.is_null() {
            return None;
        }
        let id: *mut AnyObject = msg_send![bundle, bundleIdentifier];
        if id.is_null() {
            return None;
        }
        let utf8: *const std::ffi::c_char = msg_send![id, UTF8String];
        if utf8.is_null() {
            return None;
        }
        Some(std::ffi::CStr::from_ptr(utf8).to_string_lossy().into_owned())
    }
}
