use std::{path::PathBuf, sync::Arc};

use nih_plug::prelude::*;
use nih_plug_webview::{EditorHandler, WebViewConfig, WebviewEditor, WebviewSource, WebviewState};

fn main() {
    nih_plug::nih_export_standalone::<SimplePlugin>();
}

struct SimpleEditor {}

impl SimpleEditor {
    pub fn new(state: &Arc<WebviewState>) -> Option<Box<dyn Editor>> {
        let config = WebViewConfig {
            title: "Simple Plugin".to_string(),
            source: WebviewSource::HTML(include_str!("simple.html").to_string()),
            workdir: PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/target/webview-workdir")),
        };

        let editor = WebviewEditor::new(SimpleEditor {}, state, config);

        Some(Box::new(editor))
    }
}

impl EditorHandler for SimpleEditor {
    type EditorRx = String;
    type HandlerRx = String;

    fn init(&mut self, _: &mut nih_plug_webview::Context<Self>) {}

    fn on_frame(&mut self, _: &mut nih_plug_webview::Context<Self>) {}

    fn on_message(&mut self, cx: &mut nih_plug_webview::Context<Self>, message: Self::HandlerRx) {
        println!("Received message: {}", message);
        cx.send_message("Hello from Rust!".to_string());
    }
}

///////////////////////////////////////////////////////////////////////////////

#[derive(Params)]
pub struct SimpleParams {
    editor_state: Arc<WebviewState>,
}

pub struct SimplePlugin {
    params: Arc<SimpleParams>,
}

impl Default for SimplePlugin {
    fn default() -> Self {
        Self {
            params: Arc::new(SimpleParams {
                editor_state: Arc::new(WebviewState::new(350.0, 250.0)),
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
        SimpleEditor::new(&self.params.editor_state)
    }
}
