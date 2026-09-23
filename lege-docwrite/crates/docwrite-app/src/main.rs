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
    announced: bool,
}

impl EditorApp {
    fn new() -> Self {
        Self {
            editor: docwrite_app::Editor::new(),
            announced: false,
        }
    }
}

impl pixelkit_shell::PixelApp for EditorApp {
    fn render(
        &mut self,
        buffer: &mut pixelkit_raster::WindowBuffer,
        _scale: pixelkit_shell::Scale,
    ) {
        self.editor.paint(buffer);
        if self.announced {
            return;
        }
        self.announced = true;
        let (desk, page, ink) = docwrite_app::Editor::census(&buffer.pixels);
        eprintln!(
            "paged surface: {} pages, desk pixels {desk}, page pixels {page}, ink pixels {ink}",
            self.editor.pages_painted()
        );
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
        if event.pressed {
            match event.key {
                pixelkit_shell::KeyInput::Left if event.modifiers.shift => {
                    self.editor.extend_left();
                    return;
                }
                pixelkit_shell::KeyInput::Right if event.modifiers.shift => {
                    self.editor.extend_right();
                    return;
                }
                pixelkit_shell::KeyInput::Left => {
                    self.editor.move_left();
                    return;
                }
                pixelkit_shell::KeyInput::Right => {
                    self.editor.move_right();
                    return;
                }
                _ => {}
            }
        }
        if event.pressed && !event.text.is_empty() {
            self.editor.type_text(&event.text);
        }
    }

    fn on_cursor(&mut self, x: f32, y: f32) {
        self.editor.hover(x, y);
    }

    fn on_mouse(&mut self, button: pixelkit_shell::MouseButton, pressed: bool) {
        if button == pixelkit_shell::MouseButton::Left {
            self.editor.pointer(pressed);
        }
    }

    fn on_ime(&mut self, ime: &pixelkit_shell::ImeEvent) {
        match ime {
            pixelkit_shell::ImeEvent::Commit(text) => self.editor.commit_ime(text),
            pixelkit_shell::ImeEvent::Preedit(text, _) => self.editor.set_preedit(text),
            pixelkit_shell::ImeEvent::Disabled => self.editor.set_preedit(""),
            pixelkit_shell::ImeEvent::Enabled => {}
        }
    }

    fn ime_cursor_area(&self) -> Option<pixelkit_shell::ImeCursorArea> {
        self.editor
            .caret_area()
            .map(|caret| pixelkit_shell::ImeCursorArea {
                x: caret.x,
                y: caret.y,
                width: caret.width,
                height: caret.height,
            })
    }

    fn on_scroll_event(&mut self, event: &pixelkit_shell::ScrollEvent) {
        let phase = match event.phase {
            pixelkit_shell::GesturePhase::Started => docwrite_app::Phase::Started,
            pixelkit_shell::GesturePhase::Moved => docwrite_app::Phase::Moved,
            pixelkit_shell::GesturePhase::Ended => docwrite_app::Phase::Ended,
            pixelkit_shell::GesturePhase::Cancelled => docwrite_app::Phase::Cancelled,
        };
        self.editor
            .scroll_gesture(-(event.pixel_dy as f64) / 400.0, phase);
    }
}
