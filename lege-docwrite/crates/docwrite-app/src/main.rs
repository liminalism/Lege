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
    pager: docwrite_app::Pager,
}

impl EditorApp {
    fn new() -> Self {
        Self {
            pager: docwrite_app::Pager::new(1, false),
        }
    }
}

impl pixelkit_shell::PixelApp for EditorApp {
    fn render(&mut self, buffer: &mut pixelkit_raster::WindowBuffer, scale: pixelkit_shell::Scale) {
        let _ = scale;
        for pixel in buffer.pixels.iter_mut() {
            *pixel = 0x00F4_F1EA;
        }
        let _ = self.pager.scroll();
    }
}
