//! Headless daemon binary (`ternvaled`).

fn main() -> std::process::ExitCode {
    ternvale_cli::daemon::main()
}
