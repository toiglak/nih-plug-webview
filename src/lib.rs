use baseview::{Event, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use nih_plug::{
    params::persist::PersistentField,
    prelude::{Editor, GuiContext, ParamSetter},
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use std::{
    marker::PhantomData,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use wry::{WebContext, WebView, WebViewBuilder};

use crossbeam::{
    atomic::AtomicCell,
    channel::{unbounded, Receiver},
};

pub use wry::http;

pub use baseview::{DropData, DropEffect, MouseEvent, Window};
pub use keyboard_types::*;

type WindowEvent = baseview::Event;
type WindowEventStatus = baseview::EventStatus;

pub enum HTMLSource {
    String(String),
    URL(String),
}

pub trait EditorHandler: Sized + Send + Sync + 'static {
    /// Message type sent from the editor to the plugin.
    type ToPlugin: DeserializeOwned;
    /// Message type sent from the plugin to the editor.
    type ToEditor: Serialize;

    fn init(&mut self, cx: &mut Context<Self>);
    fn on_frame(&mut self, cx: &mut Context<Self>);
    fn on_message(&mut self, cx: &mut Context<Self>, message: Self::ToPlugin);
    fn on_window_event(&mut self, cx: &mut Context<Self>, event: WindowEvent) -> WindowEventStatus {
        let _ = (cx, event);
        WindowEventStatus::Ignored
    }
}

impl EditorHandler for () {
    type ToPlugin = ();
    type ToEditor = ();

    fn init(&mut self, _cx: &mut Context<Self>) {}
    fn on_frame(&mut self, _cx: &mut Context<Self>) {}
    fn on_message(&mut self, _cx: &mut Context<Self>, _message: Self::ToPlugin) {}
}

trait EditorHandlerAny: Send + Sync {
    fn init(&mut self, cx: &mut Context<()>);
    fn on_frame(&mut self, cx: &mut Context<()>);
    fn on_message(&mut self, cx: &mut Context<()>, message: Value);
    fn on_window_event(&mut self, cx: &mut Context<()>, event: WindowEvent) -> WindowEventStatus;
}

impl<H: EditorHandler> EditorHandlerAny for H {
    fn init(&mut self, cx: &mut Context<()>) {
        let cx = unsafe { std::mem::transmute(cx) };
        EditorHandler::on_frame(self, cx)
    }

    fn on_frame(&mut self, cx: &mut Context<()>) {
        let cx = unsafe { std::mem::transmute(cx) };
        EditorHandler::on_frame(self, cx)
    }

    fn on_message(&mut self, cx: &mut Context<()>, message: Value) {
        let message =
            serde_json::from_value(message).expect("Could not parse event from webview into T.");
        let cx = unsafe { std::mem::transmute(cx) };
        EditorHandler::on_message(self, cx, message)
    }

    fn on_window_event(&mut self, cx: &mut Context<()>, event: WindowEvent) -> WindowEventStatus {
        let cx = unsafe { std::mem::transmute(cx) };
        EditorHandler::on_window_event(self, cx, event)
    }
}

#[repr(C)]
pub struct Context<'a, 'b, H: EditorHandler> {
    window_handler: &'a WindowHandler,
    window: &'a mut Window<'b>,
    _p: PhantomData<H>,
}

impl<'a, 'b, H: EditorHandler> Context<'a, 'b, H> {
    pub fn send_message(&mut self, message: H::ToEditor) {
        self.window_handler.send_json(message);
    }

    pub fn next_message(&mut self) -> Option<H::ToPlugin> {
        self.window_handler
            .next_event()
            .map(|event| {
                serde_json::from_value(event).expect("Could not parse event from webview into T.")
            })
            .ok()
    }

    pub fn resize_window(&mut self, width: u32, height: u32) {
        self.window_handler.resize(self.window, width, height)
    }

    pub fn params_changed(&mut self) -> bool {
        self.window_handler
            .params_changed
            .swap(false, Ordering::SeqCst)
    }

    pub fn setter(&self) -> ParamSetter {
        ParamSetter::new(&*self.window_handler.context)
    }

    pub fn webview(&self) -> &WebView {
        &self.window_handler.webview
    }
}

pub struct WebviewEditor {
    handler: Arc<Mutex<dyn EditorHandlerAny>>,
    state: Arc<WebViewState>,
    source: Arc<HTMLSource>,
    params_changed: Arc<AtomicBool>,
    fn_with_builder:
        Mutex<Option<Box<dyn FnOnce(WebViewBuilder) -> WebViewBuilder + Send + Sync + 'static>>>,
}

impl WebviewEditor {
    /// Creates a new `WebviewEditor`.
    pub fn new(
        source: HTMLSource,
        webview_state: Arc<WebViewState>,
        handler: impl EditorHandler,
    ) -> WebviewEditor {
        WebviewEditor {
            handler: Arc::new(Mutex::new(handler)),
            state: webview_state,
            source: Arc::new(source),
            params_changed: Arc::new(AtomicBool::new(false)),
            fn_with_builder: Mutex::new(None),
        }
    }

    /// Creates a new `WebviewEditor` with a callback which allows to configure
    /// `WebViewBuilder`. Do note that some options will be overridden by the
    /// `EditorHandler` abstraction in order for it to function properly. To see
    /// which options are overridden, see the `Editor::spawn` implementation
    /// for the `WebviewEditor`.
    pub fn new_with_webview(
        source: HTMLSource,
        webview_state: Arc<WebViewState>,
        handler: impl EditorHandler,
        f: impl FnOnce(WebViewBuilder) -> WebViewBuilder + Send + Sync + 'static,
    ) -> WebviewEditor {
        WebviewEditor {
            handler: Arc::new(Mutex::new(handler)),
            state: webview_state,
            source: Arc::new(source),
            params_changed: Arc::new(AtomicBool::new(false)),
            fn_with_builder: Mutex::new(Some(Box::new(f))),
        }
    }
}

impl Editor for WebviewEditor {
    fn spawn(
        &self,
        parent: nih_plug::prelude::ParentWindowHandle,
        context: Arc<dyn GuiContext>,
    ) -> Box<dyn std::any::Any + Send> {
        let options = WindowOpenOptions {
            scale: WindowScalePolicy::SystemScaleFactor,
            size: Size {
                width: self.state.size().0 as f64,
                height: self.state.size().1 as f64,
            },
            title: "Plug-in".to_owned(),
        };

        let handler = self.handler.clone();
        let state = self.state.clone();
        let source = self.source.clone();
        let params_changed = self.params_changed.clone();
        let fn_with_builder = self.fn_with_builder.lock().unwrap().take();

        let window_handle = baseview::Window::open_parented(&parent, options, move |mut window| {
            let (events_sender, events_receiver) = unbounded();

            let mut web_context = WebContext::new(Some(std::env::temp_dir()));

            let mut webview_builder = WebViewBuilder::new_as_child(window);

            // Apply user configuration.
            if let Some(fn_with_builder) = fn_with_builder {
                webview_builder = (fn_with_builder)(webview_builder);
            }

            // Set properties required by `EditorHandler` abstraction.
            let webview_builder = webview_builder
                .with_bounds(wry::Rect {
                    x: 0,
                    y: 0,
                    width: state.size().0 as u32,
                    height: state.size().1 as u32,
                })
                .with_accept_first_mouse(true)
                // .with_devtools(developer_mode)
                .with_web_context(&mut web_context)
                .with_ipc_handler(move |msg: String| {
                    if let Ok(json_value) = serde_json::from_str(&msg) {
                        let _ = events_sender.send(json_value);
                    } else {
                        panic!("Invalid JSON from webview: {}.", msg);
                    }
                });

            let webview = match source.as_ref() {
                HTMLSource::String(html) => webview_builder.with_html(html),
                HTMLSource::URL(url) => webview_builder.with_url(url.as_str()),
            }
            .unwrap()
            .build()
            .expect("Failed to construct webview. {}");

            let window_handler = WindowHandler {
                handler: handler.clone(),
                state,
                context,
                webview,
                events_receiver,
                params_changed: params_changed.clone(),
            };

            let mut handler = handler.lock().unwrap();
            let mut cx = window_handler.context(&mut window);
            handler.init(&mut cx);

            window_handler
        });

        return Box::new(WrapSend {
            _window_handle: window_handle,
        });
    }

    fn size(&self) -> (u32, u32) {
        (self.state.size().0, self.state.size().1)
    }

    fn set_scale_factor(&self, _factor: f32) -> bool {
        // TODO: implement for Windows and Linux
        return false;
    }

    fn param_values_changed(&self) {
        self.params_changed.store(true, Ordering::SeqCst);
    }

    fn param_value_changed(&self, _id: &str, _normalized_value: f32) {
        self.params_changed.store(true, Ordering::SeqCst);
    }

    fn param_modulation_changed(&self, _id: &str, _modulation_offset: f32) {
        self.params_changed.store(true, Ordering::SeqCst);
    }
}

pub struct WindowHandler {
    handler: Arc<Mutex<dyn EditorHandlerAny>>,
    context: Arc<dyn GuiContext>,
    webview: WebView,
    events_receiver: Receiver<Value>,
    state: Arc<WebViewState>,
    pub params_changed: Arc<AtomicBool>,
}

impl WindowHandler {
    fn context<'a, 'b>(&'a self, window: &'a mut Window<'b>) -> Context<'a, 'b, ()> {
        Context {
            window_handler: self,
            window,
            _p: PhantomData,
        }
    }

    pub fn resize(&self, window: &mut baseview::Window, width: u32, height: u32) {
        let old = self.state.size.swap((width, height));

        if !self.context.request_resize() {
            // Resize failed.
            self.state.size.store(old);
            return;
        }

        window.resize(Size {
            width: width as f64,
            height: height as f64,
        });
        self.webview.set_bounds(wry::Rect {
            x: 0,
            y: 0,
            width,
            height,
        });
    }

    pub fn send_json<T: serde::Serialize>(&self, json: T) {
        if let Ok(json_str) = serde_json::to_string(&json) {
            self.webview
                .evaluate_script(&format!("window.plugin.__ipc.recvMessage(`{}`);", json_str))
                .unwrap();
        } else {
            panic!("Can't convert JSON to string.");
        }
    }

    pub fn next_event(&self) -> Result<Value, crossbeam::channel::TryRecvError> {
        self.events_receiver.try_recv()
    }

    pub fn size(&self) -> (u32, u32) {
        self.state.size.load()
    }
}

impl baseview::WindowHandler for WindowHandler {
    fn on_frame(&mut self, window: &mut baseview::Window) {
        let handler = self.handler.clone();
        let mut handler = handler.lock().unwrap();
        let mut cx = self.context(window);

        // Call on_message for each message received from the webview.
        while let Ok(event) = self.next_event() {
            handler.on_message(&mut cx, event);
        }

        handler.on_frame(&mut cx);
    }

    fn on_event(&mut self, window: &mut baseview::Window, event: Event) -> WindowEventStatus {
        // Focus the webview so that it can receive keyboard events.
        self.webview.focus();

        let handler = self.handler.clone();
        let mut handler = handler.lock().unwrap();
        let mut cx = self.context(window);

        handler.on_window_event(&mut cx, event)
    }
}

/// State for an `nih_plug_egui` editor.
#[derive(Debug, Serialize, Deserialize)]
pub struct WebViewState {
    /// The window's size in logical pixels before applying `scale_factor`.
    #[serde(with = "nih_plug::params::persist::serialize_atomic_cell")]
    size: AtomicCell<(u32, u32)>,
}

impl<'a> PersistentField<'a, WebViewState> for Arc<WebViewState> {
    fn set(&self, new_value: WebViewState) {
        self.size.store(new_value.size.load());
    }

    fn map<F, R>(&self, f: F) -> R
    where
        F: Fn(&WebViewState) -> R,
    {
        f(self)
    }
}

impl WebViewState {
    /// Initialize the GUI's state. The window size is in logical pixels, so before it is multiplied
    /// by the DPI scaling factor.
    pub fn from_size(width: u32, height: u32) -> Arc<WebViewState> {
        Arc::new(WebViewState {
            size: AtomicCell::new((width, height)),
        })
    }

    /// Returns a `(width, height)` pair for the current size of the GUI in logical pixels.
    pub fn size(&self) -> (u32, u32) {
        self.size.load()
    }
}

struct WrapSend {
    _window_handle: WindowHandle,
}
unsafe impl Send for WrapSend {}
