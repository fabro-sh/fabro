use std::io::{self, IsTerminal, Read};

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use futures::StreamExt;

use crate::catalog;
use crate::generate::{self, GenerateParams};

#[derive(Args)]
pub struct PromptArgs {
    /// The prompt text (also accepts stdin)
    pub prompt: Option<String>,

    /// Model to use
    #[arg(short, long)]
    pub model: Option<String>,

    /// System prompt
    #[arg(short, long)]
    pub system: Option<String>,

    /// Do not stream output
    #[arg(long)]
    pub no_stream: bool,

    /// Show token usage
    #[arg(short, long)]
    pub usage: bool,

    /// JSON schema for structured output (inline JSON string)
    #[arg(short = 'S', long)]
    pub schema: Option<String>,

    /// key=value options (temperature, `max_tokens`, `top_p`)
    #[arg(short, long, value_parser = parse_option)]
    pub option: Vec<(String, String)>,
}

#[derive(Subcommand)]
pub enum ModelsCommand {
    /// List available models
    List {
        /// Filter by provider
        #[arg(short, long)]
        provider: Option<String>,

        /// Search for models matching this string
        #[arg(short, long)]
        query: Option<String>,
    },

    /// Download model metadata from OpenRouter
    Sync {
        /// URL to fetch models from
        #[arg(long, default_value = "https://openrouter.ai/api/v1/models")]
        url: String,

        /// Output file path
        #[arg(short, long, default_value = "openrouter_models.json")]
        output: String,
    },
}

fn parse_option(s: &str) -> Result<(String, String), String> {
    let (key, value) = s
        .split_once('=')
        .ok_or_else(|| format!("expected key=value, got {s}"))?;
    Ok((key.to_string(), value.to_string()))
}

fn print_models_table(models: &[crate::types::ModelInfo]) {
    println!(
        "{:<30} {:<12} {:<30} {:>14}",
        "ID", "PROVIDER", "ALIASES", "CONTEXT"
    );
    for model in models {
        let aliases = model.aliases.join(", ");
        println!(
            "{:<30} {:<12} {:<30} {:>14}",
            model.id, model.provider, aliases, model.context_window
        );
    }
}

fn read_stdin_prompt() -> Option<String> {
    let stdin = io::stdin();
    if stdin.is_terminal() {
        return None;
    }
    let mut buf = String::new();
    stdin.lock().read_to_string(&mut buf).ok()?;
    let trimmed = buf.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn resolve_prompt(arg: Option<String>, stdin: Option<String>) -> Result<String> {
    match (stdin, arg) {
        (Some(s), Some(a)) => Ok(format!("{s}\n{a}")),
        (Some(s), None) => Ok(s),
        (None, Some(a)) => Ok(a),
        (None, None) => {
            bail!("Error: no prompt provided. Pass a prompt as an argument or pipe text via stdin.")
        }
    }
}

/// Returns (`model_id`, provider) from the catalog, falling back to the first catalog model.
fn resolve_model(model_arg: Option<String>) -> (String, Option<String>) {
    let raw = model_arg.unwrap_or_else(|| {
        catalog::list_models(None)
            .first()
            .map_or_else(|| "claude-sonnet-4-5".to_string(), |m| m.id.clone())
    });
    match catalog::get_model_info(&raw) {
        Some(info) => (info.id, Some(info.provider)),
        None => (raw, None),
    }
}

fn apply_options(
    mut params: GenerateParams,
    options: &[(String, String)],
) -> Result<GenerateParams> {
    let mut provider_opts = serde_json::Map::new();

    for (key, value) in options {
        match key.as_str() {
            "temperature" => {
                let v: f64 = value
                    .parse()
                    .with_context(|| format!("invalid temperature value: {value}"))?;
                params = params.temperature(v);
            }
            "max_tokens" => {
                let v: i64 = value
                    .parse()
                    .with_context(|| format!("invalid max_tokens value: {value}"))?;
                params = params.max_tokens(v);
            }
            "top_p" => {
                let v: f64 = value
                    .parse()
                    .with_context(|| format!("invalid top_p value: {value}"))?;
                params = params.top_p(v);
            }
            _ => {
                provider_opts.insert(key.clone(), serde_json::Value::String(value.clone()));
            }
        }
    }

    if !provider_opts.is_empty() {
        params = params.provider_options(serde_json::Value::Object(provider_opts));
    }

    Ok(params)
}

fn print_usage(usage: &crate::types::Usage) {
    eprintln!(
        "Tokens: {} input, {} output, {} total",
        usage.input_tokens, usage.output_tokens, usage.total_tokens
    );
}

pub async fn run_prompt(args: PromptArgs) -> Result<()> {
    let stdin_prompt = read_stdin_prompt();
    let prompt_text = resolve_prompt(args.prompt, stdin_prompt)?;
    let (model_id, provider) = resolve_model(args.model);

    eprintln!("Using model: {model_id}");

    let mut params = GenerateParams::new(&model_id).prompt(&prompt_text);
    if let Some(p) = provider {
        params = params.provider(&p);
    }
    if let Some(sys) = args.system {
        params = params.system(&sys);
    }
    params = apply_options(params, &args.option)?;

    let schema: Option<serde_json::Value> = match &args.schema {
        Some(s) => Some(serde_json::from_str(s).context("--schema must be valid JSON")?),
        None => None,
    };

    match (args.no_stream, schema) {
        (true, Some(schema)) => {
            let result = generate::generate_object(params, schema).await?;
            let object = result.output.as_ref().unwrap_or(&serde_json::Value::Null);
            println!("{}", serde_json::to_string_pretty(object)?);
            if args.usage {
                print_usage(&result.usage);
            }
        }
        (true, None) => {
            let result = generate::generate(params).await?;
            print!("{}", result.text());
            if args.usage {
                print_usage(&result.usage);
            }
        }
        (false, Some(schema)) => {
            let mut stream_result = generate::stream_object(params, schema).await?;
            while let Some(event) = stream_result.next().await {
                event?;
            }
            if let Some(object) = stream_result.object() {
                println!("{}", serde_json::to_string_pretty(object)?);
            }
        }
        (false, None) => {
            let mut stream_result = generate::stream(params).await?;
            while let Some(event) = stream_result.next().await {
                if let crate::types::StreamEvent::TextDelta { delta, .. } = event? {
                    print!("{delta}");
                }
            }
            println!();
            if args.usage {
                if let Some(response) = stream_result.response() {
                    print_usage(&response.usage);
                }
            }
        }
    }

    Ok(())
}

async fn sync_models(url: &str, output: &str) -> Result<()> {
    let body = reqwest::get(url)
        .await
        .context("failed to connect to models endpoint")?
        .error_for_status()
        .context("models endpoint returned an error")?
        .text()
        .await
        .context("failed to read response body")?;

    let json: serde_json::Value =
        serde_json::from_str(&body).context("response is not valid JSON")?;
    let pretty = serde_json::to_string_pretty(&json).context("failed to format JSON")?;

    std::fs::write(output, &pretty).with_context(|| format!("failed to write {output}"))?;

    eprintln!("Saved models to {output}");
    Ok(())
}

pub async fn run_models(command: Option<ModelsCommand>) -> Result<()> {
    let command = command.unwrap_or(ModelsCommand::List {
        provider: None,
        query: None,
    });

    match command {
        ModelsCommand::List { provider, query } => {
            let mut models = catalog::list_models(provider.as_deref());

            if let Some(q) = &query {
                let q_lower = q.to_lowercase();
                models.retain(|m| {
                    m.id.to_lowercase().contains(&q_lower)
                        || m.display_name.to_lowercase().contains(&q_lower)
                        || m.aliases
                            .iter()
                            .any(|a| a.to_lowercase().contains(&q_lower))
                });
            }

            print_models_table(&models);
        }
        ModelsCommand::Sync { url, output } => {
            sync_models(&url, &output).await?;
        }
    }

    Ok(())
}
