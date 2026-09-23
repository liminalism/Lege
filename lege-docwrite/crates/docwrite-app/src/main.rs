//! `lege-docwrite`: the page-native book editor.
//!
//! The window hosts the same pagination the tests drive. Without a display
//! the process reports that and exits; it does not pretend to have drawn.

fn main() {
    match run() {
        Ok(()) => {}
        Err(err) => {
            eprintln!("lege-docwrite: {err}");
            std::process::exit(if err.is_display() { 2 } else { 1 });
        }
    }
}

struct RunError {
    display: bool,
    message: String,
}

impl RunError {
    fn display(message: impl Into<String>) -> Self {
        Self {
            display: true,
            message: message.into(),
        }
    }

    fn is_display(&self) -> bool {
        self.display
    }
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.display {
            write!(f, "display unavailable: {}", self.message)
        } else {
            write!(f, "{}", self.message)
        }
    }
}

fn run() -> Result<(), RunError> {
    if docwrite_app::display_unavailable() {
        return Err(RunError::display(
            "no display; the paged surface was not opened",
        ));
    }
    let app = EditorApp::new();
    let config = pixelkit_shell::WindowConfig::new("lege-docwrite", 1100.0, 800.0);
    pixelkit_shell::run_app(app, config).map_err(|err| {
        let message = err.to_string();
        if message.to_ascii_lowercase().contains("display")
            || message.to_ascii_lowercase().contains("window")
            || message.to_ascii_lowercase().contains("connection")
        {
            RunError::display(message)
        } else {
            RunError {
                display: false,
                message,
            }
        }
    })
}

struct EditorApp {
    editor: docwrite_app::Editor,
}

impl EditorApp {
    fn new() -> Self {
        Self {
            editor: docwrite_app::Editor::new(),
        }
    }
}

impl pixelkit_shell::PixelApp for EditorApp {
    fn render(&mut self, buffer: &mut pixelkit_raster::WindowBuffer, _scale: pixelkit_shell::Scale) {
        for pixel in buffer.pixels.iter_mut() {
            *pixel = 0x00E6_E1D6;
        }
        let width = buffer.width as i32;
        let height = buffer.height as i32;
        let page_h = height * 7 / 10;
        let page_w = page_h * 2 / 3;
        let gap = 24;
        let scroll = self.editor.pager().scroll();
        let origin = (height / 2) - ((scroll.fract() * page_h as f64) as i32);
        for slot in -1..4 {
            let top = origin + slot * (page_h + gap);
            let left = (width - page_w) / 2;
            fill_rect(buffer, left, top, page_w, page_h, 0x00FF_FBF4);
        }
    }

    fn on_key(&mut self, key: &pixelkit_shell::KeyInput) {
        match key {
            pixelkit_shell::KeyInput::PageDown => self.editor.page_down(),
            pixelkit_shell::KeyInput::PageUp => self.editor.page_up(),
            pixelkit_shell::KeyInput::Backspace => self.editor.backspace(),
            pixelkit_shell::KeyInput::Enter => self.editor.type_text("\n"),
            pixelkit_shell::KeyInput::Character(_) => {}
            _ => {}
        }
    }

    fn on_key_event(&mut self, event: &pixelkit_shell::KeyEvent) {
        if event.pressed && !event.text.is_empty() {
            self.editor.type_text(&event.text);
        }
    }

    fn on_scroll_event(&mut self, event: &pixelkit_shell::ScrollEvent) {
        let phase = match event.phase {
            pixelkit_shell::GesturePhase::Started => docwrite_app::Phase::Started,
            pixelkit_shell::GesturePhase::Moved => docwrite_app::Phase::Moved,
            pixelkit_shell::GesturePhase::Ended => docwrite_app::Phase::Ended,
            pixelkit_shell::GesturePhase::Cancelled => docwrite_app::Phase::Cancelled,
        };
        self.editor.scroll_gesture(-(event.pixel_dy as f64) / 400.0, phase);
    }
}

fn fill_rect(buffer: &mut pixelkit_raster::WindowBuffer, x: i32, y: i32, w: i32, h: i32, color: u32) {
    let width = buffer.width as i32;
    let height = buffer.height as i32;
    for row in y.max(0)..(y + h).min(height) {
        for col in x.max(0)..(x + w).min(width) {
            buffer.pixels[(row as u32 * buffer.width + col as u32) as usize] = color;
        }
    }
}
