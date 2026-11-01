#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(not(target_os = "android"))]
fn main() -> anyhow::Result<()> {
    lege_gui_liminalui::run()
}

#[cfg(target_os = "android")]
fn main() {
    eprintln!("The desktop GUI is not available on Android.");
}
