use std::{
    cell::RefCell,
    path::PathBuf,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use base64::{prelude::BASE64_STANDARD as BASE64, Engine};
use baseview::{Event, EventStatus, Size, Window};
use crossbeam::atomic::AtomicCell;
use nih_plug::{
    params::persist::PersistentField,
    prelude::{Editor, GuiContext, ParamSetter},
};
use raw_window_handle::from_raw_window_handle_0_5_2;
use serde::{Deserialize, Serialize};
use wry::{
    dpi::{LogicalPosition, LogicalSize, Position},
    http::{self, header::CONTENT_TYPE, Request, Response},
    Rect, WebContext, WebView, WebViewBuilder,
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

pub struct Context<'a, 'b> {
    handler: &'a WindowHandler,
    window: &'a mut Window<'b>,
}

impl<'a, 'b> Context<'a, 'b> {
    /// Send a message to the plugin.
    pub fn send_message(&mut self, message: Message) {
        self.handler.send_message(message);
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
        }
    }
}

impl Editor for WebviewEditor {
    fn spawn(
        &self,
        parent: nih_plug::prelude::ParentWindowHandle,
        _context: Arc<dyn GuiContext>,
    ) -> Box<dyn std::any::Any + Send> {
        // let (width, height) = self.config.state.size.load();

        // let options = WindowOpenOptions {
        //     scale: WindowScalePolicy::SystemScaleFactor,
        //     size: baseview::Size { width, height },
        //     title: self.config.title.clone(),
        // };

        let config = self.config.clone();
        // let params_changed = self.params_changed.clone();

        ////

        let webview_rc = Rc::new(RefCell::new(None));

        //
        // Configure the webview.

        // let Init { state, source, editor, workdir, with_webview_fn, .. } = &*config;
        let (width, height) = config.state.size.load();

        let mut webview_builder = WebViewBuilder::new();

        // Apply user configuration.
        webview_builder = config.with_webview_fn.lock().unwrap()(webview_builder);

        let mut _web_context = WebContext::new(Some(config.workdir.clone()));

        let webview_builder = webview_builder
            .with_bounds(Rect {
                position: Position::Logical(LogicalPosition { x: 0.0, y: 0.0 }),
                size: wry::dpi::Size::Logical(LogicalSize { width, height }),
            })
            .with_initialization_script(include_str!("lib.js"))
            .with_ipc_handler({
                let webview_rc = webview_rc.clone();
                let config = config.clone();
                move |request: Request<String>| {
                    let webview = webview_rc.borrow();
                    let webview: &WebView = webview.as_ref().unwrap();
                    let message = request.into_body();

                    let send_message = |message: Message| {
                        match message {
                            Message::Text(text) => {
                                let text = text.replace("`", r#"\`"#);
                                let script = format!("{PLUGIN_OBJ}.onmessage(`text`,`{}`);", text);
                                webview.evaluate_script(&script).ok();
                            }
                            Message::Binary(bytes) => {
                                let bytes = BASE64.encode(&bytes);
                                let script = format!(
                                    "{PLUGIN_OBJ}.onmessage(`binary`, {PLUGIN_OBJ}.decodeBase64(`{bytes}`));"
                                );
                                webview.evaluate_script(&script).ok();
                            }
                        }
                    };

                    if message.starts_with("text,") {
                        let message = message.trim_start_matches("text,");
                        let mut editor = config.editor.lock().unwrap();
                        editor.on_message(&send_message, Message::Text(message.to_string()));
                    } else if message.starts_with("binary,") {
                        let message = message.trim_start_matches("binary,");
                        let bytes = BASE64.decode(message.as_bytes()).unwrap();
                        let mut editor = config.editor.lock().unwrap();
                        editor.on_message(&send_message,Message::Binary(bytes));
                    }
                }
            });
        // .with_web_context(&mut web_context);

        let webview_builder = match config.source.clone() {
            WebviewSource::URL(url) => webview_builder.with_url(url.as_str()),
            WebviewSource::HTML(html) => webview_builder.with_html(html),
            WebviewSource::DirPath(root) => webview_builder
                .with_custom_protocol(
                    "wry".to_string(), //
                    move |_id, request| match get_wry_response(&root, request) {
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
            WebviewSource::CustomProtocol { url, protocol } => {
                webview_builder.with_url(format!("{protocol}://localhost/{url}").as_str())
            }
        };

        let new_window = from_raw_window_handle_0_5_2(&parent);

        let webview =
            webview_builder.build_as_child(&new_window).expect("Failed to construct webview");

        webview_rc.replace(Some(webview));

        // let window_handle = baseview::Window::open_parented(&parent, options, move |mut window| {
        //     // Theoretically, this is reparenting! I wonder if it would allow us to
        //     // preserve the webview between window close/open?

        //     // let window_ptr = match new_window.as_raw() {
        //     //     ::raw_window_handle::RawWindowHandle::AppKit(app_kit_window_handle) => {
        //     //         { app_kit_window_handle.ns_view }.cast::<_>() // i cannot believe it worked lol
        //     //     }
        //     //     _ => todo!(),
        //     // };
        //     //
        //     // webview.reparent(window_ptr.as_ptr()).unwrap();

        //     let window_handler = WindowHandler {
        //         init: config.clone(),
        //         context,
        //         webview: Rc::new(webview),
        //         webview_rx,
        //         params_changed,
        //     };

        //     let mut editor = editor.lock().unwrap();
        //     let mut cx = window_handler.context(&mut window);
        //     editor.init(&mut cx);

        //     window_handler
        // });

        return Box::new(EditorHandle { window_handle: webview_rc });
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
    #[expect(unused)]
    window_handle: Rc<RefCell<Option<WebView>>>,
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
    fn context<'a, 'b>(&'a self, window: &'a mut Window<'b>) -> Context<'a, 'b> {
        Context { handler: self, window }
    }

    fn resize(&self, window: &mut baseview::Window, width: f64, height: f64) -> bool {
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
    fn on_frame(&mut self, window: &mut baseview::Window) {
        if let Err(err) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut editor = self.init.editor.lock().unwrap();
            let mut cx = self.context(window);

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

    fn on_event(&mut self, window: &mut baseview::Window, event: Event) -> EventStatus {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut editor = self.init.editor.lock().unwrap();
            let mut cx = self.context(window);

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
