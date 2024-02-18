use baseview::{Event, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use nih_plug::{
    params::persist::PersistentField,
    prelude::{Editor, GuiContext, ParamSetter},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    borrow::Cow,
    sync::{Arc, Mutex},
};
use wry::{
    http::{Request, Response},
    WebContext, WebView, WebViewBuilder,
};

use crossbeam::{
    atomic::AtomicCell,
    channel::{unbounded, Receiver},
};

pub use wry::http;

pub use baseview::{DropData, DropEffect, EventStatus, MouseEvent, Window};
pub use keyboard_types::*;

type EventLoopHandler = dyn FnMut(&WindowHandler, ParamSetter, &mut Window) + Send + Sync;
type KeyboardHandler = dyn Fn(KeyboardEvent) -> bool + Send + Sync;
type MouseHandler = dyn Fn(MouseEvent) -> EventStatus + Send + Sync;
type CustomProtocolHandler =
    dyn Fn(&Request<Vec<u8>>) -> wry::Result<Response<Cow<'static, [u8]>>> + Send + Sync;

pub struct WebViewEditor {
    state: Arc<WebViewState>,
    source: Arc<HTMLSource>,
    event_loop_handler: Arc<Mutex<Option<Box<EventLoopHandler>>>>,
    keyboard_handler: Arc<KeyboardHandler>,
    mouse_handler: Arc<MouseHandler>,
    custom_protocol: Option<(String, Arc<CustomProtocolHandler>)>,
    developer_mode: bool,
    background_color: (u8, u8, u8, u8),
}

pub enum HTMLSource {
    String(&'static str),
    URL(&'static str),
}

impl WebViewEditor {
    pub fn new(source: HTMLSource, state: Arc<WebViewState>) -> Self {
        Self {
            state,
            source: Arc::new(source),
            developer_mode: false,
            background_color: (255, 255, 255, 255),
            event_loop_handler: Arc::new(Mutex::new(None)),
            keyboard_handler: Arc::new(|_| false),
            mouse_handler: Arc::new(|_| EventStatus::Ignored),
            custom_protocol: None,
        }
    }

    pub fn with_background_color(mut self, background_color: (u8, u8, u8, u8)) -> Self {
        self.background_color = background_color;
        self
    }

    pub fn with_custom_protocol<F>(mut self, name: String, handler: F) -> Self
    where
        F: Fn(&Request<Vec<u8>>) -> wry::Result<Response<Cow<'static, [u8]>>>
            + 'static
            + Send
            + Sync,
    {
        self.custom_protocol = Some((name, Arc::new(handler)));
        self
    }

    pub fn with_event_loop<F>(mut self, handler: F) -> Self
    where
        F: FnMut(&WindowHandler, ParamSetter, &mut baseview::Window) + 'static + Send + Sync,
    {
        self.event_loop_handler = Arc::new(Mutex::new(Some(Box::new(handler))));
        self
    }

    pub fn with_developer_mode(mut self, mode: bool) -> Self {
        self.developer_mode = mode;
        self
    }

    pub fn with_keyboard_handler<F>(mut self, handler: F) -> Self
    where
        F: Fn(KeyboardEvent) -> bool + Send + Sync + 'static,
    {
        self.keyboard_handler = Arc::new(handler);
        self
    }

    pub fn with_mouse_handler<F>(mut self, handler: F) -> Self
    where
        F: Fn(MouseEvent) -> EventStatus + Send + Sync + 'static,
    {
        self.mouse_handler = Arc::new(handler);
        self
    }
}

pub struct WindowHandler {
    context: Arc<dyn GuiContext>,
    event_loop_handler: Arc<Mutex<Option<Box<EventLoopHandler>>>>,
    keyboard_handler: Arc<KeyboardHandler>,
    mouse_handler: Arc<MouseHandler>,
    webview: WebView,
    events_receiver: Receiver<Value>,
    state: Arc<WebViewState>,
}

impl WindowHandler {
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

    pub fn send_json<T: serde::Serialize>(&self, json: T) -> Result<(), String> {
        // TODO: proper error handling
        if let Ok(json_str) = serde_json::to_string(&json) {
            self.webview
                .evaluate_script(&format!(
                    "window.plugin.on_message_internal(`{}`);",
                    json_str
                ))
                .unwrap();
            return Ok(());
        } else {
            return Err("Can't convert JSON to string.".to_owned());
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
        let setter = ParamSetter::new(&*self.context);
        let handler = self.event_loop_handler.lock().unwrap().take();

        if let Some(mut handler) = handler {
            handler(self, setter, window);
            *self.event_loop_handler.lock().unwrap() = Some(handler);
        }
    }

    fn on_event(&mut self, _window: &mut baseview::Window, event: Event) -> EventStatus {
        match event {
            Event::Keyboard(event) => {
                if (self.keyboard_handler)(event) {
                    EventStatus::Captured
                } else {
                    EventStatus::Ignored
                }
            }
            Event::Mouse(mouse_event) => (self.mouse_handler)(mouse_event),
            _ => EventStatus::Ignored,
        }
    }
}

impl Editor for WebViewEditor {
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

        let state = self.state.clone();
        let developer_mode = self.developer_mode;
        let source = self.source.clone();
        let background_color = self.background_color;
        let custom_protocol = self.custom_protocol.clone();
        let event_loop_handler = self.event_loop_handler.clone();
        let keyboard_handler = self.keyboard_handler.clone();
        let mouse_handler = self.mouse_handler.clone();

        let window_handle = baseview::Window::open_parented(&parent, options, move |window| {
            let (events_sender, events_receiver) = unbounded();

            let mut web_context = WebContext::new(Some(std::env::temp_dir()));

            let mut webview_builder = WebViewBuilder::new_as_child(window)
                .with_bounds(wry::Rect {
                    x: 0,
                    y: 0,
                    width: state.size().0 as u32,
                    height: state.size().1 as u32,
                })
                .with_accept_first_mouse(true)
                .with_devtools(developer_mode)
                .with_web_context(&mut web_context)
                .with_initialization_script(include_str!("script.js"))
                .with_ipc_handler(move |msg: String| {
                    if let Ok(json_value) = serde_json::from_str(&msg) {
                        let _ = events_sender.send(json_value);
                    } else {
                        panic!("Invalid JSON from webview: {}.", msg);
                    }
                })
                .with_background_color(background_color);

            if let Some(custom_protocol) = custom_protocol.as_ref() {
                let handler = custom_protocol.1.clone();
                webview_builder = webview_builder
                    .with_custom_protocol(custom_protocol.0.to_owned(), move |request| {
                        handler(&request).unwrap()
                    });
            }

            let webview = match source.as_ref() {
                HTMLSource::String(html_str) => webview_builder.with_html(*html_str),
                HTMLSource::URL(url) => webview_builder.with_url(*url),
            }
            .unwrap()
            .build();

            WindowHandler {
                state,
                context,
                event_loop_handler,
                webview: webview.unwrap_or_else(|e| panic!("Failed to construct webview. {}", e)),
                events_receiver,
                keyboard_handler,
                mouse_handler,
            }
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

    fn param_values_changed(&self) {}

    fn param_value_changed(&self, _id: &str, _normalized_value: f32) {}

    fn param_modulation_changed(&self, _id: &str, _modulation_offset: f32) {}
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
