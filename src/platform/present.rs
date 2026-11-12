//! Puts CPU-rendered frames on screen (macOS only).
//!
//! A `CALayer` is added on top of the window's content view; each frame becomes a
//! `CGImage` set as the layer's contents. Compared to a generic framebuffer crate
//! this does three things fluteterm needs:
//!
//! * **alpha**: frames are premultiplied ARGB, so the panel can be see-through;
//! * **screen colour space**: images are tagged with the main screen's colour space,
//!   so Core Animation composites them without a per-frame colour conversion;
//! * **buffer recycling**: Core Animation hands frame buffers back when it's done
//!   with them; we reuse them instead of allocating ~13 MB per frame.

use std::ffi::c_void;
use std::sync::{Arc, Mutex};

use objc2::encode::{Encoding, RefEncode};
use objc2::runtime::{AnyObject, Bool};
use objc2::{class, msg_send};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

use super::{CGPoint, CGRect};

#[repr(C)]
struct CGColorSpace {
    _p: [u8; 0],
}
unsafe impl RefEncode for CGColorSpace {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("CGColorSpace", &[]));
}

type DataRelease = unsafe extern "C" fn(info: *mut c_void, data: *const c_void, size: usize);

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGColorSpaceCreateDeviceRGB() -> *mut CGColorSpace;
    fn CGColorSpaceRetain(cs: *mut CGColorSpace) -> *mut CGColorSpace;
    fn CGColorSpaceRelease(cs: *mut CGColorSpace);
    fn CGDataProviderCreateWithData(info: *mut c_void, data: *const c_void, size: usize, release: Option<DataRelease>) -> *mut c_void;
    fn CGDataProviderRelease(p: *mut c_void);
    #[allow(clippy::too_many_arguments)]
    fn CGImageCreate(
        width: usize,
        height: usize,
        bits_per_component: usize,
        bits_per_pixel: usize,
        bytes_per_row: usize,
        space: *mut CGColorSpace,
        bitmap_info: u32,
        provider: *mut c_void,
        decode: *const f64,
        should_interpolate: bool,
        intent: i32,
    ) -> *mut c_void;
    fn CGImageRelease(image: *mut c_void);
}

/// kCGImageAlphaPremultipliedFirst | kCGBitmapByteOrder32Little: native little-endian 0xAARRGGBB.
const BITMAP_INFO: u32 = 2 | (2 << 12);
const POOL_MAX: usize = 3;

type Pool = Arc<Mutex<Vec<Vec<u32>>>>;

/// Owned by Core Animation while it shows the frame; returned to the pool on release.
struct Frame {
    pixels: Vec<u32>,
    pool: Pool,
}

unsafe extern "C" fn release_frame(info: *mut c_void, _data: *const c_void, _size: usize) {
    let frame = unsafe { Box::from_raw(info as *mut Frame) };
    if let Ok(mut pool) = frame.pool.lock() {
        if pool.len() < POOL_MAX {
            pool.push(frame.pixels);
        }
    }
}

pub struct Presenter {
    root: *mut AnyObject,
    layer: *mut AnyObject,
    color_space: *mut CGColorSpace,
    pool: Pool,
    current: Vec<u32>,
    width: usize,
    height: usize,
}

impl Presenter {
    pub fn new(window: &Window) -> Option<Self> {
        let handle = window.window_handle().ok()?;
        let RawWindowHandle::AppKit(h) = handle.as_raw() else { return None };
        let view = h.ns_view.as_ptr() as *mut AnyObject;
        unsafe {
            let _: () = msg_send![view, setWantsLayer: Bool::YES];
            let root: *mut AnyObject = msg_send![view, layer];
            if root.is_null() {
                return None;
            }
            // Keep the view's layer alive for as long as we draw into it.
            let root: *mut AnyObject = msg_send![root, retain];
            // Our own sublayer: setting contents on a view-managed layer is fragile.
            let layer: *mut AnyObject = msg_send![class!(CALayer), new];
            let _: () = msg_send![root, addSublayer: layer];
            // Top-left origin, content pinned top-left (frames are drawn top-down).
            let _: () = msg_send![layer, setAnchorPoint: CGPoint { x: 0.0, y: 0.0 }];
            let _: () = msg_send![layer, setGeometryFlipped: Bool::YES];
            let gravity: *mut AnyObject = msg_send![class!(NSString), stringWithUTF8String: c"topLeft".as_ptr()];
            let _: () = msg_send![layer, setContentsGravity: gravity];

            // Tag frames with the screen's colour space (no per-frame conversion).
            let screen: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
            let ns_space: *mut AnyObject = if screen.is_null() { std::ptr::null_mut() } else { msg_send![screen, colorSpace] };
            let cg_space: *mut CGColorSpace = if ns_space.is_null() { std::ptr::null_mut() } else { msg_send![ns_space, CGColorSpace] };
            let color_space = if cg_space.is_null() { CGColorSpaceCreateDeviceRGB() } else { CGColorSpaceRetain(cg_space) };

            Some(Self { root, layer, color_space, pool: Arc::new(Mutex::new(Vec::new())), current: Vec::new(), width: 0, height: 0 })
        }
    }

    /// A buffer for the next frame (`width * height` pixels, premultiplied 0xAARRGGBB).
    /// Contents are undefined: callers clear it.
    pub fn frame(&mut self, width: usize, height: usize) -> &mut [u32] {
        let len = width * height;
        if self.current.len() != len {
            let reused = self.pool.lock().ok().and_then(|mut p| {
                p.retain(|b| b.len() == len);
                p.pop()
            });
            self.current = reused.unwrap_or_else(|| vec![0; len]);
        }
        self.width = width;
        self.height = height;
        &mut self.current
    }

    /// Hand the frame to Core Animation.
    pub fn present(&mut self) {
        let (w, h) = (self.width, self.height);
        if w == 0 || h == 0 || self.current.len() != w * h {
            return;
        }
        let pixels = std::mem::take(&mut self.current);
        let data = pixels.as_ptr() as *const c_void;
        let size = pixels.len() * 4;
        let info = Box::into_raw(Box::new(Frame { pixels, pool: self.pool.clone() })) as *mut c_void;
        unsafe {
            let provider = CGDataProviderCreateWithData(info, data, size, Some(release_frame));
            if provider.is_null() {
                // Only on allocation failure. Apple doesn't document whether the release
                // callback may already have run, so leak the frame rather than risk a double free.
                return;
            }
            let image = CGImageCreate(w, h, 8, 32, w * 4, self.color_space, BITMAP_INFO, provider, std::ptr::null(), false, 0);
            CGDataProviderRelease(provider);
            if image.is_null() {
                return;
            }
            // Without a transaction, every contents change fades in over 0.25 s.
            let _: () = msg_send![class!(CATransaction), begin];
            let _: () = msg_send![class!(CATransaction), setDisableActions: Bool::YES];
            let bounds: CGRect = msg_send![self.root, bounds];
            let scale: f64 = msg_send![self.root, contentsScale];
            let _: () = msg_send![self.layer, setFrame: bounds];
            let _: () = msg_send![self.layer, setContentsScale: scale];
            let _: () = msg_send![self.layer, setContents: image as *mut AnyObject];
            let _: () = msg_send![class!(CATransaction), commit];
            CGImageRelease(image);
        }
        // Prefetch the next buffer from the pool on the next `frame` call.
        self.current = Vec::new();
    }

    /// Give the recycled frame buffers back to the system (e.g. while hidden).
    pub fn trim(&mut self) {
        if let Ok(mut pool) = self.pool.lock() {
            pool.clear();
        }
        self.current = Vec::new();
    }

    /// (frame, contents set?, contentsScale, geometryFlipped): for self-checks.
    pub fn layer_state(&self) -> (CGRect, bool, f64, bool) {
        unsafe {
            let frame: CGRect = msg_send![self.layer, frame];
            let contents: *mut AnyObject = msg_send![self.layer, contents];
            let scale: f64 = msg_send![self.layer, contentsScale];
            let flipped: Bool = msg_send![self.layer, isGeometryFlipped];
            (frame, !contents.is_null(), scale, flipped.as_bool())
        }
    }
}

impl Drop for Presenter {
    fn drop(&mut self) {
        unsafe {
            let _: () = msg_send![self.layer, removeFromSuperlayer];
            let _: () = msg_send![self.layer, release];
            let _: () = msg_send![self.root, release];
            CGColorSpaceRelease(self.color_space);
        }
    }
}
