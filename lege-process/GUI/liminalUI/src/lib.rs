mod app;
mod appearance;
#[allow(dead_code, unused_imports)]
mod backend;
#[allow(dead_code)]
mod file_tiles;
mod gui_text;
mod layout;
#[cfg(target_os = "linux")]
#[allow(dead_code)]
mod linux_desktop;
#[allow(dead_code, unused_imports)]
mod logging;
#[allow(dead_code)]
mod models;
mod sanzowada;
#[allow(dead_code)]
mod settings;
mod shell_files;
mod theme;
mod version;
#[allow(dead_code)]
mod worker_process;

use liminalkit_windowing::PixelApp;

pub fn run() -> anyhow::Result<()> {
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
        if args.iter().any(|s| s == "--flatten") {
            app.dashboard.options.flatten_pages = true;
        }
        if args.iter().any(|s| s == "--dark") {
            appearance::set_theme_preview(Some(appearance::ThemeChoice::Sanzo {
                index: 2,
                dark: true,
            }));
        }
        let mut buffer = liminalkit_raster::WindowBuffer::new(width, height);
        app.render(&mut buffer, liminalkit_shell::Scale(scale));
        liminalkit_raster::png::write(path, &buffer)?;
        return Ok(());
    }
    // The preview must not replace the shipping GUI's lege.desktop launcher.
    // Reuse desktop registration when the default frontend switches.
    let mut config =
        liminalkit_windowing::WindowConfig::new("Lege", 1040.0, 760.0).min_size(720.0, 660.0);
    config.app_id = Some("org.lege.Lege".into());
    liminalkit_windowing::run_app(app, config).map_err(|e| anyhow::anyhow!("{e}"))
}
