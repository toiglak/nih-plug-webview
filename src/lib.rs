use std::{
    marker::PhantomData,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use baseview::{Event, EventStatus, Size, Window, WindowOpenOptions, WindowScalePolicy};
use crossbeam::{atomic::AtomicCell, channel::Receiver};
use nih_plug::{
    params::persist::PersistentField,
    prelude::{Editor, GuiContext, ParamSetter},
};
use raw_window_handle::from_raw_window_handle_0_5_2;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use wry::{
    dpi::{LogicalPosition, LogicalSize, Position},
    http::{self, header::CONTENT_TYPE, Request, Response},
    Rect, WebContext, WebView, WebViewBuilder,
};

pub use baseview;
pub use keyboard_types;
pub use wry;

mod raw_window_handle;

#[derive(Debug, Clone)]
pub enum WebviewSource {
    /// Loads a web page from the given URL.
    ///
    /// For example `https://example.com`.
    URL(String),
    /// Loads a web page from the given HTML content.
    ///
    /// For example `<h1>Hello, world!</h1>`.
    HTML(String),
    /// Serves a directory over custom protocol (`wry://`).
    ///
    /// Make sure that the directory includes an `index.html` file, as it is the
    /// entry point for the webview.
    DirPath(PathBuf),
    /// Serves assets over a custom protocol.
    ///
    /// - `url_path` is the path at which a browser will attempt to load the initial page
    ///
    /// This variant allows you to serve assets from memory or any other custom
    /// source. To use this variant, you need to pair it with either
    /// [`WebViewBuilder::with_custom_protocol`] or
    /// [`WebViewBuilder::with_asynchronous_custom_protocol`].
    ///
    /// ## Example
    ///
    /// ```rust
    /// WebviewEditor::new_with_webview("my-plugin", source, params, handler, |webview| {
    ///     webview.with_custom_protocol("wry".to_string(), |request| {
    ///         // Handle the request here.
    ///         Ok(http::Response::builder())
    ///     })
    /// });
    CustomProtocol { protocol: String, url_path: String },
}

pub trait EditorHandler: Sized + Send + Sync + 'static {
    /// Message type sent from the handler to the editor.
    type EditorRx: Serialize;
    /// Message type sent from the editor to the handler.
    type HandlerRx: DeserializeOwned;

    fn init(&mut self, cx: &mut Context<Self>);
    fn on_frame(&mut self, cx: &mut Context<Self>);
    fn on_message(&mut self, cx: &mut Context<Self>, message: Self::HandlerRx);
    fn on_window_event(&mut self, cx: &mut Context<Self>, event: Event) -> EventStatus {
        let _ = (cx, event);
        EventStatus::Ignored
    }
}

#[repr(C)]
pub struct Context<'a, 'b, H: EditorHandler> {
    handler: &'a WindowHandler,
    window: &'a mut Window<'b>,
    _p: PhantomData<H>,
}

impl<'a, 'b, H: EditorHandler> Context<'a, 'b, H> {
    /// Send a message to the plugin.
    pub fn send_message(&mut self, message: H::EditorRx) {
        self.handler.send_json(message);
    }

    /// Resize the window to the given size (in logical pixels).
    ///
    /// Do note that plugin host may refuse to resize the window, in which case
    /// this method will return `false`.
    pub fn resize_window(&mut self, width: f64, height: f64) -> bool {
        self.handler.resize(self.window, width, height)
    }

    /// Returns `true` if plugin parameters have changed since the last call to this method.
    pub fn params_changed(&mut self) -> bool {
        self.handler.params_changed.swap(false, Ordering::SeqCst)
    }

    /// Returns a `ParamSetter` which can be used to set parameter values.
    pub fn get_setter(&self) -> ParamSetter {
        ParamSetter::new(&*self.handler.context)
    }

    /// Returns a reference to the `WebView` used by the editor.
    pub fn get_webview(&self) -> &WebView {
        &self.handler.webview
    }
}

/// `nih_plug_webview`'s state that should be persisted between sessions (like window size).
///
/// Add it as a persistent parameter to your plugin's state.
#[derive(Debug, Serialize, Deserialize)]
pub struct WebviewState {
    /// The window's size in logical pixels before applying `scale_factor`.
    #[serde(with = "nih_plug::params::persist::serialize_atomic_cell")]
    size: AtomicCell<(f64, f64)>,
}

impl WebviewState {
    /// Initialize the GUI's state. The window size is in logical pixels, so
    /// before it is multiplied by the DPI scaling factor.
    pub fn new(width: f64, height: f64) -> WebviewState {
        WebviewState { size: AtomicCell::new((width, height)) }
    }

    /// Returns a `(width, height)` pair for the current size of the GUI in
    /// logical pixels.
    pub fn size(&self) -> (f64, f64) {
        self.size.load()
    }
}

impl<'a> PersistentField<'a, WebviewState> for Arc<WebviewState> {
    fn set(&self, new_value: WebviewState) {
        self.size.store(new_value.size.load());
    }

    fn map<F, R>(&self, f: F) -> R
    where
        F: Fn(&WebviewState) -> R,
    {
        f(self)
    }
}

struct Init {
    handler: Box<Mutex<dyn EditorHandlerAny>>,
    state: Arc<WebviewState>,
    title: String,
    source: WebviewSource,
    workdir: PathBuf,
    with_webview_fn: Mutex<Box<dyn Fn(WebViewBuilder) -> WebViewBuilder + Send + Sync + 'static>>,
}

pub struct WebViewConfig {
    /// The title of the window when running as a standalone application.
    pub title: String,
    /// The source for the site to be loaded in the webview.
    pub source: WebviewSource,
    /// The directory where webview will store its working data.
    pub workdir: PathBuf,
}

/// A webview-based editor.
pub struct WebviewEditor {
    config: Arc<Init>,
    params_changed: Arc<AtomicBool>,
}

impl WebviewEditor {
    pub fn new(
        editor: impl EditorHandler,
        state: &Arc<WebviewState>,
        config: WebViewConfig,
    ) -> WebviewEditor {
        WebviewEditor {
            config: Arc::new(Init {
                handler: Box::new(Mutex::new(editor)),
                state: state.clone(),
                title: config.title,
                source: config.source,
                workdir: config.workdir,
                with_webview_fn: Mutex::new(Box::new(|w| w)),
            }),
            params_changed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Creates a new `WebviewEditor` with the callback which allows you to configure many
    /// of the [`WebViewBuilder`](wry::WebViewBuilder) settings.
    ///
    /// **Note:** Some settings are overridden to ensure proper functionality of this
    /// library. Refer to the `WebviewEditor::spawn` implementation for details on which
    /// settings are affected.
    pub fn new_with_webview_builder(
        editor: impl EditorHandler,
        state: &Arc<WebviewState>,
        config: WebViewConfig,
        f: impl Fn(WebViewBuilder) -> WebViewBuilder + Send + Sync + 'static,
    ) -> WebviewEditor {
        WebviewEditor {
            config: Arc::new(Init {
                handler: Box::new(Mutex::new(editor)),
                state: state.clone(),
                title: config.title,
                source: config.source,
                workdir: config.workdir,
                with_webview_fn: Mutex::new(Box::new(f)),
            }),
            params_changed: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl Editor for WebviewEditor {
    fn spawn(
        &self,
        parent: nih_plug::prelude::ParentWindowHandle,
        context: Arc<dyn GuiContext>,
    ) -> Box<dyn std::any::Any + Send> {
        let (width, height) = self.config.state.size.load();

        let options = WindowOpenOptions {
            scale: WindowScalePolicy::SystemScaleFactor,
            size: baseview::Size { width, height },
            title: self.config.title.clone(),
        };

        let config = self.config.clone();
        let params_changed = self.params_changed.clone();

        let window_handle = baseview::Window::open_parented(&parent, options, move |mut window| {
            let Init { title: _, state, source, handler, workdir, with_webview_fn } = &*config;

            let (webview_to_editor_tx, webview_rx) = crossbeam::channel::unbounded();

            let new_window = from_raw_window_handle_0_5_2(window);

            let mut webview_builder = WebViewBuilder::new_as_child(&new_window);

            // Apply user configuration.
            webview_builder = with_webview_fn.lock().unwrap()(webview_builder);

            //
            // Configure the webview.

            let (width, height) = state.size.load();

            let mut web_context = WebContext::new(Some(workdir.clone()));

            let webview_builder = webview_builder
                .with_bounds(Rect {
                    position: Position::Logical(LogicalPosition { x: 0.0, y: 0.0 }),
                    size: wry::dpi::Size::Logical(LogicalSize { width, height }),
                })
                .with_initialization_script(
                    "window.host = {
                        onmessage: function() {},
                        postMessage: function(msg) {
                            window.ipc.postMessage(JSON.stringify(msg));
                        }
                    };",
                )
                .with_ipc_handler(move |request: Request<String>| {
                    let message = request.body();

                    if let Ok(json_value) = serde_json::from_str(&message) {
                        let _ = webview_to_editor_tx.send(json_value);
                    } else {
                        panic!("Invalid JSON from webview: {}.", message);
                    }
                })
                .with_web_context(&mut web_context);

            let webview_builder = match (*source).clone() {
                WebviewSource::URL(url) => webview_builder.with_url(url.as_str()),
                WebviewSource::HTML(html) => webview_builder.with_html(html),
                WebviewSource::DirPath(root) => webview_builder
                    .with_custom_protocol(
                        "wry".to_string(), //
                        move |request| match get_wry_response(&root, request) {
                            Ok(r) => r.map(Into::into),
                            Err(e) => http::Response::builder()
                                .header(CONTENT_TYPE, "text/plain")
                                .status(500)
                                .body(e.to_string().as_bytes().to_vec())
                                .unwrap()
                                .map(Into::into),
                        },
                    )
                    .with_url("wry://localhost"),
                WebviewSource::CustomProtocol { url_path: url, protocol } => {
                    webview_builder.with_url(format!("{protocol}://localhost/{url}").as_str())
                }
            };

            let webview = webview_builder.build().expect("Failed to construct webview");

            let window_handler = WindowHandler {
                init: config.clone(),
                context,
                webview,
                webview_rx,
                params_changed,
            };

            let mut handler = handler.lock().unwrap();
            let mut cx = window_handler.context(&mut window);
            handler.init(&mut cx);

            window_handler
        });

        return Box::new(EditorHandle { window_handle });
    }

    fn size(&self) -> (u32, u32) {
        let (a, b) = self.config.state.size.load();
        (a as u32, b as u32)
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

/// A handle to the editor window, returned from [`Editor::spawn`]. Host will
/// call [`drop`] on it when the window is supposed to be closed.
struct EditorHandle {
    window_handle: baseview::WindowHandle,
}

unsafe impl Send for EditorHandle {}

impl Drop for EditorHandle {
    fn drop(&mut self) {
        self.window_handle.close();
    }
}

/// This structure manages the editor window's event loop.
struct WindowHandler {
    init: Arc<Init>,
    webview: WebView,
    context: Arc<dyn GuiContext>,
    params_changed: Arc<AtomicBool>,
    webview_rx: Receiver<Value>,
}

impl WindowHandler {
    fn context<'a, 'b>(&'a self, window: &'a mut Window<'b>) -> Context<'a, 'b, ()> {
        Context { handler: self, window, _p: PhantomData }
    }

    pub fn resize(&self, window: &mut baseview::Window, width: f64, height: f64) -> bool {
        let old = self.init.state.size.swap((width, height));

        if !self.context.request_resize() {
            // Resize failed.
            self.init.state.size.store(old);
            return false;
        }

        window.resize(Size { width: width as f64, height: height as f64 });

        // FIXME: handle error?
        let _ = self.webview.set_bounds(Rect {
            position: Position::Logical(LogicalPosition { x: 0.0, y: 0.0 }),
            size: wry::dpi::Size::Logical(LogicalSize { width, height }),
        });

        true
    }

    pub fn send_json<T: serde::Serialize>(&self, json: T) {
        if let Ok(json_str) = serde_json::to_string(&json) {
            self.webview
                .evaluate_script(&format!("window.host.onmessage(`{}`);", json_str))
                .unwrap();
        } else {
            panic!("Can't convert JSON to string.");
        }
    }

    pub fn next_message(&self) -> Result<Value, crossbeam::channel::TryRecvError> {
        self.webview_rx.try_recv()
    }
}

impl baseview::WindowHandler for WindowHandler {
    fn on_frame(&mut self, window: &mut baseview::Window) {
        let mut handler = self.init.handler.lock().unwrap();
        let mut cx = self.context(window);

        // Call on_message for each message received from the webview.
        while let Ok(event) = self.next_message() {
            handler.on_message(&mut cx, event);
        }

        handler.on_frame(&mut cx);
    }

    fn on_event(&mut self, window: &mut baseview::Window, event: Event) -> EventStatus {
        let mut handler = self.init.handler.lock().unwrap();
        let mut cx = self.context(window);

        handler.on_window_event(&mut cx, event)
    }
}

//
//
//

impl EditorHandler for () {
    type HandlerRx = ();
    type EditorRx = ();

    fn init(&mut self, _cx: &mut Context<Self>) {}
    fn on_frame(&mut self, _cx: &mut Context<Self>) {}
    fn on_message(&mut self, _cx: &mut Context<Self>, _message: Self::HandlerRx) {}
}

trait EditorHandlerAny: Send + Sync {
    fn init(&mut self, cx: &mut Context<()>);
    fn on_frame(&mut self, cx: &mut Context<()>);
    fn on_message(&mut self, cx: &mut Context<()>, message: Value);
    fn on_window_event(&mut self, cx: &mut Context<()>, event: Event) -> EventStatus;
}

impl<H: EditorHandler> EditorHandlerAny for H {
    fn init(&mut self, cx: &mut Context<()>) {
        let cx = unsafe { std::mem::transmute(cx) };
        EditorHandler::init(self, cx)
    }

    fn on_frame(&mut self, cx: &mut Context<()>) {
        let cx = unsafe { std::mem::transmute(cx) };
        EditorHandler::on_frame(self, cx)
    }

    fn on_message(&mut self, cx: &mut Context<()>, message: Value) {
        let parse_err = |e| {
            format!(
                "failed to deserialize message from webview: `{e}`. received `{}`",
                message.to_string()
            )
        };
        let message = serde_json::from_value(message.clone()).map_err(parse_err).unwrap();
        let cx = unsafe { std::mem::transmute(cx) };
        EditorHandler::on_message(self, cx, message)
    }

    fn on_window_event(&mut self, cx: &mut Context<()>, event: Event) -> EventStatus {
        let cx = unsafe { std::mem::transmute(cx) };
        EditorHandler::on_window_event(self, cx, event)
    }
}

/// TODO: Use async.
fn get_wry_response(
    root: &PathBuf,
    request: Request<Vec<u8>>,
) -> Result<http::Response<Vec<u8>>, Box<dyn std::error::Error>> {
    let path = request.uri().path();
    let path = if path == "/" {
        "index.html"
    } else {
        //  removing leading slash
        &path[1..]
    };
    let path = std::fs::canonicalize(root.join(path))?;
    let content = std::fs::read(&path)?;

    let mimetype =
        mime_guess::from_path(&path).first().map(|mime| mime.to_string()).unwrap_or("".to_string());

    Response::builder().header(CONTENT_TYPE, mimetype).body(content).map_err(Into::into)
}
