use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
    sync::Arc,
};

use crossbeam::atomic::AtomicCell;
use nih_plug::{
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
    size: AtomicCell<(f64, f64)>,
}

impl WebViewState {
    /// Initialize the GUI's state. The window size is in logical pixels, so
    /// before it is multiplied by the DPI scaling factor.
    pub fn new(width: f64, height: f64) -> WebViewState {
        WebViewState {
            size: AtomicCell::new((width, height)),
        }
    }

    /// Returns a `(width, height)` pair for the current size of the GUI in
    /// logical pixels.
    pub fn size(&self) -> (f64, f64) {
        self.size.load()
    }
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

/// A webview-based plugin editor.
pub struct WebViewEditor {
    config: SendCell<Config>,
    params_changed: SendCell<Rc<Cell<bool>>>,
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
            config: SendCell::new(Config {
                title: config.title,
                source: config.source,
                workdir: config.workdir,
                state: state.clone(),
                editor: Rc::new(RefCell::new(editor)),
                setup_webview_fn: Rc::new(f),
            }),
            params_changed: SendCell::new(Rc::new(Cell::new(false))),
            instance: SendCell::new(Rc::new(RefCell::new(None))),
        }
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
        // If the webview was already created, reuse it.
        if let Some(handle) = self.instance.borrow().as_ref() {
            if let Some(_) = reparent::reparent_webview(&handle.webview, window) {
                return Box::new(EditorHandle {
                    instance: SendCell::new(self.instance.clone()),
                });
            }
        }

        //// Create webview

        let window = util::into_window_handle(window);

        let mut web_context = WebContext::new(Some(self.config.workdir.clone()));

        let webview_builder = setup_webview(
            &self.config,
            gui_context,
            &mut web_context,
            self.params_changed.clone(),
            self.instance.clone(),
            self.config.state.size.load(),
        );

        // We use `build_as_child` over `build` because `build_as_child` knows that
        // it runs as a child and so it knows not to consume all keyboard events.
        let webview = webview_builder
            .build_as_child(&window)
            .expect("failed to construct webview");

        self.instance.replace(Some(WebViewInstance {
            webview: Rc::new(webview),
            web_context: Rc::new(web_context),
            temp_window: TempWindow::new(),
        }));

        return Box::new(EditorHandle {
            instance: SendCell::new(self.instance.clone()),
        });
    }

    fn size(&self) -> (u32, u32) {
        let (a, b) = self.config.state.size.load();
        (a as u32, b as u32)
    }

    fn set_scale_factor(&self, _factor: f32) -> bool {
        return false; // FIXME, or does wry handle it?
    }

    fn param_values_changed(&self) {
        self.params_changed.replace(true);
    }

    fn param_value_changed(&self, _id: &str, _normalized_value: f32) {
        self.params_changed.replace(true);
    }

    fn param_modulation_changed(&self, _id: &str, _modulation_offset: f32) {
        self.params_changed.replace(true);
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
    gui_context: Arc<dyn GuiContext>,
    web_context: &'a mut WebContext,
    params_changed: Rc<Cell<bool>>,
    instance: Rc<RefCell<Option<WebViewInstance>>>,
    (width, height): (f64, f64),
) -> WebViewBuilder<'a> {
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
                params_changed.clone(),
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
    context: Arc<dyn GuiContext>,
    params_changed: Rc<Cell<bool>>,
    request: Request<String>,
) {
    let message = request.into_body();

    let send_message = |message: String| util::send_message(&webview, message);

    if message.starts_with("frame") {
        if let Err(err) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut cx = Context {
                state: state.clone(),
                webview: webview.clone(),
                context: context.clone(),
                params_changed: params_changed.clone(),
            };

            editor.borrow_mut().on_frame(&mut cx);
        })) {
            // NOTE: We catch panic here, because `baseview` doesn't run from the "main entry
            // point", instead it schedules this handler as a task on the main thread. For
            // some reason, on macos if you panic from a task the process will be forever
            // stuck and you won't be able terminate it until you log out.
            eprintln!("{:?}", err);
            std::process::exit(1);
        }
    } else {
        let mut editor = editor.borrow_mut();
        editor.on_message(&send_message, message)
    }
}

struct WebViewInstance {
    webview: Rc<WebView>,
    #[expect(unused)]
    web_context: Rc<WebContext>,
    temp_window: TempWindow,
}

/// A handle to the editor window, returned from [`Editor::spawn`]. Host will
/// call [`drop`] on it when the window is supposed to be closed.
struct EditorHandle {
    instance: SendCell<Rc<RefCell<Option<WebViewInstance>>>>,
}

impl Drop for EditorHandle {
    fn drop(&mut self) {
        self.instance.borrow_mut().as_mut().map(|instance| {
            // Reparent the webview to a temporary window, so that it can be reused
            // later. On MacOS this is a NOOP.
            instance.temp_window.reparent_from(&instance.webview);
        });
    }
}
