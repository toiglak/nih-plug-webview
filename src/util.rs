use std::{
    num::{NonZero, NonZeroIsize},
    ptr::NonNull,
};

use base64::{prelude::BASE64_STANDARD as BASE64, Engine};
use nih_plug::editor::ParentWindowHandle;
use raw_window_handle::{
    AppKitWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle, XcbWindowHandle,
};
use wry::WebView;

const PLUGIN_OBJ: &str = "window.__NIH_PLUG_WEBVIEW__";

pub fn send_message(webview: &WebView, message: String) {
    let text = message.replace("`", r#"\`"#);
    let script = format!("{PLUGIN_OBJ}.onmessage(`text`,`{}`);", text);
    webview.evaluate_script(&script).ok();
}

pub fn into_window_handle<'a>(handle: ParentWindowHandle) -> WindowHandle<'a> {
    let raw = match handle {
        ParentWindowHandle::AppKitNsView(h) => {
            RawWindowHandle::AppKit(AppKitWindowHandle::new(NonNull::new(h).unwrap()))
        }
        ParentWindowHandle::Win32Hwnd(h) => RawWindowHandle::Win32(Win32WindowHandle::new(
            NonZeroIsize::new(h as isize).unwrap(),
        )),
        ParentWindowHandle::X11Window(h) => {
            RawWindowHandle::Xcb(XcbWindowHandle::new(NonZero::new(h).unwrap()))
        }
    };

    unsafe { WindowHandle::borrow_raw(raw) }
}
