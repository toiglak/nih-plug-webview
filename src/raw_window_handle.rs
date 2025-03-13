use std::{
    num::{NonZero, NonZeroIsize},
    ptr::NonNull,
};

use raw_window_handle::{
    AndroidNdkWindowHandle, AppKitWindowHandle, RawWindowHandle, UiKitWindowHandle,
    WaylandWindowHandle, Win32WindowHandle, WinRtWindowHandle, WindowHandle, XcbWindowHandle,
};
use raw_window_handle_0_5::{HasRawWindowHandle, RawWindowHandle as OldRawWindowHandle};

// FIXME: This is unrelated to the raw window handle, still, the reason we cannot upgrade
// above wry-0.45.0 is because wry made the wrong assumption in two places in more recent
// versions, where they call `ns_view.window().unwrap()`. This is wrong because the
// `window` method returns null (correctly). I also tried compiling for Windows and the
// upgraded wry worked just fine.

pub fn from_raw_window_handle_0_5_2(old_handle: &impl HasRawWindowHandle) -> WindowHandle<'static> {
    let old_handle = old_handle.raw_window_handle();

    let raw = match old_handle {
        OldRawWindowHandle::AppKit(ref h) => {
            RawWindowHandle::AppKit(AppKitWindowHandle::new(NonNull::new(h.ns_view).unwrap()))
        }
        OldRawWindowHandle::Win32(ref _h) => RawWindowHandle::Win32(Win32WindowHandle::new(
            NonZeroIsize::new(_h.hwnd as isize).unwrap(),
        )),
        OldRawWindowHandle::Xlib(ref h) => {
            RawWindowHandle::Xlib(raw_window_handle::XlibWindowHandle::new(h.window))
        }
        OldRawWindowHandle::Xcb(ref h) => {
            RawWindowHandle::Xcb(XcbWindowHandle::new(NonZero::new(h.window).unwrap()))
        }
        OldRawWindowHandle::Wayland(ref h) => {
            RawWindowHandle::Wayland(WaylandWindowHandle::new(NonNull::new(h.surface).unwrap()))
        }
        _ => unimplemented!("Unsupported window handle type"),
    };

    // SAFETY: Upheld by the implementers of `HasRawWindowHandle`.
    unsafe { WindowHandle::borrow_raw(raw) }
}
