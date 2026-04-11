use anyhow::{Context, Result, bail};
use cli_table::format::{Border, Justify, Separator};
use cli_table::{Cell, CellStruct, Color, Style, Table};
use fabro_api::{self, types as api_types};
use fabro_model::{Catalog, Model, Provider};
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::args::{GlobalArgs, ModelListArgs, ModelTestArgs, ModelsCommand};
use crate::command_context::CommandContext;
use crate::server_client;

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum ModelTestResultKind {
    Pass,
    Fail,
    Skip,
}

#[derive(Serialize)]
struct ModelTestRow {
    model:    String,
    provider: Provider,
    result:   ModelTestResultKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail:   Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error:    Option<String>,
}

#[derive(Serialize)]
struct ModelTestOutput {
    results:  Vec<ModelTestRow>,
    total:    usize,
    failures: u32,
}

pub(crate) async fn execute(
    command: Option<ModelsCommand>,
    globals: &GlobalArgs,
    printer: Printer,
) -> Result<()> {
    let command = command.unwrap_or_default();
    let target_args = match &command {
        ModelsCommand::List(args) => &args.target,
        ModelsCommand::Test(args) => &args.target,
    };
    let ctx = CommandContext::for_target(target_args, printer)?;
    let server = ctx.server().await?;

    run_models(command, server.api(), globals.json).await
}

fn format_context_window(tokens: i64) -> String {
    let rounded = ((tokens + 500) / 1_000) * 1_000;
    if rounded >= 1_000_000 {
        format!("{}m", rounded / 1_000_000)
    } else if rounded >= 1_000 {
        format!("{}k", rounded / 1_000)
    } else {
        tokens.to_string()
    }
}

fn format_cost(cost: Option<f64>) -> String {
    match cost {
        None => "-".to_string(),
        Some(c) => format!("${c:.1}"),
    }
}

fn format_speed(tps: Option<f64>) -> String {
    match tps {
        None => "-".to_string(),
        #[allow(clippy::cast_possible_truncation)]
        Some(t) => format!("{} tok/s", t as i64),
    }
}

fn color_if(use_color: bool, color: Color) -> Option<Color> {
    if use_color { Some(color) } else { None }
}

fn color_choice(use_color: bool) -> cli_table::ColorChoice {
    if use_color {
        cli_table::ColorChoice::Auto
    } else {
        cli_table::ColorChoice::Never
    }
}

fn model_row(model: &Model, use_color: bool) -> Vec<CellStruct> {
    let aliases = model.aliases.join(", ");
    let cost = format!(
        "{} / {}",
        format_cost(model.costs.input_cost_per_mtok),
        format_cost(model.costs.output_cost_per_mtok),
    );
    vec![
        model.id.clone().cell().bold(use_color),
        model
            .provider
            .cell()
            .foreground_color(color_if(use_color, Color::Ansi256(8))),
        aliases
            .cell()
            .foreground_color(color_if(use_color, Color::Ansi256(8))),
        format_context_window(model.limits.context_window)
            .cell()
            .justify(Justify::Right),
        cost.cell().justify(Justify::Right),
        format_speed(model.estimated_output_tps)
            .cell()
            .justify(Justify::Right)
            .foreground_color(color_if(use_color, Color::Cyan)),
    ]
}

fn models_title(use_color: bool) -> Vec<CellStruct> {
    vec![
        "MODEL".cell().bold(use_color),
        "PROVIDER".cell().bold(use_color),
        "ALIASES".cell().bold(use_color),
        "CONTEXT".cell().bold(use_color).justify(Justify::Right),
        "COST".cell().bold(use_color).justify(Justify::Right),
        "SPEED".cell().bold(use_color).justify(Justify::Right),
    ]
}

#[allow(clippy::print_stdout)]
fn print_models_table(models: &[Model], styles: &Styles) {
    let use_color = styles.use_color;
    let rows: Vec<Vec<CellStruct>> = models
        .iter()
        .map(|model| model_row(model, use_color))
        .collect();
    let table = rows
        .table()
        .title(models_title(use_color))
        .color_choice(color_choice(use_color))
        .border(Border::builder().build())
        .separator(Separator::builder().build());
    println!("{}", table.display().unwrap());
}

fn model_test_row_from_status(model: &Model, status: &str, result_color: Color) -> ModelTestRow {
    let trimmed = status.trim();
    match result_color {
        Color::Green => ModelTestRow {
            model:    model.id.clone(),
            provider: model.provider,
            result:   ModelTestResultKind::Pass,
            detail:   None,
            error:    None,
        },
        Color::Yellow => ModelTestRow {
            model:    model.id.clone(),
            provider: model.provider,
            result:   ModelTestResultKind::Skip,
            detail:   Some(trimmed.to_string()),
            error:    None,
        },
        _ => ModelTestRow {
            model:    model.id.clone(),
            provider: model.provider,
            result:   ModelTestResultKind::Fail,
            detail:   None,
            error:    Some(
                trimmed
                    .strip_prefix("error: ")
                    .unwrap_or(trimmed)
                    .to_string(),
            ),
        },
    }
}

fn convert_type<TInput, TOutput>(value: TInput) -> Result<TOutput>
where
    TInput: serde::Serialize,
    TOutput: DeserializeOwned,
{
    serde_json::from_value(serde_json::to_value(value)?).map_err(Into::into)
}

async fn fetch_models_from_server(
    client: &fabro_api::Client,
    provider: Option<&str>,
    query: Option<&str>,
) -> Result<Vec<Model>> {
    let mut offset = 0u64;
    let mut models = Vec::new();

    loop {
        let mut request = client.list_models().page_limit(100u64).page_offset(offset);
        if let Some(provider) = provider {
            request = request.provider(provider.to_string());
        }
        if let Some(query) = query {
            request = request.query(query.to_string());
        }

        let response = request.send().await.map_err(server_client::map_api_error)?;
        let parsed = response.into_inner();
        let count = parsed.data.len() as u64;
        models.extend(convert_type::<_, Vec<Model>>(parsed.data)?);
        if !parsed.meta.has_more {
            break;
        }
        offset += count;
    }

    Ok(models)
}

async fn test_model_via_server(
    client: &fabro_api::Client,
    model_id: &str,
    mode: Option<api_types::ModelTestMode>,
) -> Result<api_types::ModelTestResult> {
    let mut request = client.test_model().id(model_id.to_string());
    if let Some(mode) = mode {
        request = request.mode(mode);
    }
    let response = request.send().await.map_err(server_client::map_api_error)?;
    Ok(response.into_inner())
}

#[allow(clippy::print_stdout, clippy::print_stderr)]
async fn test_models_via_server(
    client: &fabro_api::Client,
    provider: Option<&str>,
    model: Option<&str>,
    deep: bool,
    styles: &Styles,
    json_output: bool,
) -> Result<()> {
    let request_mode = deep.then_some(api_types::ModelTestMode::Deep);

    let use_color = styles.use_color;
    let mut title = models_title(use_color);
    title.push("RESULT".cell().bold(use_color));

    let mut rows: Vec<Vec<CellStruct>> = Vec::new();
    let mut json_rows = Vec::new();
    let mut failures = 0u32;
    if let Some(model_id) = model {
        if !json_output {
            eprint!("Testing {model_id}...");
        }
        let result = test_model_via_server(client, model_id, request_mode).await;
        if !json_output {
            eprintln!(" done");
        }

        let (info, result_color, status) = match result {
            Ok(resp) => {
                let info = Catalog::builtin()
                    .get(&resp.model_id)
                    .cloned()
                    .with_context(|| {
                        format!("Unknown model returned by server: {}", resp.model_id)
                    })?;
                if resp.status == api_types::ModelTestResultStatus::Ok {
                    (info, Color::Green, "ok".to_string())
                } else {
                    failures += 1;
                    let message = resp
                        .error_message
                        .unwrap_or_else(|| "unknown error".to_string());
                    (info, Color::Red, format!("error: {message}"))
                }
            }
            Err(err) if err.to_string().contains("Model not found") => {
                bail!("Unknown model: {model_id}");
            }
            Err(err) => {
                let info = Catalog::builtin()
                    .get(model_id)
                    .cloned()
                    .with_context(|| format!("Unknown model: {model_id}"))?;
                failures += 1;
                (info, Color::Red, format!("error: {err}"))
            }
        };

        let mut row = model_row(&info, use_color);
        row.push(
            status
                .clone()
                .cell()
                .foreground_color(color_if(use_color, result_color)),
        );
        rows.push(row);
        json_rows.push(model_test_row_from_status(&info, &status, result_color));
    } else {
        let models_to_test = fetch_models_from_server(client, provider, None).await?;
        if models_to_test.is_empty() {
            bail!("No models found");
        }

        for info in &models_to_test {
            if !json_output {
                eprint!("Testing {}...", info.id);
            }
            let result = test_model_via_server(client, &info.id, request_mode).await;
            if !json_output {
                eprintln!(" done");
            }

            let (result_color, status) = match result {
                Ok(resp) if resp.status == api_types::ModelTestResultStatus::Ok => {
                    (Color::Green, "ok".to_string())
                }
                Ok(resp) => {
                    failures += 1;
                    let message = resp
                        .error_message
                        .unwrap_or_else(|| "unknown error".to_string());
                    (Color::Red, format!("error: {message}"))
                }
                Err(err) => {
                    failures += 1;
                    (Color::Red, format!("error: {err}"))
                }
            };

            let mut row = model_row(info, use_color);
            row.push(
                status
                    .clone()
                    .cell()
                    .foreground_color(color_if(use_color, result_color)),
            );
            rows.push(row);
            json_rows.push(model_test_row_from_status(info, &status, result_color));
        }
    }

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&ModelTestOutput {
                total: json_rows.len(),
                failures,
                results: json_rows,
            })?
        );
        if failures > 0 {
            bail!("{failures} model(s) failed");
        }
        return Ok(());
    }

    let table = rows
        .table()
        .title(title)
        .color_choice(color_choice(use_color))
        .border(Border::builder().build())
        .separator(Separator::builder().build());
    println!("{}", table.display()?);

    if failures > 0 {
        bail!("{failures} model(s) failed");
    }

    Ok(())
}

#[allow(clippy::print_stdout)]
async fn run_models(
    command: ModelsCommand,
    client: &fabro_api::Client,
    json_output: bool,
) -> Result<()> {
    let styles = Styles::detect_stdout();

    match command {
        ModelsCommand::List(ModelListArgs {
            provider, query, ..
        }) => {
            let models =
                fetch_models_from_server(client, provider.as_deref(), query.as_deref()).await?;

            if json_output {
                println!("{}", serde_json::to_string_pretty(&models)?);
            } else {
                print_models_table(&models, &styles);
            }
        }
        ModelsCommand::Test(ModelTestArgs {
            provider,
            model,
            deep,
            ..
        }) => {
            test_models_via_server(
                client,
                provider.as_deref(),
                model.as_deref(),
                deep,
                &styles,
                json_output,
            )
            .await?;
        }
    }

    Ok(())
}

impl Default for ModelsCommand {
    fn default() -> Self {
        Self::List(ModelListArgs::default())
    }
}

#[cfg(test)]
mod tests {
    use fabro_model::{ModelCosts, ModelFeatures, ModelLimits};

    use super::*;

    fn test_http_client() -> reqwest::Client {
        reqwest::Client::builder().no_proxy().build().unwrap()
    }

    fn test_api_client(api_url: &str) -> fabro_api::Client {
        fabro_api::Client::new_with_client(api_url, test_http_client())
    }

    fn test_model_json(id: &str, provider: Provider) -> serde_json::Value {
        serde_json::to_value(Model {
            id: id.to_string(),
            provider,
            family: "test".to_string(),
            display_name: format!("{id} display"),
            limits: ModelLimits {
                context_window: 128_000,
                max_output:     Some(4096),
            },
            training: None,
            knowledge_cutoff: None,
            features: ModelFeatures {
                tools:     true,
                vision:    false,
                reasoning: false,
                effort:    false,
            },
            costs: ModelCosts {
                input_cost_per_mtok:       Some(1.0),
                output_cost_per_mtok:      Some(2.0),
                cache_input_cost_per_mtok: None,
            },
            estimated_output_tps: Some(100.0),
            aliases: vec!["tm".to_string()],
            default: false,
        })
        .unwrap()
    }

    #[test]
    fn format_context_window_millions() {
        assert_eq!(format_context_window(1_000_000), "1m");
    }

    #[test]
    fn format_context_window_thousands() {
        assert_eq!(format_context_window(128_000), "128k");
    }

    #[test]
    fn format_context_window_small() {
        assert_eq!(format_context_window(400), "400");
    }

    #[test]
    fn format_context_window_rounds_up() {
        assert_eq!(format_context_window(1500), "2k");
    }

    #[test]
    fn format_context_window_rounds_down() {
        assert_eq!(format_context_window(1499), "1k");
    }

    #[test]
    fn format_context_window_zero() {
        assert_eq!(format_context_window(0), "0");
    }

    #[test]
    fn format_cost_none() {
        assert_eq!(format_cost(None), "-");
    }

    #[test]
    fn format_cost_some() {
        assert_eq!(format_cost(Some(3.0)), "$3.0");
    }

    #[test]
    fn format_cost_fractional() {
        assert_eq!(format_cost(Some(15.75)), "$15.8");
    }

    #[test]
    fn format_speed_none() {
        assert_eq!(format_speed(None), "-");
    }

    #[test]
    fn format_speed_some() {
        assert_eq!(format_speed(Some(85.5)), "85 tok/s");
    }

    #[tokio::test]
    async fn test_model_via_server_parses_ok() {
        let server = httpmock::MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method("POST").path("/api/v1/models/test-model/test");
                then.status(200)
                    .header("Content-Type", "application/json")
                    .body(
                        serde_json::json!({
                            "model_id": "test-model",
                            "status": "ok"
                        })
                        .to_string(),
                    );
            })
            .await;

        let client = test_api_client(&server.url(""));
        let response = test_model_via_server(&client, "test-model", None)
            .await
            .unwrap();

        assert_eq!(response.status, api_types::ModelTestResultStatus::Ok);
        assert!(response.error_message.is_none());
    }

    #[tokio::test]
    async fn test_model_via_server_passes_mode_and_parses_error() {
        let server = httpmock::MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method("POST")
                    .path("/api/v1/models/test-model/test")
                    .query_param("mode", "deep");
                then.status(200)
                    .header("Content-Type", "application/json")
                    .body(
                        serde_json::json!({
                            "model_id": "test-model",
                            "status": "error",
                            "error_message": "timeout"
                        })
                        .to_string(),
                    );
            })
            .await;

        let client = test_api_client(&server.url(""));
        let response =
            test_model_via_server(&client, "test-model", Some(api_types::ModelTestMode::Deep))
                .await
                .unwrap();

        assert_eq!(response.status, api_types::ModelTestResultStatus::Error);
        assert_eq!(response.error_message.as_deref(), Some("timeout"));
    }

    #[tokio::test]
    async fn test_model_via_server_404() {
        let server = httpmock::MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method("POST").path("/api/v1/models/bad-model/test");
                then.status(404)
                    .header("Content-Type", "application/json")
                    .body(
                        serde_json::json!({
                            "errors": [{"status": "404", "title": "Not Found", "detail": "Model not found"}]
                        })
                        .to_string(),
                    );
            })
            .await;

        let client = test_api_client(&server.url(""));
        let result = test_model_via_server(&client, "bad-model", None).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Model not found"));
    }

    #[tokio::test]
    async fn fetch_models_from_server_parses_response() {
        let server = httpmock::MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                when.method("GET")
                    .path("/api/v1/models")
                    .query_param("page[limit]", "100")
                    .query_param("page[offset]", "0");
                then.status(200)
                    .header("Content-Type", "application/json")
                    .body(
                        serde_json::json!({
                            "data": [test_model_json("test-model", Provider::Anthropic)],
                            "meta": { "has_more": false }
                        })
                        .to_string(),
                    );
            })
            .await;

        let client = test_api_client(&server.url(""));
        let models = fetch_models_from_server(&client, None, None).await.unwrap();

        mock.assert_async().await;
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "test-model");
        assert_eq!(models[0].provider, Provider::Anthropic);
    }

    #[tokio::test]
    async fn fetch_models_from_server_filters_by_provider() {
        let server = httpmock::MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method("GET")
                    .path("/api/v1/models")
                    .query_param("page[limit]", "100")
                    .query_param("page[offset]", "0")
                    .query_param("provider", "anthropic");
                then.status(200)
                    .header("Content-Type", "application/json")
                    .body(
                        serde_json::json!({
                            "data": [test_model_json("model-a", Provider::Anthropic)],
                            "meta": { "has_more": false }
                        })
                        .to_string(),
                    );
            })
            .await;

        let client = test_api_client(&server.url(""));
        let models = fetch_models_from_server(&client, Some("anthropic"), None)
            .await
            .unwrap();

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "model-a");
    }

    #[tokio::test]
    async fn fetch_models_from_server_passes_query_param() {
        let server = httpmock::MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                when.method("GET")
                    .path("/api/v1/models")
                    .query_param("page[limit]", "100")
                    .query_param("page[offset]", "0")
                    .query_param("query", "sonnet");
                then.status(200)
                    .header("Content-Type", "application/json")
                    .body(
                        serde_json::json!({
                            "data": [test_model_json("claude-sonnet-4-5", Provider::Anthropic)],
                            "meta": { "has_more": false }
                        })
                        .to_string(),
                    );
            })
            .await;

        let client = test_api_client(&server.url(""));
        let models = fetch_models_from_server(&client, None, Some("sonnet"))
            .await
            .unwrap();

        mock.assert_async().await;
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "claude-sonnet-4-5");
    }

    #[tokio::test]
    async fn fetch_models_from_server_follows_pagination() {
        let server = httpmock::MockServer::start_async().await;
        let first_page = server
            .mock_async(|when, then| {
                when.method("GET")
                    .path("/api/v1/models")
                    .query_param("page[limit]", "100")
                    .query_param("page[offset]", "0");
                then.status(200)
                    .header("Content-Type", "application/json")
                    .body(
                        serde_json::json!({
                            "data": [test_model_json("model-a", Provider::Anthropic)],
                            "meta": { "has_more": true }
                        })
                        .to_string(),
                    );
            })
            .await;
        let second_page = server
            .mock_async(|when, then| {
                when.method("GET")
                    .path("/api/v1/models")
                    .query_param("page[limit]", "100")
                    .query_param("page[offset]", "1");
                then.status(200)
                    .header("Content-Type", "application/json")
                    .body(
                        serde_json::json!({
                            "data": [test_model_json("model-b", Provider::OpenAi)],
                            "meta": { "has_more": false }
                        })
                        .to_string(),
                    );
            })
            .await;

        let client = test_api_client(&server.url(""));
        let models = fetch_models_from_server(&client, None, None).await.unwrap();

        first_page.assert_async().await;
        second_page.assert_async().await;
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "model-a");
        assert_eq!(models[1].id, "model-b");
    }

    #[tokio::test]
    async fn fetch_models_from_server_error_on_failure() {
        let server = httpmock::MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method("GET")
                    .path("/api/v1/models")
                    .query_param("page[limit]", "100")
                    .query_param("page[offset]", "0");
                then.status(500).body("internal error");
            })
            .await;

        let client = test_api_client(&server.url(""));
        let result = fetch_models_from_server(&client, None, None).await;
        assert!(result.is_err());
    }
}
