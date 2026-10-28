//! Read-only corpus measurement entry point; the acceptance gate fails closed.

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "Usage: corpus-sweep --legit-corpus <directory> --report <path>\n\
Reads <directory>/manifest.json. Writes per-producer/document measurements.\n\
Exit codes: 0 M5 false-positive acceptance satisfied; 1 I/O/schema error;\n\
2 usage error; 3 measurement produced but M5 acceptance remains unsatisfied.";

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let (mut root, mut report_path) = (None, None);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--help" | "-h") => { println!("{USAGE}"); return ExitCode::SUCCESS; }
            Some("--legit-corpus") => root = args.next().map(PathBuf::from),
            Some("--report") => report_path = args.next().map(PathBuf::from),
            _ => { eprintln!("{USAGE}"); return ExitCode::from(2); }
        }
    }
    let (Some(root), Some(path)) = (root, report_path) else {
        eprintln!("{USAGE}"); return ExitCode::from(2);
    };
    // Outputs are separate from all corpus inputs, including symlink aliases.
    let result = (|| {
        let root = root.canonicalize().map_err(|e| e.to_string())?;
        let parent = path.parent().filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| std::path::Path::new("."));
        let parent = parent.canonicalize().map_err(|e| e.to_string())?;
        let destination = if path.exists() {
            path.canonicalize().map_err(|e| e.to_string())?
        } else {
            parent.join(path.file_name().ok_or("report needs a filename")?)
        };
        if destination.starts_with(&root) {
            return Err("report must be outside the read-only corpus directory".to_owned());
        }
        let report = pdf_forensics::corpus::sweep_corpus(&root)?;
        let json = serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?;
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true)
            .open(&destination).map_err(|e| format!("cannot create {}: {e}", destination.display()))?;
        use std::io::Write;
        file.write_all(&json).and_then(|()| file.write_all(b"\n")).map_err(|e| e.to_string())?;
        Ok(report.m5_false_positive_acceptance_satisfied)
    })();
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => { eprintln!("Measurement written; M5 acceptance remains unsatisfied."); ExitCode::from(3) }
        Err(e) => { eprintln!("corpus-sweep: {e}"); ExitCode::from(1) }
    }
}
