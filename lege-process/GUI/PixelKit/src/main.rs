#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app;
mod layout;
#[cfg(target_os = "linux")]
#[allow(dead_code)]
#[path = "../../Freya/src/linux_desktop.rs"]
mod linux_desktop;
mod theme;
// Reuse the toolkit-independent frontend support during migration. The
// baseline Freya frontend consumes these exact same models and wire types.
#[path = "../../Freya/src/appearance.rs"]
mod appearance;
#[allow(dead_code, unused_imports)]
#[path = "../../Freya/src/backend.rs"]
mod backend;
#[allow(dead_code)]
#[path = "../../Freya/src/gui_text.rs"]
mod gui_text;
#[allow(dead_code, unused_imports)]
#[path = "../../Freya/src/logging.rs"]
mod logging;
#[allow(dead_code)]
#[path = "../../Freya/src/models.rs"]
mod models;
#[path = "../../Freya/src/sanzowada.rs"]
mod sanzowada;
#[allow(dead_code)]
#[path = "../../Freya/src/settings.rs"]
mod settings;
#[path = "../../Freya/src/version.rs"]
mod version;
#[allow(dead_code)]
#[path = "../../Freya/src/worker_process.rs"]
mod worker_process;

use pixelkit_windowing::PixelApp;

fn main() -> anyhow::Result<()> {
    let mut app = app::LegeApp::new();
    // The actual UI renderer, without a window or display server, for visual
    // review at different sizes/scales. Never changes the user's settings.
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "--snapshot") {
        let path = args
            .get(1)
            .ok_or_else(|| anyhow::anyhow!("--snapshot PATH [WIDTH HEIGHT SCALE]"))?;
        let width = args
            .get(2)
            .map(|s| s.parse())
            .transpose()?
            .unwrap_or(940u32);
        let height = args
            .get(3)
            .map(|s| s.parse())
            .transpose()?
            .unwrap_or(700u32);
        let scale = args
            .get(4)
            .map(|s| s.parse())
            .transpose()?
            .unwrap_or(1.0f32);
        anyhow::ensure!(
            width > 0
                && height > 0
                && width <= 8192
                && height <= 8192
                && scale.is_finite()
                && (0.5..=4.0).contains(&scale),
            "invalid snapshot dimensions or scale"
        );
        app.snapshot_mode = true;
        if args.iter().any(|s| s == "--preserve") {
            app.dashboard.options.set_preserve_mode(true);
        }
        if args.iter().any(|s| s == "--threshold") {
            app.dashboard.select_binarization(1);
        }
        if args.iter().any(|s| s == "--heavy") {
            app.dashboard.select_binarization(2);
        }
        if args.iter().any(|s| s == "--dark") {
            appearance::set_theme_preview(Some(appearance::ThemeChoice::Sanzo {
                index: 2,
                dark: true,
            }));
        }
        let mut buffer = pixelkit_raster::WindowBuffer::new(width, height);
        app.render(&mut buffer, pixelkit_shell::Scale(scale));
        pixelkit_raster::png::write(path, &buffer)?;
        return Ok(());
    }
    // The preview must not replace the shipping GUI's lege.desktop launcher.
    // Reuse desktop registration when the default frontend switches.
    let mut config =
        pixelkit_windowing::WindowConfig::new("Lege", 940.0, 700.0).min_size(620.0, 680.0);
    config.app_id = Some("org.lege.Lege".into());
    pixelkit_windowing::run_app(app, config).map_err(|e| anyhow::anyhow!("{e}"))
}
