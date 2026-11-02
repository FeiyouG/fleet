mod cli;
mod tui;

fn main() {
    if let Err(e) = cli::run() {
        eprintln!("fleet: {e:#}");
        std::process::exit(1);
    }
}
