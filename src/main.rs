fn main() {
    if let Err(error) = loom_router::cli::run() {
        eprintln!("loom-router: {error:#}");
        std::process::exit(1);
    }
}
