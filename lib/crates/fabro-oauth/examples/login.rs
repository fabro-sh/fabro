#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::env;

use fabro_oauth::run_browser_flow;

#[tokio::main]
async fn main() {
    let issuer = env::var("OAUTH_ISSUER").expect("set OAUTH_ISSUER");
    let client_id = env::var("OAUTH_CLIENT_ID").expect("set OAUTH_CLIENT_ID");
    let scope = env::var("OAUTH_SCOPE").unwrap_or_else(|_| "openid profile email".to_string());
    let port: u16 = env::var("OAUTH_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let callback_path = env::var("OAUTH_CALLBACK_PATH").unwrap_or_else(|_| "/callback".to_string());

    match run_browser_flow(&issuer, &client_id, &scope, port, &callback_path).await {
        Ok(tokens) => {
            println!("Login successful!");
            println!(
                "Access token: {}...",
                &tokens.access_token[..20.min(tokens.access_token.len())]
            );
            if let Some(expires_in) = tokens.expires_in {
                println!("Expires in: {expires_in}s");
            }
        }
        Err(e) => {
            eprintln!("Login failed: {e}");
            std::process::exit(1);
        }
    }
}
