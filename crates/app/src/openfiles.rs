//! Opening files from the Finder, the Dock and `open -a`.
//!
//! A bundled macOS app is not handed its documents on the command line.
//! LaunchServices sends them to the application delegate, and an app
//! that declares no document types — or declares them and then ignores
//! the event — is refused with "can't open files of this type".
//!
//! The awkward part is that winit registers an application delegate of
//! its own and asserts that the one installed is that exact class, so
//! putting ours in its place replaces winit's and panics the first time
//! it looks. Its documentation says it guarantees not to register one;
//! that guarantee is from a different version than the one here.
//!
//! So the method is added to winit's own delegate class instead, at
//! runtime, before the loop starts. Objective-C allows a class to gain
//! a method at any point and existing instances pick it up, winit's
//! `is_kind_of` check still passes because the class is unchanged, and
//! AppKit asks `respondsToSelector:` only when it has a document to
//! deliver — by which time the method is there.
//!
//! The paths are buffered rather than acted on: they can arrive before
//! the event loop is running, and they arrive on AppKit's thread.

use std::path::PathBuf;
use std::sync::Mutex;

/// Paths LaunchServices has handed us and the editor has not yet opened.
static PENDING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Everything LaunchServices has asked for since the last call.
pub fn take() -> Vec<PathBuf> {
    PENDING
        .lock()
        .map(|mut queue| std::mem::take(&mut *queue))
        .unwrap_or_default()
}

#[cfg(not(target_os = "macos"))]
pub fn install<T: Default + Send + 'static>(_proxy: winit::event_loop::EventLoopProxy<T>) {}

#[cfg(target_os = "macos")]
pub use mac::install;

#[cfg(target_os = "macos")]
mod mac {
    use super::PENDING;
    use std::path::PathBuf;
    use std::sync::OnceLock;

    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, Sel};
    use objc2::{msg_send, sel};
    use objc2_app_kit::NSApplication;
    use objc2_foundation::MainThreadMarker;

    /// Wakes the event loop once paths have been queued.
    ///
    /// A closure, so this module needs to know nothing about the
    /// application's own user-event type.
    static WAKE: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

    /// `application:openURLs:`, as AppKit will call it.
    ///
    /// Takes the array as an opaque object and reads it by message, so
    /// no generic Objective-C type has to line up across crate
    /// versions — only the selectors, which have been stable for years.
    extern "C" fn open_urls(_this: &AnyObject, _cmd: Sel, _app: &AnyObject, urls: *mut AnyObject) {
        if urls.is_null() {
            return;
        }
        let count: usize = unsafe { msg_send![urls, count] };
        let mut queued = false;
        for i in 0..count {
            let url: *mut AnyObject = unsafe { msg_send![urls, objectAtIndex: i] };
            if url.is_null() {
                continue;
            }
            // `path` is nil for anything that is not a file URL, which
            // is the filter we want: a custom scheme would be a
            // different feature and a different promise.
            let path: *mut AnyObject = unsafe { msg_send![url, path] };
            if path.is_null() {
                continue;
            }
            let utf8: *const std::ffi::c_char = unsafe { msg_send![path, UTF8String] };
            if utf8.is_null() {
                continue;
            }
            let text = unsafe { std::ffi::CStr::from_ptr(utf8) }.to_string_lossy();
            if let Ok(mut pending) = PENDING.lock() {
                pending.push(PathBuf::from(text.as_ref()));
                queued = true;
            }
        }
        if queued {
            if let Some(wake) = WAKE.get() {
                wake();
            }
        }
    }

    /// Teaches winit's application delegate to accept documents.
    ///
    /// Call after the event loop is built — the delegate does not exist
    /// before that — and before it runs, so the method is in place by
    /// the time AppKit has a document to deliver.
    pub fn install<T: Default + Send + 'static>(proxy: winit::event_loop::EventLoopProxy<T>) {
        let Some(mtm) = MainThreadMarker::new() else {
            log::warn!("not on the main thread; files opened from Finder will be ignored");
            return;
        };
        if WAKE
            .set(Box::new(move || {
                let _ = proxy.send_event(T::default());
            }))
            .is_err()
        {
            return;
        }

        let app = NSApplication::sharedApplication(mtm);
        let Some(delegate) = (unsafe { app.delegate() }) else {
            log::warn!("no application delegate; files opened from Finder will be ignored");
            return;
        };
        // Through the runtime, because the delegate is handed back as
        // a protocol object that does not expose its own class.
        let object: &AnyObject = unsafe { &*(Retained::as_ptr(&delegate) as *const AnyObject) };
        let class: &AnyClass = object.class();
        if class.instance_method(sel!(application:openURLs:)).is_some() {
            // A later winit grew the method itself; leave it alone.
            return;
        }

        // SAFETY: the signature matches `v@:@@` — no return, self, the
        // selector, the application and the array — which is what
        // AppKit calls this selector with.
        let added = unsafe {
            objc2::ffi::class_addMethod(
                (class as *const AnyClass).cast_mut().cast(),
                sel!(application:openURLs:).as_ptr(),
                Some(std::mem::transmute::<
                    extern "C" fn(&AnyObject, Sel, &AnyObject, *mut AnyObject),
                    unsafe extern "C" fn(),
                >(open_urls)),
                c"v@:@@".as_ptr(),
            )
        };
        if !added {
            log::warn!("could not accept documents from Finder");
        }
    }
}
