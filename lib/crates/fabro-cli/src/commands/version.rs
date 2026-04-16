use anyhow::Result;
use fabro_types::settings::CliSettings;
use fabro_types::settings::cli::{CliLayer, OutputFormat};
use fabro_util::printer::Printer;
use serde_json::{Map, Value, json};

use crate::args::VersionArgs;
use crate::command_context::CommandContext;
use crate::server_client;
use crate::shared::print_json_pretty;
use crate::user_config::{self, ServerTarget};

pub(crate) async fn version_command(
    args: &VersionArgs,
    cli: &CliSettings,
    cli_layer: &CliLayer,
    printer: Printer,
) -> Result<()> {
    let client = client_info();
    let ctx = CommandContext::for_target(&args.target, printer, cli.clone(), cli_layer)?;
    let server_target = user_config::resolve_server_target(&args.target, ctx.machine_settings())?;
    let server_address = format_server_target(&server_target);
    let server_info = match ctx.server().await {
        Ok(server) => match server
            .api()
            .get_system_info()
            .send()
            .await
            .map_err(server_client::map_api_error)
        {
            Ok(response) => {
                let response = response.into_inner();
                ServerVersionInfo::Success {
                    address: server_address,
                    version: response.version,
                    git_sha: response.git_sha,
                    build_date: response.build_date,
                    os: response.os,
                    arch: response.arch,
                    uptime_secs: response.uptime_secs,
                }
            }
            Err(err) => ServerVersionInfo::Error {
                address: server_address,
                error: err.to_string(),
            },
        },
        Err(err) => ServerVersionInfo::Error {
            address: server_address,
            error: err.to_string(),
        },
    };

    if cli.output.format == OutputFormat::Json {
        print_json_pretty(&json_output(&client, &server_info))?;
        return Ok(());
    }

    print_text_output(&client, &server_info);
    Ok(())
}

struct ClientVersionInfo {
    version: &'static str,
    git_sha: &'static str,
    build_date: &'static str,
    os: &'static str,
    arch: &'static str,
}

enum ServerVersionInfo {
    Success {
        address: String,
        version: Option<String>,
        git_sha: Option<String>,
        build_date: Option<String>,
        os: Option<String>,
        arch: Option<String>,
        uptime_secs: Option<i64>,
    },
    Error {
        address: String,
        error: String,
    },
}

fn client_info() -> ClientVersionInfo {
    ClientVersionInfo {
        version: env!("CARGO_PKG_VERSION"),
        git_sha: env!("FABRO_GIT_SHA"),
        build_date: env!("FABRO_BUILD_DATE"),
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
    }
}

fn format_server_target(target: &ServerTarget) -> String {
    match target {
        ServerTarget::HttpUrl { api_url, .. } => api_url.clone(),
        ServerTarget::UnixSocket(path) => path.display().to_string(),
    }
}

fn json_output(client: &ClientVersionInfo, server: &ServerVersionInfo) -> Value {
    let client = json!({
        "version": client.version,
        "git_sha": client.git_sha,
        "build_date": client.build_date,
        "os": client.os,
        "arch": client.arch,
    });

    let mut server_map = Map::new();
    match server {
        ServerVersionInfo::Success {
            address,
            version,
            git_sha,
            build_date,
            os,
            arch,
            uptime_secs,
        } => {
            server_map.insert("address".to_string(), Value::String(address.clone()));
            if let Some(version) = version {
                server_map.insert("version".to_string(), Value::String(version.clone()));
            }
            if let Some(git_sha) = git_sha {
                server_map.insert("git_sha".to_string(), Value::String(git_sha.clone()));
            }
            if let Some(build_date) = build_date {
                server_map.insert("build_date".to_string(), Value::String(build_date.clone()));
            }
            if let Some(os) = os {
                server_map.insert("os".to_string(), Value::String(os.clone()));
            }
            if let Some(arch) = arch {
                server_map.insert("arch".to_string(), Value::String(arch.clone()));
            }
            if let Some(uptime_secs) = uptime_secs {
                server_map.insert("uptime_secs".to_string(), Value::from(*uptime_secs));
            }
        }
        ServerVersionInfo::Error { address, error } => {
            server_map.insert("address".to_string(), Value::String(address.clone()));
            server_map.insert("error".to_string(), Value::String(error.clone()));
        }
    }

    json!({
        "client": client,
        "server": server_map,
    })
}

#[allow(clippy::print_stdout)]
fn print_text_output(client: &ClientVersionInfo, server: &ServerVersionInfo) {
    println!("Client:");
    println!(" Version:      {}", client.version);
    println!(" Git SHA:      {}", client.git_sha);
    println!(" Build Date:   {}", client.build_date);
    println!(" OS/Arch:      {}/{}", client.os, client.arch);
    println!();

    match server {
        ServerVersionInfo::Success {
            address,
            version,
            git_sha,
            build_date,
            os,
            arch,
            uptime_secs,
        } => {
            println!("Server: {address}");
            println!(" Version:      {}", version.as_deref().unwrap_or("unknown"));
            println!(" Git SHA:      {}", git_sha.as_deref().unwrap_or("unknown"));
            println!(
                " Build Date:   {}",
                build_date.as_deref().unwrap_or("unknown")
            );
            println!(
                " OS/Arch:      {}/{}",
                os.as_deref().unwrap_or("unknown"),
                arch.as_deref().unwrap_or("unknown")
            );
            println!(
                " Uptime:       {}",
                format_uptime(uptime_secs.unwrap_or_default())
            );
        }
        ServerVersionInfo::Error { address, error } => {
            println!("Server: {address}");
            println!(" Error:        {error}");
        }
    }
}

fn format_uptime(total_secs: i64) -> String {
    let total_secs = total_secs.max(0);
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;

    if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        format!("{seconds}s")
    }
}
