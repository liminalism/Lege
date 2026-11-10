use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("appimage") => build_appimage(args.collect()),
        Some("latest") => build_latest(args.collect()),
        Some("help") | Some("--help") | Some("-h") | None => {
            print_help();
            Ok(())
        }
        Some(other) => Err(format!(
            "unknown xtask command `{other}`\n\nRun `cargo appimage`, `cargo latest` or `cargo run -p xtask -- help`."
        )),
    }
}

fn build_appimage(extra_args: Vec<String>) -> Result<(), String> {
    let gui_variant = match extra_args.as_slice() {
        [] => "liminalui",
        [arg] if arg == "viewer" || arg == "--viewer" => "viewer",
        [arg] if arg == "liminalui" || arg == "--liminalui" => "liminalui",
        _ => {
            return Err(format!(
                "unsupported cargo appimage arguments: {}\n\nRun `cargo appimage` (liminalUI GUI) or `cargo appimage viewer`.",
                extra_args.join(" ")
            ));
        }
    };

    if !cfg!(target_os = "linux") {
        return Err("AppImage packaging is only supported from Linux/WSL.".to_string());
    }

    let root = workspace_root()?;
    let script = root.join("lege-process/scripts/build-appimage.sh");
    if !script.exists() {
        return Err(format!("missing AppImage script: {}", script.display()));
    }

    println!("Building Lege AppImage via {}", script.display());
    let status = Command::new("bash")
        .arg(script)
        .env("APPIMAGE_GUI", gui_variant)
        .current_dir(&root)
        .status()
        .map_err(|err| format!("failed to launch AppImage build script: {err}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("AppImage build failed with status {status}"))
    }
}

/// Build Lege against the newest code of its git dependencies.
///
/// A git dependency names a branch, not a commit, but `Cargo.lock` records the
/// commit it was last resolved to and a plain `cargo build` keeps using it.
/// This moves every git dependency in the lockfile to the head of its branch
/// and then builds. Registry (crates.io) versions are left where the lockfile
/// has them; the sibling `exprim` checkout is a path dependency and is always
/// built as it stands.
fn build_latest(build_args: Vec<String>) -> Result<(), String> {
    let root = workspace_root()?;
    let lock = std::fs::read_to_string(root.join("Cargo.lock"))
        .map_err(|err| format!("could not read Cargo.lock: {err}"))?;
    let packages = git_packages(&lock);
    if packages.is_empty() {
        println!("No git dependencies in Cargo.lock; nothing to refresh.");
    } else {
        println!("Refreshing git dependencies: {}", packages.join(", "));
        let mut update = Command::new("cargo");
        update.arg("update").current_dir(&root);
        for package in &packages {
            update.arg("--package").arg(package);
        }
        run_cargo(update, "cargo update")?;
    }
    let mut build = Command::new("cargo");
    build
        .args(["build", "--package", "lege"])
        .args(&build_args)
        .current_dir(&root);
    run_cargo(build, "cargo build")
}

fn run_cargo(mut command: Command, what: &str) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|err| format!("failed to launch {what}: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{what} failed with status {status}"))
    }
}

/// Names of the packages `Cargo.lock` resolves from a git repository.
fn git_packages(lock: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut current: Option<&str> = None;
    for line in lock.lines() {
        let line = line.trim();
        if line == "[[package]]" {
            current = None;
        } else if let Some(name) = line
            .strip_prefix("name = \"")
            .and_then(|rest| rest.strip_suffix('"'))
        {
            current = Some(name);
        } else if line.starts_with("source = \"git+")
            && let Some(name) = current
            && !names.iter().any(|known| known == name)
        {
            names.push(name.to_owned());
        }
    }
    names
}

fn workspace_root() -> Result<PathBuf, String> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            format!(
                "could not determine workspace root from {}",
                manifest_dir.display()
            )
        })
}

fn print_help() {
    println!(
        "Lege build and packaging tasks\n\n  cargo latest [cargo build options]\n      Move the git dependencies in Cargo.lock to the head of their branches,\n      then build lege and lege-gui (for example `cargo latest --release`).\n\n  cargo appimage [viewer]\n      Build target/appimage/Lege-<version>-<arch>.AppImage\n      Defaults to the liminalUI GUI; pass `viewer` to use the Liminaldoc reader.\n\nEnvironment forwarded to scripts/build-appimage.sh:\n  APPIMAGETOOL                appimagetool executable/path\n  APPIMAGE_GUI                 gui variant: liminalui (default) or viewer\n  APPIMAGE_UPDATE_INFORMATION AppImage update metadata string\n  LEGE_CARGO_FEATURES         Additional Cargo features"
    );
}

#[cfg(test)]
mod tests {
    use super::git_packages;

    #[test]
    fn only_packages_resolved_from_git_are_refreshed() {
        let lock = r#"
[[package]]
name = "image"
version = "0.25.10"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "lege"
version = "1.6.0"

[[package]]
name = "liminalkit-ui"
version = "0.1.0"
source = "git+https://github.com/liminalism/liminalkit?branch=main#b60e4cea"

[[package]]
name = "liminalkit-text"
version = "0.1.0"
source = "git+https://github.com/liminalism/liminalkit?branch=main#b60e4cea"
"#;
        assert_eq!(git_packages(lock), ["liminalkit-ui", "liminalkit-text"]);
    }
}
