use arc_openai_oauth::{extract_account_id, run_browser_flow, DEFAULT_CLIENT_ID, DEFAULT_ISSUER};

#[tokio::main]
async fn main() {
    match run_browser_flow(DEFAULT_ISSUER, DEFAULT_CLIENT_ID).await {
        Ok(tokens) => {
            println!("Login successful!");
            if let Some(account_id) = extract_account_id(&tokens) {
                println!("Account ID: {account_id}");
            }
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
