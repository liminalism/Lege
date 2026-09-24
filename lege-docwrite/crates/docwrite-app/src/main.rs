//! `lege-docwrite`: the page-native book editor.
//!
//! ```text
//! lege-docwrite [--fullscreen] [BOOK.legebook]  open or start a book
//! lege-docwrite export BOOK.legebook OUT     write OUT as .pdf or .md
//! ```
//!
//! Without a display the window is not opened; the process says so and
//! exits. It does not pretend to have drawn.

use std::path::{Path, PathBuf};
use std::time::Duration;

use pixelkit_shell::clipboard::Clipboard;
use pixelkit_shell::{KeyEvent, KeyInput};

/// Typing has to pause this long before autosave writes the bundle.
const AUTOSAVE_IDLE: Duration = Duration::from_millis(1500);

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

    fn other(message: impl Into<String>) -> Self {
        Self {
            display: false,
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
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let fullscreen = args.iter().any(|arg| arg == "--fullscreen");
    args.retain(|arg| arg != "--fullscreen");
    match args.as_slice() {
        [command, book, out] if command == "export" => export(Path::new(book), Path::new(out)),
        [] => open(PathBuf::from("Untitled.legebook"), fullscreen),
        [book] if !book.starts_with('-') => open(PathBuf::from(book), fullscreen),
        _ => Err(RunError::other(
            "usage: lege-docwrite [--fullscreen] [BOOK.legebook] | lege-docwrite export BOOK.legebook OUT.pdf|OUT.md",
        )),
    }
}

fn export(book: &Path, out: &Path) -> Result<(), RunError> {
    if !docwrite_model::Book::is_bundle(book) {
        return Err(RunError::other(format!("{} is not a book", book.display())));
    }
    let editor = docwrite_app::Editor::open_or_create(book).map_err(RunError::other)?;
    match out.extension().and_then(|ext| ext.to_str()) {
        Some("pdf") => editor.export_pdf_to(out),
        Some("md") => editor.export_markdown_to(out),
        _ => Err("the output must end in .pdf or .md".to_string()),
    }
    .map_err(RunError::other)?;
    println!("wrote {}", out.display());
    Ok(())
}

fn open(book: PathBuf, fullscreen: bool) -> Result<(), RunError> {
    if docwrite_app::display_unavailable() {
        return Err(RunError::display(
            "no display; the paged surface was not opened",
        ));
    }
    let mut editor = docwrite_app::Editor::open_or_create(&book).map_err(RunError::other)?;
    editor.set_fullscreen(fullscreen);
    let title = format!(
        "{} — lege-docwrite",
        book.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".into())
    );
    let app = EditorApp {
        editor,
        clipboard: Clipboard::new(),
        announced: false,
        status: None,
    };
    let config = pixelkit_shell::WindowConfig::new(&title, 1100.0, 800.0);
    pixelkit_shell::run_app(app, config).map_err(|err| {
        let message = err.to_string();
        let lower = message.to_ascii_lowercase();
        if lower.contains("display") || lower.contains("window") || lower.contains("connection") {
            RunError::display(message)
        } else {
            RunError::other(message)
        }
    })
}

struct EditorApp {
    editor: docwrite_app::Editor,
    clipboard: Clipboard,
    announced: bool,
    /// The last save or export outcome, reported once on stderr.
    status: Option<String>,
}

impl EditorApp {
    fn report(&mut self, outcome: Result<String, String>) {
        let line = match outcome {
            Ok(done) => done,
            Err(err) => format!("error: {err}"),
        };
        eprintln!("lege-docwrite: {line}");
        self.status = Some(line);
    }

    fn export_next_to_bundle(&mut self, extension: &str) {
        let Some(bundle) = self.editor.bundle().map(Path::to_path_buf) else {
            self.report(Err("no bundle to export next to".into()));
            return;
        };
        let out = bundle.with_extension(extension);
        let result = if extension == "pdf" {
            self.editor.export_pdf_to(&out)
        } else {
            self.editor.export_markdown_to(&out)
        };
        self.report(result.map(|()| format!("exported {}", out.display())));
    }

    /// Commands bound to the platform accelerator (Cmd on macOS, Ctrl elsewhere).
    fn accelerator(&mut self, event: &KeyEvent) -> bool {
        let shift = event.modifiers.shift;
        let KeyInput::Character(ch) = event.key else {
            return false;
        };
        match ch.to_ascii_lowercase() {
            'z' if shift => self.editor.redo(),
            'z' => self.editor.undo(),
            'y' => self.editor.redo(),
            'c' => {
                if let Some(text) = self.editor.copy_text() {
                    self.clipboard.set_text(text);
                }
            }
            'x' => {
                if let Some(text) = self.editor.cut_text() {
                    self.clipboard.set_text(text);
                }
            }
            'v' => {
                if let Some(text) = self.clipboard.text() {
                    self.editor.paste_text(&text);
                }
            }
            's' => {
                let outcome = self.editor.save_now().map(|()| "saved".to_string());
                self.report(outcome);
            }
            'f' if shift => self.editor.toggle_fullscreen(),
            '\\' => self.editor.toggle_sidebar(),
            'e' if shift => self.export_next_to_bundle("md"),
            'e' => self.export_next_to_bundle("pdf"),
            _ => return false,
        }
        true
    }
}

impl pixelkit_shell::PixelApp for EditorApp {
    fn render(&mut self, buffer: &mut pixelkit_raster::WindowBuffer, scale: pixelkit_shell::Scale) {
        self.editor.set_ui_scale(scale.0);
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

    fn poll_fullscreen(&mut self) -> Option<bool> {
        self.editor.poll_fullscreen()
    }

    fn cursor_shape(&self) -> pixelkit_shell::CursorShape {
        match self.editor.pointer_shape() {
            docwrite_app::PointerShape::Arrow => pixelkit_shell::CursorShape::Default,
            docwrite_app::PointerShape::Text => pixelkit_shell::CursorShape::Text,
            docwrite_app::PointerShape::Hand => pixelkit_shell::CursorShape::Pointer,
            docwrite_app::PointerShape::ResizeColumn => {
                pixelkit_shell::CursorShape::ResizeHorizontal
            }
        }
    }

    fn animation_interval(&self) -> Option<Duration> {
        // Only while there is something to save: tick() starts the save
        // once typing has paused.
        self.editor.is_dirty().then_some(Duration::from_millis(500))
    }

    fn tick(&mut self) {
        self.editor.autosave_if_idle(AUTOSAVE_IDLE);
        if let Some(err) = self.editor.take_save_error() {
            self.report(Err(format!("autosave failed: {err}")));
        }
    }

    fn on_exit(&mut self) {
        if let Err(err) = self.editor.save_now() {
            eprintln!("lege-docwrite: could not save on exit: {err}");
        }
    }

    fn on_key_event(&mut self, event: &KeyEvent) {
        if !event.pressed {
            return;
        }
        let modifiers = event.modifiers;
        if modifiers.accel() && self.accelerator(event) {
            return;
        }
        let shift = modifiers.shift;
        // Word motion: Option on macOS, Ctrl elsewhere.
        let word = if cfg!(target_os = "macos") {
            modifiers.alt
        } else {
            modifiers.control
        };
        match event.key {
            KeyInput::Function(11) => self.editor.toggle_fullscreen(),
            KeyInput::Escape if self.editor.is_fullscreen() => self.editor.set_fullscreen(false),
            KeyInput::Left if word => self.editor.move_word(false, shift),
            KeyInput::Right if word => self.editor.move_word(true, shift),
            KeyInput::Left if modifiers.logo => self.editor.move_home(shift),
            KeyInput::Right if modifiers.logo => self.editor.move_end(shift),
            KeyInput::Left if shift => self.editor.extend_left(),
            KeyInput::Right if shift => self.editor.extend_right(),
            KeyInput::Left => self.editor.move_left(),
            KeyInput::Right => self.editor.move_right(),
            KeyInput::Up => self.editor.move_line(false, shift),
            KeyInput::Down => self.editor.move_line(true, shift),
            KeyInput::Home => self.editor.move_home(shift),
            KeyInput::End => self.editor.move_end(shift),
            KeyInput::PageDown => self.editor.page_down(),
            KeyInput::PageUp => self.editor.page_up(),
            KeyInput::Backspace => self.editor.backspace(),
            KeyInput::Delete => self.editor.delete_forward(),
            KeyInput::Enter => self.editor.type_text("\n"),
            _ if !event.text.is_empty() && !modifiers.accel() && !modifiers.control => {
                let text: String = event.text.chars().filter(|ch| !ch.is_control()).collect();
                if !text.is_empty() {
                    self.editor.type_text(&text);
                }
            }
            _ => {}
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
