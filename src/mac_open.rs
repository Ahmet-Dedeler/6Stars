//! Files opened from Finder on macOS: double-click, "Open With", or a drop on the Dock icon.
//!
//! Finder does not pass these as command-line arguments. AppKit delivers an Apple Event and
//! calls `application:openURLs:` on the app delegate. winit's delegate does not implement that
//! method, so the event was silently dropped. `install()` adds the method to `NSObject` before
//! the event loop starts, so winit's delegate (an `NSObject` subclass) inherits it. The handler
//! only queues paths; the UI drains the queue with `take()` each frame.

use std::ffi::{c_char, c_void, CStr};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use eframe::egui;

static QUEUE: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static CTX: OnceLock<egui::Context> = OnceLock::new();

type Id = *mut c_void;
type Sel = *const c_void;

#[link(name = "objc")]
extern "C" {
    fn objc_getClass(name: *const c_char) -> Id;
    fn sel_registerName(name: *const c_char) -> Sel;
    fn class_addMethod(cls: Id, name: Sel, imp: *const c_void, types: *const c_char) -> bool;
    fn objc_msgSend();
}

unsafe fn sel(name: &CStr) -> Sel {
    sel_registerName(name.as_ptr())
}

/// `- (void)application:(NSApplication *)app openURLs:(NSArray<NSURL *> *)urls`
extern "C" fn open_urls(_this: Id, _cmd: Sel, _app: Id, urls: Id) {
    if urls.is_null() {
        return;
    }
    unsafe {
        let msg = objc_msgSend as *const c_void;
        let count: extern "C" fn(Id, Sel) -> usize = std::mem::transmute(msg);
        let at: extern "C" fn(Id, Sel, usize) -> Id = std::mem::transmute(msg);
        let obj: extern "C" fn(Id, Sel) -> Id = std::mem::transmute(msg);
        let utf8: extern "C" fn(Id, Sel) -> *const c_char = std::mem::transmute(msg);

        let mut paths = Vec::new();
        for i in 0..count(urls, sel(c"count")) {
            let url = at(urls, sel(c"objectAtIndex:"), i);
            if url.is_null() {
                continue;
            }
            let path = obj(url, sel(c"path"));
            if path.is_null() {
                continue;
            }
            let s = utf8(path, sel(c"UTF8String"));
            if !s.is_null() {
                paths.push(PathBuf::from(CStr::from_ptr(s).to_string_lossy().into_owned()));
            }
        }
        QUEUE.lock().unwrap().extend(paths);
    }
    if let Some(ctx) = CTX.get() {
        ctx.request_repaint();
    }
}

/// Call once on the main thread, before `eframe::run_native`.
pub fn install() {
    unsafe {
        let cls = objc_getClass(c"NSObject".as_ptr());
        if cls.is_null() {
            return;
        }
        // Returns false if the method already exists; nothing else to do then.
        class_addMethod(cls, sel(c"application:openURLs:"), open_urls as *const c_void, c"v@:@@".as_ptr());
    }
}

/// Lets the handler wake the UI when files arrive while the app is idle.
pub fn set_context(ctx: &egui::Context) {
    let _ = CTX.set(ctx.clone());
}

/// Paths Finder asked us to open since the last call.
pub fn take() -> Vec<PathBuf> {
    std::mem::take(&mut *QUEUE.lock().unwrap())
}
