use std::{rc::Rc, sync::Arc};

use nih_plug::{
    log,
    params::Param,
    prelude::{GuiContext, ParamSetter},
};
use wry::{
    dpi::{LogicalPosition, LogicalSize, Position, Size},
    Rect, WebView,
};

use crate::WebViewState;

pub trait EditorHandler: Send + 'static {
    /// Called once per frame. Use this to update internal state or trigger side effects.
    fn on_frame(&mut self, cx: &mut Context);

    /// Called when a message is received from the UI.
    ///
    /// ## About Initialization
    ///
    /// After the webview loads, you’ll likely want to sync the UI with the plugin state.
    /// A typical pattern is to send a `"ready"` message from the frontend once it’s mounted.
    ///
    /// In your frontend (e.g., in `DOMContentLoaded`, `onMount`, or `useEffect`):
    ///
    /// ```js
    /// document.addEventListener("DOMContentLoaded", () => {
    ///     window.plugin.listen((message) => {
    ///         // Handle messages from the plugin
    ///     });
    ///     window.plugin.send("ready");
    /// });
    /// ```
    ///
    /// Then in Rust, handle that `"ready"` message and respond with the initial state:
    ///
    /// ```rust
    /// fn on_message(&mut self, cx: &mut Context, message: String) {
    ///     if message == "ready" {
    ///         cx.send_message(json!({
    ///             "type": "init",
    ///             "attack": self.params.attack.value(),
    ///             "decay": self.params.decay.value(),
    ///             "sustain": self.params.sustain.value(),
    ///             "release": self.params.release.value(),
    ///             // ... other parameters
    ///         }).to_string());
    ///     }
    /// }
    /// ```
    fn on_message(&mut self, cx: &mut Context, message: String);

    /// Called when one or more parameters change.
    ///
    /// By default, [`on_param_value_changed`] and [`on_param_modulation_changed`]
    /// delegate to this method. Override those if you need finer control over
    /// individual parameter changes.
    ///
    /// This method may be called for bulk updates (e.g., when DAW loads a preset).
    ///
    /// ## Example
    ///
    /// ```rust
    /// fn on_params_changed(&mut self, cx: &mut Context) {
    ///     cx.send_message(json!({
    ///         "type": "params_changed",
    ///         "attack": self.params.attack.value(),
    ///         "decay": self.params.decay.value(),
    ///         "sustain": self.params.sustain.value(),
    ///         "release": self.params.release.value(),
    ///     }).to_string());
    /// }
    /// ```
    ///
    /// [`on_param_value_changed`]: EditorHandler::on_param_value_changed
    /// [`on_param_modulation_changed`]: EditorHandler::on_param_modulation_changed
    fn on_params_changed(&mut self, cx: &mut Context);

    /// Called when a parameter’s value changes.
    fn on_param_value_changed(&mut self, cx: &mut Context, id: &str, normalized_value: f32) {
        let _ = (id, normalized_value);
        self.on_params_changed(cx);
    }

    /// Called when a parameter’s modulation changes.
    fn on_param_modulation_changed(&mut self, cx: &mut Context, id: &str, modulation_offset: f32) {
        let _ = (id, modulation_offset);
        self.on_params_changed(cx);
    }
}

pub struct Context {
    pub(crate) state: Arc<WebViewState>,
    pub(crate) webview: Rc<WebView>,
    pub(crate) gui_context: Arc<dyn GuiContext>,
}

impl Context {
    /// Send a message to the plugin.
    pub fn send_message(&mut self, message: String) {
        log::debug!("Sending message to webview: {}", message);
        crate::util::send_message(&self.webview, message)
    }

    /// Resize the window to the given size (in logical pixels).
    ///
    /// Do note that plugin host may refuse to resize the window, in which case
    /// this method will return `false`.
    pub fn resize_window(&mut self, width: f64, height: f64) -> bool {
        log::debug!("Requesting resize to {}x{} (logical pixels)", width, height);
        let old = self.state.window_size.swap((width, height));

        if !self.gui_context.request_resize() {
            log::warn!("Host refused to resize the window.");
            // Resize failed.
            self.state.window_size.store(old);
            return false;
        }

        // We may need to reimplement this ourselves.
        // window.resize(Size { width: width as f64, height: height as f64 });

        if let Err(e) = self.webview.set_bounds(Rect {
            position: Position::Logical(LogicalPosition { x: 0.0, y: 0.0 }),
            size: Size::Logical(LogicalSize { width, height }),
        }) {
            log::warn!("Failed to set webview bounds: {}", e);
        }

        true
    }

    /// Returns a reference to the `WebView` used by the editor.
    pub fn get_webview(&self) -> &WebView {
        &self.webview
    }

    /// Returns a `ParamSetter` which can be used to set parameter values.
    pub fn get_param_setter(&self) -> ParamSetter {
        ParamSetter::new(&*self.gui_context)
    }

    /// Begin an automation gesture for a parameter. This should be called when the
    /// user starts dragging a knob or a slider.
    pub fn set_param_begin<P: Param>(&self, param: &P) {
        ParamSetter::new(&*self.gui_context).begin_set_parameter(param);
    }

    /// Set a parameter's value. This should be called as part of a gesture, between
    /// `set_parameter_begin` and `set_parameter_end`.
    pub fn set_param<P: Param>(&self, param: &P, value: P::Plain) {
        ParamSetter::new(&*self.gui_context).set_parameter(param, value);
    }

    /// Set a parameter's value from a normalized `[0, 1]` value. This should be
    /// called as part of a gesture, between `set_parameter_begin` and
    /// `set_parameter_end`.
    pub fn set_param_normalized<P: Param>(&self, param: &P, normalized: f32) {
        ParamSetter::new(&*self.gui_context).set_parameter_normalized(param, normalized);
    }

    /// End an automation gesture for a parameter. This should be called when the
    /// user releases the mouse button after dragging a knob or a slider.
    pub fn set_param_end<P: Param>(&self, param: &P) {
        ParamSetter::new(&*self.gui_context).end_set_parameter(param);
    }
}
