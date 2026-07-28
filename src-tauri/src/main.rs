fn main() -> std::process::ExitCode {
    if epub_mgr_lib::cli::is_cli_invocation() {
        epub_mgr_lib::cli::run_from_env()
    } else {
        epub_mgr_lib::run();
        std::process::ExitCode::SUCCESS
    }
}
