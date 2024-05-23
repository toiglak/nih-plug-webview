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
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use wry::{
    http::{self, header::CONTENT_TYPE, Request, Response},
    WebContext, WebView, WebViewBuilder,
};

pub use baseview;
pub use keyboard_types;
pub use wry;

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
    CustomProtocol(String),
}

pub trait EditorHandler: Sized + Send + Sync + 'static {
    /// Message type sent from the handler to the editor.
    type EditorTx: Serialize;
    /// Message type sent from the editor to the handler.
    type EditorRx: DeserializeOwned;

    fn init(&mut self, cx: &mut Context<Self>);
    fn on_frame(&mut self, cx: &mut Context<Self>);
    fn on_message(&mut self, cx: &mut Context<Self>, message: Self::EditorRx);
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
    pub fn send_message(&mut self, message: H::EditorTx) {
        self.handler.send_json(message);
    }

    /// Resize the window to the given size (in logical pixels).
    ///
    /// Do note that plugin host may refuse to resize the window, in which case
    /// this method will return `false`.
    pub fn resize_window(&mut self, width: u32, height: u32) -> bool {
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
    size: AtomicCell<(u32, u32)>,
}

impl WebviewState {
    /// Initialize the GUI's state. The window size is in logical pixels, so
    /// before it is multiplied by the DPI scaling factor.
    pub fn new(width: u32, height: u32) -> Arc<WebviewState> {
        Arc::new(WebviewState { size: AtomicCell::new((width, height)) })
    }

    /// Returns a `(width, height)` pair for the current size of the GUI in
    /// logical pixels.
    pub fn size(&self) -> (u32, u32) {
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

struct Config {
    title: String,
    state: Arc<WebviewState>,
    source: WebviewSource,
    handler: Box<Mutex<dyn EditorHandlerAny>>,
    context_dir: PathBuf,
    with_webview_fn: Mutex<Box<dyn Fn(WebViewBuilder) -> WebViewBuilder + Send + Sync + 'static>>,
}

/// A webview-based editor.
pub struct WebviewEditor {
    config: Arc<Config>,
    params_changed: Arc<AtomicBool>,
}

impl WebviewEditor {
    /// Creates a new `WebviewEditor`.
    pub fn new(
        title: String,
        source: WebviewSource,
        state: Arc<WebviewState>,
        handler: impl EditorHandler,
        context_dir: PathBuf,
    ) -> WebviewEditor {
        WebviewEditor {
            config: Arc::new(Config {
                title,
                state,
                source,
                handler: Box::new(Mutex::new(handler)),
                context_dir,
                with_webview_fn: Mutex::new(Box::new(|w| w)),
            }),
            params_changed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Creates a new `WebviewEditor` with a callback which allows to configure
    /// `WebViewBuilder`. Do note that some options will be overridden by the
    /// `EditorHandler` abstraction in order for it to function properly. To see
    /// which options are overridden, see the `Editor::spawn` implementation
    /// for the `WebviewEditor`.
    pub fn new_with_webview(
        title: String,
        source: WebviewSource,
        state: Arc<WebviewState>,
        handler: impl EditorHandler,
        context_dir: PathBuf,
        f: impl Fn(WebViewBuilder) -> WebViewBuilder + Send + Sync + 'static,
    ) -> WebviewEditor {
        WebviewEditor {
            config: Arc::new(Config {
                title,
                state,
                source,
                handler: Box::new(Mutex::new(handler)),
                context_dir,
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
            size: Size { width: width as f64, height: height as f64 },
            title: self.config.title.clone(),
            gl_config: None,
        };

        let config = self.config.clone();
        let params_changed = self.params_changed.clone();

        let window_handle = baseview::Window::open_parented(&parent, options, move |mut window| {
            let Config { title: _, state, source, handler, context_dir, with_webview_fn } =
                &*config;

            let (webview_to_editor_tx, webview_rx) = crossbeam::channel::unbounded();

            let mut webview_builder = WebViewBuilder::new_as_child(window);

            // Apply user configuration.
            webview_builder = with_webview_fn.lock().unwrap()(webview_builder);

            //
            // Configure the webview.

            let (width, height) = state.size.load();

            let mut web_context = WebContext::new(Some(context_dir.clone()));

            let webview_builder = webview_builder
                .with_bounds(wry::Rect { x: 0, y: 0, width, height })
                .with_ipc_handler(move |msg: String| {
                    if let Ok(json_value) = serde_json::from_str(&msg) {
                        let _ = webview_to_editor_tx.send(json_value);
                    } else {
                        panic!("Invalid JSON from webview: {}.", msg);
                    }
                })
                .with_web_context(&mut web_context);

            let webview = match (*source).clone() {
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
                WebviewSource::CustomProtocol(protocol) => {
                    webview_builder.with_url(format!("{protocol}://localhost").as_str())
                }
            }
            .unwrap()
            .build()
            .expect("Failed to construct webview. {}");

            let window_handler = WindowHandler {
                config: config.clone(),
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
        self.config.state.size.load()
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
    config: Arc<Config>,
    webview: WebView,
    context: Arc<dyn GuiContext>,
    params_changed: Arc<AtomicBool>,
    webview_rx: Receiver<Value>,
}

impl WindowHandler {
    fn context<'a, 'b>(&'a self, window: &'a mut Window<'b>) -> Context<'a, 'b, ()> {
        Context { handler: self, window, _p: PhantomData }
    }

    pub fn resize(&self, window: &mut baseview::Window, width: u32, height: u32) -> bool {
        let old = self.config.state.size.swap((width, height));

        if !self.context.request_resize() {
            // Resize failed.
            self.config.state.size.store(old);
            return false;
        }

        window.resize(Size { width: width as f64, height: height as f64 });

        self.webview.set_bounds(wry::Rect { x: 0, y: 0, width, height });

        true
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

    pub fn next_message(&self) -> Result<Value, crossbeam::channel::TryRecvError> {
        self.webview_rx.try_recv()
    }
}

impl baseview::WindowHandler for WindowHandler {
    fn on_frame(&mut self, window: &mut baseview::Window) {
        let mut handler = self.config.handler.lock().unwrap();
        let mut cx = self.context(window);

        // Call on_message for each message received from the webview.
        while let Ok(event) = self.next_message() {
            handler.on_message(&mut cx, event);
        }

        handler.on_frame(&mut cx);
    }

    fn on_event(&mut self, window: &mut baseview::Window, event: Event) -> EventStatus {
        // Focus the webview so that it can receive keyboard events.
        self.webview.focus();

        let mut handler = self.config.handler.lock().unwrap();
        let mut cx = self.context(window);

        handler.on_window_event(&mut cx, event)
    }
}

//
//
//

impl EditorHandler for () {
    type EditorRx = ();
    type EditorTx = ();

    fn init(&mut self, _cx: &mut Context<Self>) {}
    fn on_frame(&mut self, _cx: &mut Context<Self>) {}
    fn on_message(&mut self, _cx: &mut Context<Self>, _message: Self::EditorRx) {}
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
        let message =
            serde_json::from_value(message).expect("Could not parse event from webview into T.");
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
