use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    match agent::cli::run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[error] {e}");
            ExitCode::FAILURE
        }
    }
}
