use std::{borrow::Cow, path::PathBuf, sync::Arc};

use nih_plug::prelude::*;
use nih_plug_webview::{
    Context, EditorHandler, WebViewConfig, WebViewEditor, WebViewSource, WebViewState,
};
use wry::http::Response;

fn main() {
    // FIXME: `nih_export_standalone` doesn't work on macos due to a bug in
    // nih-plug's window creation logic. Do note that it works fine when running as
    // a plugin inside a DAW.
    nih_plug::nih_export_standalone::<SimplePlugin>();
}

struct SimpleEditor {}

impl EditorHandler for SimpleEditor {
    fn on_frame(&mut self, _cx: &mut Context) {
        // This is where you would handle side effects.
    }

    fn on_message(&mut self, cx: &mut Context, message: String) {
        println!("Received message: {:?}", message);
        cx.send_message("Hello from Rust!".to_string());
    }

    fn on_params_changed(&mut self, _cx: &mut Context) {
        // This is where you would react to parameter changes and update UI.
    }
}

impl SimpleEditor {
    pub fn new(state: &Arc<WebViewState>) -> Option<Box<dyn Editor>> {
        let protocol = "nih".to_string();

        let config = WebViewConfig {
            // The title of the window when running as a standalone application.
            title: "Simple Plugin".to_string(),
            // Your source can be URL, an HTML string, or a custom protocol. Custom
            // protocol allows you to serve files yourself without relying on a web
            // server (see `with_custom_protocol` handler below).
            source: WebViewSource::CustomProtocol {
                protocol: protocol.clone(),
                url: "index.html".to_string(),
            },
            // Ideally, the workdir should be a temporary directory within your
            // application's data directory. For simplicity, we're using a fixed
            // path under cargo's target/ directory.
            workdir: PathBuf::from(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/target/webview-workdir"
            )),
        };

        let editor = WebViewEditor::new_with_webview(
            SimpleEditor {},
            state,
            config,
            move |w: wry::WebViewBuilder| {
                w.with_custom_protocol(protocol.clone(), |_id, req| match req.uri().path() {
                    "/index.html" => {
                        let body = Cow::Borrowed(include_bytes!("simple.html") as &[u8]);
                        Response::builder().body(body).unwrap()
                    }
                    "/simple.js" => {
                        let body = Cow::Borrowed(include_bytes!("simple.js") as &[u8]);
                        Response::builder().body(body).unwrap()
                    }
                    _ => unreachable!(),
                })
            },
        );

        Some(Box::new(editor))
    }
}

#[derive(Params)]
pub struct SimpleParams {
    // Apart from regular parameters, you should also persist the webview state so
    // that it can be restored when the plugin is reloaded. For example, size of
    // the window.
    #[persist = "webview_state"]
    webview_state: Arc<WebViewState>,
}

pub struct SimplePlugin {
    params: Arc<SimpleParams>,
}

impl Default for SimplePlugin {
    fn default() -> Self {
        Self {
            params: Arc::new(SimpleParams {
                webview_state: Arc::new(WebViewState::new(350.0, 250.0)),
            }),
        }
    }
}

impl Plugin for SimplePlugin {
    const NAME: &'static str = "simple-plugin";
    const VENDOR: &'static str = "nih-plug-webview";
    const URL: &'static str = env!("CARGO_PKG_HOMEPAGE");
    const EMAIL: &'static str = "";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[AudioIOLayout {
        main_input_channels: NonZeroU32::new(2),
        main_output_channels: NonZeroU32::new(2),
        ..AudioIOLayout::const_default()
    }];

    const MIDI_INPUT: MidiConfig = MidiConfig::Basic;
    const SAMPLE_ACCURATE_AUTOMATION: bool = true;

    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn process(
        &mut self,
        _: &mut Buffer,
        _: &mut AuxiliaryBuffers,
        _: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        ProcessStatus::Normal
    }

    fn editor(&mut self, _e: AsyncExecutor<Self>) -> Option<Box<dyn Editor>> {
        SimpleEditor::new(&self.params.webview_state)
    }
}
