fn main() {
    if let Err(error) = loom_router_lib::cli::run() {
        eprintln!("loom-router: {error:#}");
        std::process::exit(1);
    }
}
