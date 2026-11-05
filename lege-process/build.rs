use std::env;
use std::path::Path;

/// Emit `lege_paddle_ocr` when PP-OCR is the active OCR backend.
///
/// The condition — "the `paddle-ocr` feature is on *and* this platform uses it"
/// — was previously spelled out at sixteen `#[cfg]` sites across six files as
/// `all(any(target_os = "linux", target_os = "macos"), feature = "paddle-ocr")`.
/// Adding a platform meant editing all sixteen. Deriving it once here keeps
/// platform knowledge in one place and leaves those sites reading
/// `#[cfg(lege_paddle_ocr)]`, which is also what they actually mean.
fn emit_paddle_ocr_cfg() {
    println!("cargo::rustc-check-cfg=cfg(lege_paddle_ocr)");

    if env::var_os("CARGO_FEATURE_PADDLE_OCR").is_none() {
        return;
    }

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let platform_uses_paddle = match target_os.as_str() {
        "linux" | "macos" => true,
        // Android has no system OCR service to fall back to, so PP-OCR is the
        // only backend. Gated on the platform feature to match the rest of the
        // Android surface.
        "android" => env::var_os("CARGO_FEATURE_ANDROID").is_some(),
        _ => false,
    };

    if platform_uses_paddle {
        println!("cargo::rustc-cfg=lege_paddle_ocr");
    }
}

fn main() {
    emit_paddle_ocr_cfg();

    let profile = env::var("PROFILE").unwrap_or_default();
    let debug_logging_enabled = env::var_os("CARGO_FEATURE_DEBUG_LOGGING").is_some();
    if profile == "release" && debug_logging_enabled {
        println!(
            "cargo:warning=debug-logging is enabled in the release profile; Cargo's default release build still uses LTO."
        );
        println!(
            "cargo:warning=Use `cargo build-debug-logging` or `cargo run-debug-logging` to build debug-logging with the repo's no-LTO profile."
        );
    }

    // Get external version from the file, with fallback to environment or default
    let external_version = read_external_version_file().unwrap_or_else(|| {
        env::var("LEGE_EXTERNAL_VERSION").unwrap_or_else(|_| "1.20.5.0".to_string())
    });

    // Pass the version to the build script
    println!("cargo:rustc-env=LEGE_EXTERNAL_VERSION={}", external_version);

    // Windows: version metadata for each executable, attached per binary so
    // the `gui` feature's `lege-gui` gets the application icon and the CLI
    // does not.
    #[cfg(windows)]
    windows_resources(&external_version);

    // Ensure build recompiles when icon changes
    println!("cargo:rerun-if-changed=../lege-misc/assets/icon.png");
    println!("cargo:rerun-if-changed=../lege-misc/assets/icon.ico");

    // Re-run this build script if environment variables change
    println!("cargo:rerun-if-changed=build.rs");
}

/// One resource script per binary. A package-wide resource (what `winres`
/// links) would reach `lege-gui` too, and a second VERSIONINFO there would
/// collide with the icon script's.
#[cfg(windows)]
fn windows_resources(version: &str) {
    let out_dir = std::path::PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));
    let icon = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lege-misc/assets/icon.ico");
    let mut binaries = vec![("lege", "Lege - PDF processor CLI", None)];
    if env::var_os("CARGO_FEATURE_GUI").is_some() {
        binaries.push(("lege-gui", "Lege", Some(icon.as_path())));
    }
    for (binary, description, icon) in binaries {
        let script = out_dir.join(format!("{binary}.rc"));
        std::fs::write(&script, resource_script(version, description, icon))
            .expect("failed to write the resource script");
        if let Err(error) =
            embed_resource::compile_for(&script, [binary], embed_resource::NONE).manifest_optional()
        {
            println!("cargo:warning=resource compile failed for {binary}: {error}");
        }
    }
}

#[cfg(windows)]
fn resource_script(version: &str, description: &str, icon: Option<&Path>) -> String {
    let icon_line = icon
        .filter(|icon| icon.exists())
        .map(|icon| {
            // rc.exe accepts forward slashes and needs no escaping for them.
            let path = icon.canonicalize().unwrap_or(icon.to_path_buf());
            let path = path.to_string_lossy().replace('\\', "/");
            let path = path.strip_prefix("//?/").unwrap_or(&path).to_owned();
            format!("IDI_APPLICATION ICON \"{path}\"\n")
        })
        .unwrap_or_default();
    let mut numbers: Vec<u16> = version
        .split('.')
        .filter_map(|part| part.parse().ok())
        .take(4)
        .collect();
    numbers.resize(4, 0);
    let tuple = format!(
        "{},{},{},{}",
        numbers[0], numbers[1], numbers[2], numbers[3]
    );
    let version = version.replace('"', "");
    format!(
        r#"{icon_line}1 VERSIONINFO
 FILEVERSION {tuple}
 PRODUCTVERSION {tuple}
 FILEOS 0x40004L
 FILETYPE 0x1L
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904B0"
        BEGIN
            VALUE "CompanyName", "Lege Apps\0"
            VALUE "FileDescription", "{description}\0"
            VALUE "FileVersion", "{version}\0"
            VALUE "ProductName", "Lege\0"
            VALUE "ProductVersion", "{version}\0"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x0409, 1200
    END
END
"#
    )
}

/// Read external version from the file in the project root
fn read_external_version_file() -> Option<String> {
    let version_file = Path::new("external_version.txt");
    if version_file.exists()
        && let Ok(content) = std::fs::read_to_string(version_file)
    {
        let content = content.trim();
        if !content.is_empty() {
            return Some(content.to_string());
        }
    }
    None
}
