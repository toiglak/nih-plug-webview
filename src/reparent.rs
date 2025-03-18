use nih_plug::editor::ParentWindowHandle;
use std::rc::Rc;
use wry::WebView;

#[cfg(target_os = "macos")]
pub fn reparent_webview(webview: &Rc<WebView>, window: ParentWindowHandle) -> Option<()> {
    unsafe {
        use objc2_app_kit::NSView;
        use wry::WebViewExtMacOS;

        // Obtain `ns_window` from `ParentWindowHandle`
        let ns_view = match window {
            ParentWindowHandle::AppKitNsView(ns_view) => ns_view.cast::<NSView>(),
            _ => unreachable!(),
        };
        let ns_view = ns_view.as_ref().unwrap();
        let ns_window = ns_view.window().unwrap();
        let ns_window_ptr = objc2::rc::Retained::into_raw(ns_window);

        // Reparent and focus the window
        webview.reparent(ns_window_ptr).unwrap();
        // NOTE: This breaks Shift + W — we need to press Shift + W twice.
        webview.activate().unwrap();
        // Make first responder.
        webview.focus().unwrap();

        Some(())
    }
}

#[cfg(target_os = "windows")]
pub fn reparent_webview(webview: &Rc<WebView>, handle: ParentWindowHandle) -> Option<()> {
    use wry::WebViewExtWindows;

    let hwnd = match handle {
        ParentWindowHandle::Win32Hwnd(hwnd) => hwnd,
        _ => unreachable!(),
    };

    // TODO: Handle reparenting gracefully.
    webview.reparent(hwnd as isize).unwrap();

    Some(())
}

#[cfg(target_os = "linux")]
pub fn reparent_webview(_webview: Rc<WebView>, _window: WindowHandle) -> Option<()> {
    None
}

pub struct TempWindow {
    #[cfg(target_os = "windows")]
    pub hwnd: windows::Win32::Foundation::HWND,
}

impl TempWindow {
    pub fn new() -> Self {
        #[cfg(target_os = "windows")]
        unsafe {
            use windows::{
                core::PCWSTR,
                Win32::UI::WindowsAndMessaging::{
                    CreateWindowExW, WS_DISABLED, WS_EX_TOOLWINDOW, WS_OVERLAPPED,
                },
            };

            // Create an invisible window to host the webview
            let class_name = windows::core::w!("STATIC");
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW, // Extended style (tool window has no taskbar presence)
                PCWSTR(class_name.as_ptr()),
                PCWSTR::null(),              // Window title
                WS_OVERLAPPED | WS_DISABLED, // Window style (disabled and not visible)
                0,
                0,
                0,
                0,
                None,
                None,
                None,
                None,
            )
            .unwrap();

            return TempWindow { hwnd };
        }
        #[cfg(target_os = "macos")]
        return TempWindow {};
        #[cfg(target_os = "linux")]
        return TempWindow {};
    }

    /// Reparent the webview INTO a temporary invisible window.
    pub fn reparent_from(&mut self, webview: &Rc<WebView>) {
        #[cfg(target_os = "windows")]
        {
            use wry::WebViewExtWindows;
            let temp_window = self.hwnd;
            webview.reparent(temp_window.0 as isize).unwrap();
        }
        #[cfg(target_os = "macos")]
        let _ = webview;
        #[cfg(target_os = "linux")]
        let _ = webview;
    }
}

impl Drop for TempWindow {
    fn drop(&mut self) {
        #[cfg(target_os = "windows")]
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::DestroyWindow;
            DestroyWindow(self.hwnd).unwrap();
        }
        #[cfg(target_os = "macos")]
        {}
        #[cfg(target_os = "linux")]
        {}
    }
}
