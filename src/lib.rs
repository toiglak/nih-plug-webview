use std::{
    cell::RefCell,
    path::PathBuf,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use ::raw_window_handle::{RawWindowHandle, WindowHandle};
use base64::{prelude::BASE64_STANDARD as BASE64, Engine};
use baseview::{Event, EventStatus};
use crossbeam::atomic::AtomicCell;
use nih_plug::{
    params::persist::PersistentField,
    prelude::{Editor, GuiContext, ParamSetter},
};
use objc2_app_kit::NSView;
use raw_window_handle::from_raw_window_handle_0_5_2;
use serde::{Deserialize, Serialize};
use wry::{
    dpi::{LogicalPosition, LogicalSize, Position},
    http::{self, header::CONTENT_TYPE, Request, Response},
    Rect, WebContext, WebView, WebViewBuilder, WebViewExtMacOS,
};

pub use baseview;
pub use keyboard_types;
pub use wry;

mod raw_window_handle;

const PLUGIN_OBJ: &str = "window.__NIH_PLUG_WEBVIEW__";

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
    CustomProtocol {
        /// The protocol over which the site assets will be served.
        protocol: String,
        /// The path at which a browser will attempt to load the initial page.
        url: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Message {
    Text(String),
    Binary(Vec<u8>),
}

pub trait EditorHandler: Send + 'static {
    fn init(&mut self, cx: &mut Context);
    fn on_frame(&mut self, cx: &mut Context);
    fn on_message(&mut self, send_message: &dyn Fn(Message), message: Message);
    fn on_window_event(&mut self, cx: &mut Context, event: Event) -> EventStatus {
        let _ = (cx, event);
        EventStatus::Ignored
    }
}

pub struct Context<'a> {
    handler: &'a WindowHandler,
}

impl<'a> Context<'a> {
    /// Send a message to the plugin.
    pub fn send_message(&mut self, message: Message) {
        self.handler.send_message(message);
    }

    /// Resize the window to the given size (in logical pixels).
    ///
    /// Do note that plugin host may refuse to resize the window, in which case
    /// this method will return `false`.
    pub fn resize_window(&mut self, width: f64, height: f64) -> bool {
        self.handler.resize(width, height)
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

pub struct WebViewConfig {
    /// The title of the window when running as a standalone application.
    pub title: String,
    /// The source for the site to be loaded in the webview.
    pub source: WebviewSource,
    /// The directory where webview will store its working data.
    pub workdir: PathBuf,
}

struct Init {
    editor: Box<Mutex<dyn EditorHandler>>,
    state: Arc<WebviewState>,
    #[expect(unused)]
    title: String,
    source: WebviewSource,
    workdir: PathBuf,
    with_webview_fn: Mutex<Box<dyn Fn(WebViewBuilder) -> WebViewBuilder + Send + Sync + 'static>>,
}

/// A webview-based editor.
pub struct WebviewEditor {
    config: Arc<Init>,
    params_changed: Arc<AtomicBool>,
    // TODO: Idk why Editor must be Send, but make UnsafeSend a SafeSend (panic if other thread).
    webview: UnsafeSend<Rc<RefCell<Option<Rc<WebView>>>>>,
}

impl WebviewEditor {
    pub fn new(
        editor: impl EditorHandler,
        state: &Arc<WebviewState>,
        config: WebViewConfig,
    ) -> WebviewEditor {
        Self::new_with_webview(editor, state, config, |webview| webview)
    }

    /// Creates a new `WebviewEditor` with the callback which allows you to configure many
    /// of the [`WebViewBuilder`](wry::WebViewBuilder) settings.
    ///
    /// **Note:** Some settings are overridden to ensure proper functionality of this
    /// library. Refer to the `WebviewEditor::spawn` implementation for details on which
    /// settings are affected.
    pub fn new_with_webview(
        editor: impl EditorHandler,
        state: &Arc<WebviewState>,
        config: WebViewConfig,
        f: impl Fn(WebViewBuilder) -> WebViewBuilder + Send + Sync + 'static,
    ) -> WebviewEditor {
        WebviewEditor {
            config: Arc::new(Init {
                editor: Box::new(Mutex::new(editor)),
                state: state.clone(),
                title: config.title,
                source: config.source,
                workdir: config.workdir,
                with_webview_fn: Mutex::new(Box::new(f)),
            }),
            params_changed: Arc::new(AtomicBool::new(false)),
            webview: UnsafeSend(Rc::new(RefCell::new(None))),
        }
    }
}

impl Editor for WebviewEditor {
    fn spawn(
        &self,
        window: nih_plug::prelude::ParentWindowHandle,
        context: Arc<dyn GuiContext>,
    ) -> Box<dyn std::any::Any + Send> {
        let window = from_raw_window_handle_0_5_2(&window);

        // OBSERVATION: When running as a standalone app, `ns_view.window()` is
        // None.
        //
        // Perhaps this is what happens: When running as a standalone app `window`
        // IS the `ns_window`, because that's what nih_plug provides it with.
        // However, when running in Bitwig, `window` is a `ns_view`, which is a
        // child of the `ns_window`, which is why need to call `window()` on it.
        //
        // Maybe we could assume that there's ALWAYS a `ns_window`, it's just
        // sometimes accessbile directly from `ns_view` and sometimes from
        // `ns_view.window()`.
        unsafe {
            log::debug!("ns_view: {:?}", window);
            let ns_view = as_ns_view(window);
            log::debug!("ns_view.window: {:?}", ns_view.window());
        };

        let webview_rc = self.webview.0.clone();

        // //// If the webview was already created, reuse it.

        // if let Some(webview) = webview_rc.borrow().clone() {
        //     #[allow(unused)]
        //     unsafe {
        //         let ns_view = as_ns_view(window);

        //         //// Obtain ns_window from ns_view

        //         let ns_window = ns_view.window().unwrap();
        //         let (ns_window, ns_window_ptr) =
        //             (ns_window.clone(), objc2::rc::Retained::into_raw(ns_window));

        //         //// Reparent and focus the window

        //         webview.reparent(ns_window_ptr).unwrap();
        //         // NOTE: This breaks Shift + W — we need to press Shift + W twice.
        //         webview.activate().unwrap();
        //         // Make first responder.
        //         webview.focus().unwrap();

        //         return Box::new(EditorHandle { webview });
        //     }
        // }

        //// Create webview

        let webview_builder = configure_webview(
            context,
            webview_rc.clone(),
            self.config.clone(),
            self.config.state.size.load(),
            self.params_changed.clone(),
        );

        // We must use build_as_child(), because, unlike build(), it assumes that "parent"
        // exists and it doesn't consume all keyboard events.
        let webview = webview_builder.build_as_child(&window).expect("failed to construct webview");

        ////

        let webview = Rc::new(webview);
        webview_rc.replace(Some(webview.clone()));
        return Box::new(EditorHandle { webview });
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

unsafe fn as_ns_view<'a>(parent: WindowHandle) -> &'a NSView {
    let ns_view_ptr = match parent.as_raw() {
        RawWindowHandle::AppKit(app_kit_window_handle) => {
            app_kit_window_handle.ns_view.cast::<NSView>()
        }
        _ => panic!(),
    };
    ns_view_ptr.as_ptr().as_ref().unwrap()
}

fn configure_webview<'a>(
    context: Arc<dyn GuiContext>,
    webview_rc: Rc<RefCell<Option<Rc<WebView>>>>,
    config: Arc<Init>,
    (width, height): (f64, f64),
    params_changed: Arc<AtomicBool>,
) -> WebViewBuilder<'a> {
    let mut webview_builder = WebViewBuilder::new();

    // Apply user configuration.
    webview_builder = config.with_webview_fn.lock().unwrap()(webview_builder);

    let mut _web_context = WebContext::new(Some(config.workdir.clone()));

    let ipc_handler = {
        let webview_rc = webview_rc.clone();
        let config = config.clone();
        let context = context.clone();
        move |request: Request<String>| {
            let webview_rc = webview_rc.clone();
            let config = config.clone();
            let context = context.clone();
            ipc_handler(params_changed.clone(), webview_rc, config, context, request);
        }
    };

    let webview_builder = webview_builder
        .with_accept_first_mouse(true)
        .with_focused(true)
        .with_bounds(Rect {
            position: Position::Logical(LogicalPosition { x: 0.0, y: 0.0 }),
            size: wry::dpi::Size::Logical(LogicalSize { width, height }),
        })
        .with_initialization_script(include_str!("lib.js"))
        .with_ipc_handler(ipc_handler);
    // .with_web_context(&mut web_context);

    let webview_builder = match config.source.clone() {
        WebviewSource::URL(url) => webview_builder.with_url(url.as_str()),
        WebviewSource::HTML(html) => webview_builder.with_html(html),
        WebviewSource::DirPath(root) => webview_builder
            .with_custom_protocol("wry".to_string(), move |_id, request| {
                match get_wry_response(&root, request) {
                    Ok(r) => r.map(Into::into),
                    Err(e) => http::Response::builder()
                        .header(CONTENT_TYPE, "text/plain")
                        .status(500)
                        .body(e.to_string().as_bytes().to_vec())
                        .unwrap()
                        .map(Into::into),
                }
            })
            .with_url("wry://localhost"),
        WebviewSource::CustomProtocol { url, protocol } => {
            webview_builder.with_url(format!("{protocol}://localhost/{url}").as_str())
        }
    };
    webview_builder
}

fn ipc_handler(
    params_changed: Arc<AtomicBool>,
    webview_rc: Rc<RefCell<Option<Rc<WebView>>>>,
    config: Arc<Init>,
    context: Arc<dyn GuiContext>,
    request: Request<String>,
) {
    let webview = webview_rc.borrow();
    let webview: &WebView = webview.as_ref().unwrap();

    let message = request.into_body();

    let send_message = |message: Message| match message {
        Message::Text(text) => {
            let text = text.replace("`", r#"\`"#);
            let script = format!("{PLUGIN_OBJ}.onmessage(`text`,`{}`);", text);
            webview.evaluate_script(&script).ok();
        }
        Message::Binary(bytes) => {
            let bytes = BASE64.encode(&bytes);
            let script =
                format!("{PLUGIN_OBJ}.onmessage(`binary`, {PLUGIN_OBJ}.decodeBase64(`{bytes}`));");
            webview.evaluate_script(&script).ok();
        }
    };

    if message.starts_with("frame") {
        if let Err(err) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut editor = config.editor.lock().unwrap();

            let handler = &WindowHandler {
                init: config.clone(),
                webview: webview_rc.borrow().clone().unwrap(),
                context: context.clone(),
                params_changed: params_changed.clone(),
            };
            let mut cx = Context { handler };

            editor.on_frame(&mut cx);
        })) {
            // NOTE: We catch panic here, because `baseview` doesn't run from the "main entry
            // point", instead it schedules this handler as a task on the main thread. For
            // some reason, on macos if you panic from a task the process will be forever
            // stuck and you won't be able terminate it until you log out.
            eprintln!("{:?}", err);
            std::process::exit(1);
        }
    } else if message.starts_with("text,") {
        let message = message.trim_start_matches("text,");
        let mut editor = config.editor.lock().unwrap();
        editor.on_message(&send_message, Message::Text(message.to_string()));
    } else if message.starts_with("binary,") {
        let message = message.trim_start_matches("binary,");
        let bytes = BASE64.decode(message.as_bytes()).unwrap();
        let mut editor = config.editor.lock().unwrap();
        editor.on_message(&send_message, Message::Binary(bytes));
    }
}

/// A handle to the editor window, returned from [`Editor::spawn`]. Host will
/// call [`drop`] on it when the window is supposed to be closed.
struct EditorHandle {
    #[expect(unused)]
    webview: Rc<WebView>,
}

unsafe impl Send for EditorHandle {}

impl Drop for EditorHandle {
    fn drop(&mut self) {
        // TODO: Consider notifying the plugin that the window was closed.
        // self.window_handle.close();
    }
}

/// This structure manages the editor window's event loop.
struct WindowHandler {
    init: Arc<Init>,
    webview: Rc<WebView>,
    context: Arc<dyn GuiContext>,
    params_changed: Arc<AtomicBool>,
}

impl WindowHandler {
    fn context<'a>(&'a self) -> Context<'a> {
        Context { handler: self }
    }

    fn resize(&self, width: f64, height: f64) -> bool {
        let old = self.init.state.size.swap((width, height));

        if !self.context.request_resize() {
            // Resize failed.
            self.init.state.size.store(old);
            return false;
        }

        // We may need to reimplement this ourselves.
        // window.resize(Size { width: width as f64, height: height as f64 });

        // FIXME: handle error?
        let _ = self.webview.set_bounds(Rect {
            position: Position::Logical(LogicalPosition { x: 0.0, y: 0.0 }),
            size: wry::dpi::Size::Logical(LogicalSize { width, height }),
        });

        true
    }

    fn send_message(&self, message: Message) {
        match message {
            Message::Text(text) => {
                let text = text.replace("`", r#"\`"#);
                let script = format!("{PLUGIN_OBJ}.onmessage(`text`,`{}`);", text);
                self.webview.evaluate_script(&script).ok();
            }
            Message::Binary(bytes) => {
                let bytes = BASE64.encode(&bytes);
                let script = format!(
                    "{PLUGIN_OBJ}.onmessage(`binary`, {PLUGIN_OBJ}.decodeBase64(`{bytes}`));"
                );
                self.webview.evaluate_script(&script).ok();
            }
        }
    }
}

impl baseview::WindowHandler for WindowHandler {
    fn on_frame(&mut self, _window: &mut baseview::Window) {
        if let Err(err) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut editor = self.init.editor.lock().unwrap();
            let mut cx = self.context();
            editor.on_frame(&mut cx);
        })) {
            // NOTE: We catch panic here, because `baseview` doesn't run from the "main entry
            // point", instead it schedules this handler as a task on the main thread. For
            // some reason, on macos if you panic from a task the process will be forever
            // stuck and you won't be able terminate it until you log out.
            eprintln!("{:?}", err);
            std::process::exit(1);
        }
    }

    fn on_event(&mut self, _window: &mut baseview::Window, event: Event) -> EventStatus {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut editor = self.init.editor.lock().unwrap();
            let mut cx = self.context();

            editor.on_window_event(&mut cx, event)
        })) {
            Ok(status) => status,
            Err(err) => {
                // NOTE: We catch panic here, because `baseview` doesn't run from the "main entry
                // point", instead it schedules this handler as a task on the main thread. For
                // some reason, on macos if you panic from a task the process will be forever
                // stuck and you won't be able terminate it until you log out.
                eprintln!("{:?}", err);
                std::process::exit(1);
            }
        }
    }
}

fn get_wry_response(
    root: &PathBuf,
    request: Request<Vec<u8>>,
) -> Result<http::Response<Vec<u8>>, Box<dyn std::error::Error>> {
    let path = request.uri().path();
    let path = if path == "/" {
        "index.html"
    } else {
        // Remove leading slash.
        &path[1..]
    };
    let path = std::fs::canonicalize(root.join(path))?;
    let content = std::fs::read(&path)?;

    let mimetype =
        mime_guess::from_path(&path).first().map(|mime| mime.to_string()).unwrap_or("".to_string());

    Response::builder().header(CONTENT_TYPE, mimetype).body(content).map_err(Into::into)
}

struct UnsafeSend<T>(T);
unsafe impl<T> Send for UnsafeSend<T> {}
