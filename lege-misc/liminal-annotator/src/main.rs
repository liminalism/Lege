//! liminal-annotator: see `liminal-annotator --help`.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = liminal_annotator::cli::main_with(args) {
        eprintln!("liminal-annotator: {e}");
        std::process::exit(1);
    }
}
