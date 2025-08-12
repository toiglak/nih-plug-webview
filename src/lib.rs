use std::{
    cell::RefCell,
    path::PathBuf,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use crossbeam::atomic::AtomicCell;
use nih_plug::{
    log,
    params::persist::PersistentField,
    prelude::{Editor, GuiContext, ParentWindowHandle},
};
use serde::{Deserialize, Serialize};
use wry::{
    dpi::{LogicalPosition, LogicalSize, Position},
    http::Request,
    Rect, WebContext, WebView, WebViewBuilder,
};

use self::reparent::TempWindow;
use self::safe_cell::SendCell;

mod handler;
mod reparent;
mod safe_cell;
mod util;

pub use handler::*;
pub use wry;

#[derive(Debug, Clone)]
pub enum WebViewSource {
    /// Loads a web page from the given URL.
    ///
    /// For example `https://example.com`.
    URL(String),
    /// Loads a web page from the given HTML content.
    ///
    /// For example `<h1>Hello, world!</h1>`.
    HTML(String),
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
    /// WebViewEditor::new_with_webview("my-plugin", source, params, handler, |webview| {
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

pub struct WebViewConfig {
    /// The title of the window when running as a standalone application.
    pub title: String,
    /// The source for the site to be loaded in the webview.
    pub source: WebViewSource,
    /// The directory where webview will store its working data.
    pub workdir: PathBuf,
}

/// `nih_plug_webview`'s state that should be persisted between sessions (like window size).
///
/// Add it as a persistent parameter to your plugin's state.
#[derive(Debug, Serialize, Deserialize)]
pub struct WebViewState {
    /// The window's size in logical pixels before applying `scale_factor`.
    #[serde(with = "nih_plug::params::persist::serialize_atomic_cell")]
    window_size: AtomicCell<(f64, f64)>,
    #[serde(skip)]
    resize_pending: AtomicCell<bool>,
    // NOTE: this code has been adapted from nih-plug-vizia:
    // https://github.com/robbert-vdh/nih-plug/blob/master/nih_plug_vizia/
    /// Whether the editor's window is currently open.
    #[serde(skip)]
    open: AtomicBool,
}

impl WebViewState {
    /// Initialize the GUI's state. The window size is in logical pixels, so
    /// before it is multiplied by the DPI scaling factor.
    pub fn new(width: f64, height: f64) -> WebViewState {
        WebViewState {
            window_size: AtomicCell::new((width, height)),
            resize_pending: AtomicCell::new(false),
            open: AtomicBool::new(false),
        }
    }

    /// Returns a `(width, height)` pair for the current size of the GUI in
    /// logical pixels.
    pub fn window_size(&self) -> (f64, f64) {
        self.window_size.load()
    }

    /// Whether the GUI is currently visible.
    // Called `is_open()` instead of `open()` to avoid the ambiguity.
    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::Acquire)
    }
}

impl<'a> PersistentField<'a, WebViewState> for Arc<WebViewState> {
    fn set(&self, new_value: WebViewState) {
        self.window_size.store(new_value.window_size.load());
    }

    fn map<F, R>(&self, f: F) -> R
    where
        F: Fn(&WebViewState) -> R,
    {
        f(self)
    }
}

/// A webview-based plugin editor.
pub struct WebViewEditor {
    webview_state: Arc<WebViewState>,
    config: SendCell<Config>,
    instance: SendCell<Rc<RefCell<Option<WebViewInstance>>>>,
}

impl WebViewEditor {
    pub fn new(
        editor: impl EditorHandler,
        state: &Arc<WebViewState>,
        config: WebViewConfig,
    ) -> WebViewEditor {
        Self::new_with_webview(editor, state, config, |webview| webview)
    }

    /// Creates a new `WebViewEditor` with the callback which allows you to configure
    /// [`WebViewBuilder`](wry::WebViewBuilder) just the way you want it.
    ///
    /// **Note:** Some settings are overridden to initialize this library. Refer to the
    /// [`setup_webview`] implementation for details on which settings are affected.
    pub fn new_with_webview(
        editor: impl EditorHandler,
        state: &Arc<WebViewState>,
        config: WebViewConfig,
        f: impl Fn(WebViewBuilder) -> WebViewBuilder + Send + Sync + 'static,
    ) -> WebViewEditor {
        WebViewEditor {
            webview_state: state.clone(),
            config: SendCell::new(Config {
                title: config.title,
                source: config.source,
                workdir: config.workdir,
                state: state.clone(),
                editor: Rc::new(RefCell::new(editor)),
                setup_webview_fn: Rc::new(f),
            }),
            instance: SendCell::new(Rc::new(RefCell::new(None))),
        }
    }

    fn create_context(&self) -> Option<Context> {
        let state = self.config.state.clone();
        let webview = self.instance.borrow().as_ref()?.webview.clone();
        let gui_context = self.instance.borrow().as_ref()?.gui_context.clone();
        Some(Context {
            state,
            webview,
            gui_context,
        })
    }
}

impl Editor for WebViewEditor {
    // MACOS: When running as a standalone application, nih_plug relies on
    // `baseview` to provide the plugin with a `ParentWindowHandle`. Due to a bug,
    // the exposed `ns_view` lacks a parent `ns_window`. This causes `wry` to panic
    // during `build_as_child`, as it requires a parent `ns_window` to be present.
    fn spawn(
        &self,
        window: ParentWindowHandle,
        gui_context: Arc<dyn GuiContext>,
    ) -> Box<dyn std::any::Any + Send> {
        log::debug!("Spawning editor");

        self.webview_state.open.store(true, Ordering::Release);

        // If the webview was already created, reuse it.
        if let Some(handle) = self.instance.borrow().as_ref() {
            if let Some(_) = reparent::reparent_webview(&handle.webview, window) {
                log::debug!("Reusing existing webview instance");
                return Box::new(EditorHandle {
                    instance: SendCell::new(self.instance.clone()),
                });
            }
        }

        log::info!("Creating new webview instance");
        log::debug!("Workdir: {:?}", self.config.workdir);

        let window = util::into_window_handle(window);

        let mut web_context = WebContext::new(Some(self.config.workdir.clone()));

        let webview_builder = setup_webview(
            &self.config,
            &mut web_context,
            gui_context.clone(),
            self.instance.clone(),
            self.config.state.window_size.load(),
        );

        let webview = webview_builder
            .build_as_child(&window)
            .expect("failed to construct webview");

        log::debug!("Webview constructed");

        self.instance.replace(Some(WebViewInstance {
            webview_state: self.webview_state.clone(),
            webview: Rc::new(webview),
            web_context: Rc::new(web_context),
            gui_context: Arc::clone(&gui_context),
            temp_window: TempWindow::new(),
        }));

        log::info!("Editor spawned successfully");

        return Box::new(EditorHandle {
            instance: SendCell::new(self.instance.clone()),
        });
    }

    fn size(&self) -> (u32, u32) {
        let (a, b) = self.config.state.window_size.load();
        (a as u32, b as u32)
    }

    fn set_scale_factor(&self, factor: f32) -> bool {
        // NOTE: this is more functionality ported from nih-plug-vizia.
        // this might not be necessary for us.

        // If the editor is currently open then the host must not change the current HiDPI scale as
        // we don't have a way to handle that. Ableton Live does this.
        if self.webview_state.is_open() {
            return false;
        }

        log::info!("set_scale_factor: {}", factor);

        // Update window size (and webview bounds) to match new scaled resolution.
        //
        // FIXME: Normally, we'd simply call cx.resize_window() here, but due to a
        // bug in nih-plug, doing so causes a deadlock. To work around this, we
        // defer that call to on_frame.
        self.config.state.resize_pending.store(true);

        true
    }

    fn param_values_changed(&self) {
        log::debug!("param_values_changed");
        let mut cx = self.create_context().unwrap();
        self.config.editor.borrow_mut().on_params_changed(&mut cx);
    }

    fn param_value_changed(&self, id: &str, normalized_value: f32) {
        log::debug!(
            "param_value_changed: '{}' value changed to {}",
            id,
            normalized_value
        );
        let mut cx = self.create_context().unwrap();
        self.config
            .editor
            .borrow_mut()
            .on_param_value_changed(&mut cx, id, normalized_value);
    }

    fn param_modulation_changed(&self, id: &str, modulation_offset: f32) {
        log::debug!(
            "param_modulation_changed: '{}' modulation changed by {}",
            id,
            modulation_offset
        );
        let mut cx = self.create_context().unwrap();
        self.config
            .editor
            .borrow_mut()
            .on_param_modulation_changed(&mut cx, id, modulation_offset);
    }
}

struct Config {
    #[expect(unused)]
    title: String,
    source: WebViewSource,
    workdir: PathBuf,
    state: Arc<WebViewState>,
    editor: Rc<RefCell<dyn EditorHandler>>,
    setup_webview_fn: Rc<dyn Fn(WebViewBuilder) -> WebViewBuilder + Send + Sync + 'static>,
}

fn setup_webview<'a>(
    config: &Config,
    web_context: &'a mut WebContext,
    gui_context: Arc<dyn GuiContext>,
    instance: Rc<RefCell<Option<WebViewInstance>>>,
    (width, height): (f64, f64),
) -> WebViewBuilder<'a> {
    log::debug!("Setting up webview with source: {:?}", config.source);
    let mut webview_builder = WebViewBuilder::with_web_context(web_context);

    // Apply user configuration.
    webview_builder = (*config.setup_webview_fn)(webview_builder);

    let ipc_handler = {
        let instance = Rc::downgrade(&instance);
        let state = config.state.clone();
        let editor = config.editor.clone();
        let context = gui_context.clone();
        move |request: Request<String>| {
            let instance = instance.upgrade().unwrap();
            let webview = instance.borrow().as_ref().unwrap().webview.clone();
            ipc_handler(
                webview,
                state.clone(),
                editor.clone(),
                context.clone(),
                request,
            );
        }
    };

    let webview_builder = webview_builder
        .with_focused(true)
        .with_accept_first_mouse(true)
        .with_bounds(Rect {
            position: Position::Logical(LogicalPosition { x: 0.0, y: 0.0 }),
            size: wry::dpi::Size::Logical(LogicalSize { width, height }),
        })
        .with_initialization_script(include_str!("lib.js"))
        .with_ipc_handler(ipc_handler);

    let webview_builder = match config.source.clone() {
        WebViewSource::URL(url) => webview_builder.with_url(url.as_str()),
        WebViewSource::HTML(html) => webview_builder.with_html(html),
        WebViewSource::CustomProtocol { url, protocol } => {
            webview_builder.with_url(format!("{protocol}://localhost/{url}").as_str())
        }
    };

    webview_builder
}

fn ipc_handler(
    webview: Rc<WebView>,
    state: Arc<WebViewState>,
    editor: Rc<RefCell<dyn EditorHandler>>,
    gui_context: Arc<dyn GuiContext>,
    request: Request<String>,
) {
    if let Err(err) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut cx = Context {
            state: state.clone(),
            webview: webview.clone(),
            gui_context: gui_context.clone(),
        };
        let message = request.into_body();
        handle_message(editor, &mut cx, message);
    })) {
        // NOTE: We catch panic here, because `baseview` doesn't run from the "main entry
        // point", instead it schedules this handler as a task on the main thread. For
        // some reason, on macos if you panic from a task the process will be forever
        // stuck and you won't be able terminate it until you log out.
        log::error!("IPC handler panicked: {:?}", err);
        std::process::exit(1);
    }
}

fn handle_message(editor: Rc<RefCell<dyn EditorHandler>>, cx: &mut Context, message: String) {
    let mut editor = editor.borrow_mut();

    if message.starts_with("frame") {
        // Handle set_scale_factor
        if cx.state.resize_pending.swap(false) {
            let (width, height) = cx.state.window_size();
            log::debug!("Executing deferred resize to {}x{}", width, height);
            // nih-plug already takes care of setting the window size to
            // Editor::size() multiplied by the scale factor, so we just need to
            // pass in the logical size.
            cx.resize_window(width, height);
        }

        editor.on_frame(cx);
    } else if message.starts_with("text,") {
        let text_message = message.trim_start_matches("text,");
        log::debug!("Received message from webview");
        editor.on_message(cx, text_message.to_string());
    } else {
        log::warn!("Unexpected ipc message type: {}", message);
    }
}

struct WebViewInstance {
    webview_state: Arc<WebViewState>,
    webview: Rc<WebView>,
    #[expect(unused)]
    web_context: Rc<WebContext>,
    gui_context: Arc<dyn GuiContext>,
    temp_window: TempWindow,
}

/// A handle to the editor window, returned from [`Editor::spawn`]. Host will
/// call [`drop`] on it when the window is supposed to be closed.
struct EditorHandle {
    instance: SendCell<Rc<RefCell<Option<WebViewInstance>>>>,
}

impl Drop for EditorHandle {
    fn drop(&mut self) {
        log::debug!("Editor handle dropped");
        self.instance.borrow_mut().as_mut().map(|instance| {
            instance.webview_state.open.store(false, Ordering::Release);
            // Reparent the webview to a temporary window, so that it can be reused
            // later. On MacOS this is a NOOP.
            instance.temp_window.reparent_from(&instance.webview);
        });
    }
}
