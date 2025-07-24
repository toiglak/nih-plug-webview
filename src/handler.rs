use std::{cell::Cell, rc::Rc, sync::Arc};

use nih_plug::prelude::{GuiContext, ParamSetter};
use wry::{
    dpi::{LogicalPosition, LogicalSize, Position, Size},
    Rect, WebView,
};

use crate::WebViewState;

pub trait EditorHandler: Send + 'static {
    fn init(&mut self, cx: &mut Context);
    fn on_frame(&mut self, cx: &mut Context);
    fn on_message(&mut self, send_message: &dyn Fn(String), message: String);
}

pub struct Context {
    pub(crate) state: Arc<WebViewState>,
    pub(crate) webview: Rc<WebView>,
    pub(crate) context: Arc<dyn GuiContext>,
    pub(crate) params_changed: Rc<Cell<bool>>,
}

impl Context {
    /// Send a message to the plugin.
    pub fn send_message(&mut self, message: String) {
        crate::util::send_message(&self.webview, message)
    }

    /// Resize the window to the given size (in logical pixels).
    ///
    /// Do note that plugin host may refuse to resize the window, in which case
    /// this method will return `false`.
    pub fn resize_window(&mut self, width: f64, height: f64) -> bool {
        let old = self.state.size.swap((width, height));

        if !self.context.request_resize() {
            // Resize failed.
            self.state.size.store(old);
            return false;
        }

        // We may need to reimplement this ourselves.
        // window.resize(Size { width: width as f64, height: height as f64 });

        // FIXME: handle error?
        let _ = self.webview.set_bounds(Rect {
            position: Position::Logical(LogicalPosition { x: 0.0, y: 0.0 }),
            size: Size::Logical(LogicalSize { width, height }),
        });

        true
    }

    /// Returns `true` if plugin parameters have changed since the last call to this method.
    pub fn params_changed(&mut self) -> bool {
        self.params_changed.replace(false)
    }

    /// Returns a `ParamSetter` which can be used to set parameter values.
    pub fn get_setter(&self) -> ParamSetter {
        ParamSetter::new(&*self.context)
    }

    /// Returns a reference to the `WebView` used by the editor.
    pub fn get_webview(&self) -> &WebView {
        &self.webview
    }
}
