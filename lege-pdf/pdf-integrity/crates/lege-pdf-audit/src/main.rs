//! `lege-pdf-audit` binary: process entry point over the parsing, audit and
//! rendering logic in the library.

use std::io::Write;
use std::process::ExitCode;

fn main() -> ExitCode {
    match lege_pdf_audit::parse_args(std::env::args_os()) {
        Ok(lege_pdf_audit::Action::Help) => {
            let stdout = std::io::stdout();
            let mut locked = stdout.lock();
            // Help goes to stdout; if it cannot be written there is nothing
            // left to report it with.
            let _ = writeln!(locked, "{}", lege_pdf_audit::USAGE);
            ExitCode::SUCCESS
        }
        Ok(lege_pdf_audit::Action::Inspect(options)) => {
            let stdout = std::io::stdout();
            let mut locked = stdout.lock();
            match lege_pdf_audit::run(&options, &mut locked) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("lege-pdf-audit: {error}");
                    ExitCode::from(error.exit_code)
                }
            }
        }
        Err(error) => {
            eprintln!("lege-pdf-audit: {error}\n\n{}", lege_pdf_audit::USAGE);
            ExitCode::from(2)
        }
    }
}
