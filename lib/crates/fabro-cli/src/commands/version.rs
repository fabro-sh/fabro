#![expect(
    clippy::disallowed_methods,
    reason = "sync CLI `version` command: writes version info to std::io::stderr"
)]

use std::io::IsTerminal;

use anyhow::Result;
use fabro_util::printer::Printer;
use serde_json::{Map, Value, json};

use crate::args::VersionArgs;
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;
use crate::user_config::{self, ServerTarget};

pub(crate) async fn version_command(args: &VersionArgs, base_ctx: &CommandContext) -> Result<()> {
    let client = client_info();
    let printer = base_ctx.printer();
    let ctx = base_ctx.with_target(&args.target)?;
    let server_target = user_config::resolve_server_target(&args.target, ctx.machine_settings())?;
    let server_address = format_server_target(&server_target);
    let server_info = match ctx.server().await {
        Ok(server) => match server.get_system_info().await {
            Ok(response) => ServerVersionInfo::Success {
                address:     server_address,
                version:     response.version,
                git_sha:     response.git_sha,
                build_date:  response.build_date,
                profile:     response.profile,
                os:          response.os,
                arch:        response.arch,
                uptime_secs: response.uptime_secs,
            },
            Err(err) => ServerVersionInfo::Error {
                address: server_address,
                error:   err.to_string(),
            },
        },
        Err(err) => ServerVersionInfo::Error {
            address: server_address,
            error:   err.to_string(),
        },
    };

    if ctx.json_output() {
        print_json_pretty(&json_output(&client, &server_info))?;
        return Ok(());
    }

    print_text_output(&client, &server_info);
    warn_on_version_mismatch(&client, &server_info, printer);
    Ok(())
}

fn warn_on_version_mismatch(
    client: &ClientVersionInfo,
    server: &ServerVersionInfo,
    printer: Printer,
) {
    if !std::io::stderr().is_terminal() {
        return;
    }
    let Some(message) = version_mismatch_message(client.version, server) else {
        return;
    };
    let yellow = console::Style::new().yellow();
    fabro_util::printerr!(printer, "\n{} {}", yellow.apply_to("warning:"), message);
}

fn version_mismatch_message(client_version: &str, server: &ServerVersionInfo) -> Option<String> {
    let ServerVersionInfo::Success {
        version: Some(server_version),
        ..
    } = server
    else {
        return None;
    };
    if server_version == client_version {
        return None;
    }
    Some(format!(
        "client version ({client_version}) does not match server version ({server_version})"
    ))
}

struct ClientVersionInfo {
    version:    &'static str,
    git_sha:    &'static str,
    build_date: &'static str,
    profile:    &'static str,
    os:         &'static str,
    arch:       &'static str,
}

enum ServerVersionInfo {
    Success {
        address:     String,
        version:     Option<String>,
        git_sha:     Option<String>,
        build_date:  Option<String>,
        profile:     Option<String>,
        os:          Option<String>,
        arch:        Option<String>,
        uptime_secs: Option<i64>,
    },
    Error {
        address: String,
        error:   String,
    },
}

fn client_info() -> ClientVersionInfo {
    ClientVersionInfo {
        version:    env!("CARGO_PKG_VERSION"),
        git_sha:    env!("FABRO_GIT_SHA"),
        build_date: env!("FABRO_BUILD_DATE"),
        profile:    env!("FABRO_BUILD_PROFILE"),
        os:         std::env::consts::OS,
        arch:       std::env::consts::ARCH,
    }
}

fn is_non_release_profile(profile: &str) -> bool {
    !profile.is_empty() && profile != "release"
}

fn format_server_target(target: &ServerTarget) -> String {
    if let Some(api_url) = target.as_http_url() {
        api_url.to_string()
    } else {
        target
            .as_unix_socket_path()
            .map(|path| path.display().to_string())
            .unwrap_or_default()
    }
}

fn json_output(client: &ClientVersionInfo, server: &ServerVersionInfo) -> Value {
    let client = json!({
        "version": client.version,
        "git_sha": client.git_sha,
        "build_date": client.build_date,
        "profile": client.profile,
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
            profile,
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
            if let Some(profile) = profile {
                server_map.insert("profile".to_string(), Value::String(profile.clone()));
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

#[allow(
    clippy::print_stdout,
    reason = "The version report is the command's primary stdout output."
)]
fn print_text_output(client: &ClientVersionInfo, server: &ServerVersionInfo) {
    println!("Client:");
    println!(" Version:      {}", client.version);
    println!(" Git SHA:      {}", client.git_sha);
    println!(" Build Date:   {}", client.build_date);
    if is_non_release_profile(client.profile) {
        println!(" Profile:      {}", client.profile);
    }
    println!(" OS/Arch:      {}/{}", client.os, client.arch);
    println!();

    match server {
        ServerVersionInfo::Success {
            address,
            version,
            git_sha,
            build_date,
            profile,
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
            if let Some(profile) = profile.as_deref().filter(|p| is_non_release_profile(p)) {
                println!(" Profile:      {profile}");
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_mismatch_message_covers_all_branches() {
        let matching = ServerVersionInfo::Success {
            address:     "http://localhost".into(),
            version:     Some("1.0.0".into()),
            git_sha:     None,
            build_date:  None,
            profile:     None,
            os:          None,
            arch:        None,
            uptime_secs: None,
        };
        assert_eq!(version_mismatch_message("1.0.0", &matching), None);

        let mismatched = ServerVersionInfo::Success {
            address:     "http://localhost".into(),
            version:     Some("1.2.0".into()),
            git_sha:     None,
            build_date:  None,
            profile:     None,
            os:          None,
            arch:        None,
            uptime_secs: None,
        };
        assert_eq!(
            version_mismatch_message("1.0.0", &mismatched).as_deref(),
            Some("client version (1.0.0) does not match server version (1.2.0)")
        );

        let unknown = ServerVersionInfo::Success {
            address:     "http://localhost".into(),
            version:     None,
            git_sha:     None,
            build_date:  None,
            profile:     None,
            os:          None,
            arch:        None,
            uptime_secs: None,
        };
        assert_eq!(version_mismatch_message("1.0.0", &unknown), None);

        let errored = ServerVersionInfo::Error {
            address: "http://localhost".into(),
            error:   "oops".into(),
        };
        assert_eq!(version_mismatch_message("1.0.0", &errored), None);
    }
}
