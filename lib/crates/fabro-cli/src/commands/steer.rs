use anyhow::{Context, Result, bail};
use fabro_config::user::default_storage_dir;

use crate::args::SteerArgs;
use crate::server_client;

#[expect(clippy::print_stdout, reason = "CLI feedback to the user.")]
pub(crate) async fn execute(args: SteerArgs) -> Result<()> {
    let text = if args.text_stdin {
        let mut buf = String::new();
        #[expect(
            clippy::disallowed_methods,
            reason = "Steer reads optional stdin text; no Tokio path involved."
        )]
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)
            .context("reading steering text from stdin")?;
        buf
    } else if let Some(text) = args.text {
        text
    } else {
        bail!("provide steering text as an argument or use --text-stdin");
    };

    if text.trim().is_empty() {
        bail!("steering text cannot be empty");
    }

    let storage_dir = default_storage_dir();
    let client = server_client::connect_server(&storage_dir).await?;
    client
        .steer_run(&args.run_id, &text, args.interrupt)
        .await
        .context("failed to steer run")?;

    println!("Steer delivered.");
    Ok(())
}
