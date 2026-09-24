//! Paint the editor without a window and save the frame as a PNG:
//! `cargo run -p docwrite-app --example snapshot -- BOOK.legebook OUT.png [page] [--fullscreen] [--scale 2] [--source S.pdf] [--spread]`

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use docwrite_app::Editor;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let fullscreen = args.iter().any(|arg| arg == "--fullscreen");
    let spread = args.iter().any(|arg| arg == "--spread");
    let scale: f32 = args
        .iter()
        .position(|arg| arg == "--scale")
        .and_then(|index| args.get(index + 1))
        .and_then(|value| value.parse().ok())
        .unwrap_or(1.0);
    let source = args
        .iter()
        .position(|arg| arg == "--source")
        .and_then(|index| args.get(index + 1))
        .cloned();
    let plain: Vec<&String> = args
        .iter()
        .filter(|arg| !arg.starts_with("--") && arg.parse::<f32>().is_err())
        .filter(|arg| Some(*arg) != source.as_ref())
        .collect();
    let page: u32 = args
        .iter()
        .filter(|arg| !arg.starts_with("--"))
        .filter_map(|arg| arg.parse().ok())
        .find(|value: &u32| f32::from(*value as u16) != scale)
        .unwrap_or(1);
    let (Some(book), Some(out)) = (plain.first(), plain.get(1)) else {
        panic!("usage: snapshot BOOK.legebook OUT.png [page] [--fullscreen] [--scale N]");
    };
    let mut editor = Editor::open_or_create(book.as_str()).expect("open");
    editor.set_ui_scale(scale);
    editor.set_fullscreen(fullscreen);
    editor.set_spread(spread);
    if let Some(source) = &source {
        editor.open_source(source).expect("open source");
    }
    let base_w = if source.is_some() || spread {
        1500.0
    } else {
        1100.0
    };
    let (width, height) = ((base_w * scale) as u32, (800.0 * scale) as u32);
    let mut frame = pixelkit_raster::WindowBuffer::new(width, height);
    editor.paint(&mut frame);
    editor.jump_to_page(page);
    editor.paint(&mut frame);
    pixelkit_raster::png::write(out.as_str(), &frame).expect("write png");
    println!("wrote {out}: page {page} of {}", editor.pages_painted());
}
