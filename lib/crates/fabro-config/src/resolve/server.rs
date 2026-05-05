use fabro_types::settings::InterpString;
use fabro_types::settings::server::{
    DiscordIntegrationSettings, GithubIntegrationSettings, GithubIntegrationStrategy,
    IntegrationWebhooksSettings, IpAllowEntry, ObjectStoreProvider, ObjectStoreSettings,
    ServerApiSettings, ServerArtifactsSettings, ServerAuthGithubSettings, ServerAuthMethod,
    ServerAuthSettings, ServerIntegrationsSettings, ServerIpAllowlistOverrideSettings,
    ServerIpAllowlistSettings, ServerListenSettings, ServerLoggingSettings, ServerNamespace,
    ServerSchedulerSettings, ServerSlateDbSettings, ServerStorageSettings, ServerWebSettings,
    SlackIntegrationSettings, TeamsIntegrationSettings, WebhookStrategy,
};
use fabro_util::Home;

use super::{ResolveError, default_interp, parse_socket_addr, require_interp};
use crate::user::default_storage_dir;
use crate::{
    IntegrationWebhooksLayer, ObjectStoreLocalLayer, ObjectStoreS3Layer, ServerApiLayer,
    ServerArtifactsLayer, ServerAuthLayer, ServerIntegrationsLayer, ServerIpAllowlistLayer,
    ServerIpAllowlistOverrideLayer, ServerLayer, ServerListenLayer, ServerSlateDbLayer,
    ServerStorageLayer, ServerWebLayer,
};

pub fn resolve_server(layer: &ServerLayer, errors: &mut Vec<ResolveError>) -> ServerNamespace {
    let storage = resolve_storage(layer.storage.as_ref());
    let listen = resolve_listen(layer.listen.as_ref(), errors);
    let web = resolve_web(layer.web.as_ref());
    let auth = resolve_auth(layer.auth.as_ref(), errors);
    let ip_allowlist = resolve_ip_allowlist(layer.ip_allowlist.as_ref(), errors);
    let integrations = resolve_integrations(layer.integrations.as_ref(), errors);
    validate_ip_allowlist_for_listen(&listen, &ip_allowlist, errors);
    validate_github_webhook_ip_allowlist_for_listen(&listen, &ip_allowlist, &integrations, errors);
    validate_github_webhook_strategy(&integrations, layer.api.as_ref(), errors);

    ServerNamespace {
        listen,
        api: ServerApiSettings {
            url: layer.api.as_ref().and_then(|api| api.url.clone()),
        },
        web,
        auth,
        ip_allowlist,
        storage: storage.clone(),
        artifacts: resolve_artifacts(layer.artifacts.as_ref(), &storage.root, errors),
        slatedb: resolve_slatedb(layer.slatedb.as_ref(), &storage.root, errors),
        scheduler: ServerSchedulerSettings {
            max_concurrent_runs: layer
                .scheduler
                .as_ref()
                .and_then(|scheduler| scheduler.max_concurrent_runs)
                .expect("defaults.toml should provide server.scheduler.max_concurrent_runs"),
        },
        logging: ServerLoggingSettings {
            level:       layer
                .logging
                .as_ref()
                .and_then(|logging| logging.level.as_ref())
                .map(|level| level.as_str().to_owned()),
            destination: layer
                .logging
                .as_ref()
                .and_then(|logging| logging.destination)
                .unwrap_or_default(),
        },
        integrations,
    }
}

fn resolve_storage(layer: Option<&ServerStorageLayer>) -> ServerStorageSettings {
    ServerStorageSettings {
        root: layer
            .and_then(|storage| storage.root.clone())
            .unwrap_or_else(|| default_interp(default_storage_dir())),
    }
}

fn resolve_listen(
    layer: Option<&ServerListenLayer>,
    errors: &mut Vec<ResolveError>,
) -> ServerListenSettings {
    match layer {
        None => ServerListenSettings::Unix {
            path: default_interp(Home::from_env().socket_path()),
        },
        Some(ServerListenLayer::Unix { path }) => ServerListenSettings::Unix {
            path: path
                .clone()
                .unwrap_or_else(|| default_interp(Home::from_env().socket_path())),
        },
        Some(ServerListenLayer::Tcp { address }) => {
            let address = parse_socket_addr(
                &require_interp(address.as_ref(), "server.listen.address", errors),
                "server.listen.address",
                errors,
            );
            ServerListenSettings::Tcp { address }
        }
    }
}

fn resolve_web(layer: Option<&ServerWebLayer>) -> ServerWebSettings {
    let layer = layer.expect("defaults.toml should provide server.web defaults");

    ServerWebSettings {
        enabled: layer
            .enabled
            .expect("defaults.toml should provide server.web.enabled"),
        url:     layer
            .url
            .clone()
            .expect("defaults.toml should provide server.web.url"),
    }
}

fn resolve_auth(
    layer: Option<&ServerAuthLayer>,
    errors: &mut Vec<ResolveError>,
) -> ServerAuthSettings {
    let methods = if let Some(mut methods) = layer.and_then(|auth| auth.methods.clone()) {
        if methods.is_empty() {
            errors.push(ResolveError::Invalid {
                path:   "server.auth.methods".to_string(),
                reason: "must not be empty".to_string(),
            });
        }
        methods.dedup();
        methods
    } else {
        errors.push(ResolveError::Missing {
            path: "server.auth.methods".to_string(),
        });
        Vec::new()
    };

    let github = layer
        .and_then(|auth| auth.github.as_ref())
        .cloned()
        .unwrap_or_default();
    if methods.contains(&ServerAuthMethod::Github) && github.allowed_usernames.is_empty() {
        errors.push(ResolveError::Invalid {
            path:   "server.auth.github.allowed_usernames".to_string(),
            reason: "must not be empty when github auth is enabled".to_string(),
        });
    }

    ServerAuthSettings {
        methods,
        github: ServerAuthGithubSettings {
            allowed_usernames: github.allowed_usernames,
        },
    }
}

fn resolve_ip_allowlist(
    layer: Option<&ServerIpAllowlistLayer>,
    errors: &mut Vec<ResolveError>,
) -> ServerIpAllowlistSettings {
    let entries = layer
        .and_then(|allowlist| allowlist.entries.as_ref())
        .map(|entries| {
            resolve_ip_allow_entries(entries, "server.ip_allowlist.entries", false, errors)
        })
        .unwrap_or_default();

    ServerIpAllowlistSettings {
        entries,
        trusted_proxy_count: layer
            .and_then(|allowlist| allowlist.trusted_proxy_count)
            .unwrap_or(0),
    }
}

fn effective_ip_allowlist_settings(
    global: &ServerIpAllowlistSettings,
    overlay: Option<&ServerIpAllowlistOverrideSettings>,
) -> ServerIpAllowlistSettings {
    let Some(overlay) = overlay else {
        return global.clone();
    };

    ServerIpAllowlistSettings {
        entries:             overlay
            .entries
            .clone()
            .unwrap_or_else(|| global.entries.clone()),
        trusted_proxy_count: overlay
            .trusted_proxy_count
            .unwrap_or(global.trusted_proxy_count),
    }
}

fn resolve_ip_allowlist_override(
    layer: Option<&ServerIpAllowlistOverrideLayer>,
    path: &str,
    allow_github_meta_hooks: bool,
    errors: &mut Vec<ResolveError>,
) -> Option<ServerIpAllowlistOverrideSettings> {
    layer.map(|allowlist| ServerIpAllowlistOverrideSettings {
        entries:             allowlist.entries.as_ref().map(|entries| {
            resolve_ip_allow_entries(
                entries,
                &format!("{path}.entries"),
                allow_github_meta_hooks,
                errors,
            )
        }),
        trusted_proxy_count: allowlist.trusted_proxy_count,
    })
}

fn resolve_ip_allow_entries(
    entries: &[String],
    path: &str,
    allow_github_meta_hooks: bool,
    errors: &mut Vec<ResolveError>,
) -> Vec<IpAllowEntry> {
    entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            resolve_ip_allow_entry(
                entry,
                &format!("{path}[{index}]"),
                allow_github_meta_hooks,
                errors,
            )
        })
        .collect()
}

fn resolve_ip_allow_entry(
    entry: &str,
    path: &str,
    allow_github_meta_hooks: bool,
    errors: &mut Vec<ResolveError>,
) -> Option<IpAllowEntry> {
    if entry == IpAllowEntry::GITHUB_META_HOOKS_KEYWORD {
        if allow_github_meta_hooks {
            return Some(IpAllowEntry::GitHubMetaHooks);
        }

        errors.push(ResolveError::Invalid {
            path:   path.to_string(),
            reason: format!(
                "`{}` is only valid in server.integrations.github.webhooks.ip_allowlist.entries",
                IpAllowEntry::GITHUB_META_HOOKS_KEYWORD
            ),
        });
        return None;
    }

    match IpAllowEntry::parse_literal(entry) {
        Ok(parsed) => Some(parsed),
        Err(reason) => {
            errors.push(ResolveError::ParseFailure {
                path: path.to_string(),
                reason,
            });
            None
        }
    }
}

fn validate_ip_allowlist_for_listen(
    listen: &ServerListenSettings,
    ip_allowlist: &ServerIpAllowlistSettings,
    errors: &mut Vec<ResolveError>,
) {
    if matches!(listen, ServerListenSettings::Unix { .. })
        && !ip_allowlist.entries.is_empty()
        && ip_allowlist.trusted_proxy_count == 0
    {
        errors.push(ResolveError::Invalid {
            path: "server.ip_allowlist.trusted_proxy_count".to_string(),
            reason: "must be greater than 0 when using a Unix socket listener with a non-empty IP allowlist".to_string(),
        });
    }
}

fn validate_github_webhook_ip_allowlist_for_listen(
    listen: &ServerListenSettings,
    global_ip_allowlist: &ServerIpAllowlistSettings,
    integrations: &ServerIntegrationsSettings,
    errors: &mut Vec<ResolveError>,
) {
    let Some(overlay) = integrations
        .github
        .webhooks
        .as_ref()
        .and_then(|webhooks| webhooks.ip_allowlist.as_ref())
    else {
        return;
    };

    let effective = effective_ip_allowlist_settings(global_ip_allowlist, Some(overlay));
    if effective == *global_ip_allowlist {
        return;
    }

    if matches!(listen, ServerListenSettings::Unix { .. })
        && !effective.entries.is_empty()
        && effective.trusted_proxy_count == 0
    {
        errors.push(ResolveError::Invalid {
            path: "server.integrations.github.webhooks.ip_allowlist.trusted_proxy_count"
                .to_string(),
            reason:
                "must be greater than 0 when using a Unix socket listener with a non-empty GitHub webhook IP allowlist"
                    .to_string(),
        });
    }
}

fn validate_github_webhook_strategy(
    integrations: &ServerIntegrationsSettings,
    api_layer: Option<&ServerApiLayer>,
    errors: &mut Vec<ResolveError>,
) {
    let github = &integrations.github;
    let strategy = github
        .webhooks
        .as_ref()
        .and_then(|webhooks| webhooks.strategy);

    if strategy.is_some()
        && github.strategy == GithubIntegrationStrategy::App
        && github.app_id.is_none()
    {
        errors.push(ResolveError::Invalid {
            path:   "server.integrations.github.app_id".to_string(),
            reason: "must be set when server.integrations.github.webhooks.strategy is configured"
                .to_string(),
        });
    }

    if matches!(strategy, Some(WebhookStrategy::ServerUrl))
        && api_layer.and_then(|api| api.url.as_ref()).is_none()
    {
        errors.push(ResolveError::Invalid {
            path:   "server.api.url".to_string(),
            reason:
                "must be set when server.integrations.github.webhooks.strategy = \"server_url\""
                    .to_string(),
        });
    }
}

fn resolve_artifacts(
    layer: Option<&ServerArtifactsLayer>,
    storage_root: &InterpString,
    errors: &mut Vec<ResolveError>,
) -> ServerArtifactsSettings {
    let provider = layer
        .and_then(|artifacts| artifacts.provider)
        .expect("defaults.toml should provide server.artifacts.provider");

    ServerArtifactsSettings {
        prefix: layer
            .and_then(|artifacts| artifacts.prefix.clone())
            .expect("defaults.toml should provide server.artifacts.prefix"),
        store:  resolve_object_store(
            provider,
            layer.and_then(|artifacts| artifacts.local.as_ref()),
            layer.and_then(|artifacts| artifacts.s3.as_ref()),
            &object_store_default_root(storage_root, "artifacts"),
            "server.artifacts",
            errors,
        ),
    }
}

fn resolve_slatedb(
    layer: Option<&ServerSlateDbLayer>,
    storage_root: &InterpString,
    errors: &mut Vec<ResolveError>,
) -> ServerSlateDbSettings {
    let provider = layer
        .and_then(|slatedb| slatedb.provider)
        .expect("defaults.toml should provide server.slatedb.provider");

    let disk_cache = layer
        .and_then(|slatedb| slatedb.disk_cache)
        .expect("defaults.toml should provide server.slatedb.disk_cache");

    if disk_cache && provider == ObjectStoreProvider::Local {
        tracing::warn!(
            "disk_cache enabled with local provider; \
             disk cache is designed for S3-backed deployments \
             and adds overhead on local filesystems"
        );
    }

    ServerSlateDbSettings {
        prefix: layer
            .and_then(|slatedb| slatedb.prefix.clone())
            .expect("defaults.toml should provide server.slatedb.prefix"),
        store: resolve_object_store(
            provider,
            layer.and_then(|slatedb| slatedb.local.as_ref()),
            layer.and_then(|slatedb| slatedb.s3.as_ref()),
            &object_store_default_root(storage_root, "slatedb"),
            "server.slatedb",
            errors,
        ),
        flush_interval: layer
            .and_then(|slatedb| slatedb.flush_interval)
            .map(|duration| duration.as_std())
            .expect("defaults.toml should provide server.slatedb.flush_interval"),
        disk_cache,
    }
}

fn resolve_object_store(
    provider: ObjectStoreProvider,
    local: Option<&ObjectStoreLocalLayer>,
    s3: Option<&ObjectStoreS3Layer>,
    storage_root: &InterpString,
    path_prefix: &str,
    errors: &mut Vec<ResolveError>,
) -> ObjectStoreSettings {
    match provider {
        ObjectStoreProvider::Local => ObjectStoreSettings::Local {
            root: local
                .and_then(|local| local.root.clone())
                .unwrap_or_else(|| storage_root.clone()),
        },
        ObjectStoreProvider::S3 => {
            let bucket = require_interp(
                s3.and_then(|s3| s3.bucket.as_ref()),
                &format!("{path_prefix}.s3.bucket"),
                errors,
            );
            let region = require_interp(
                s3.and_then(|s3| s3.region.as_ref()),
                &format!("{path_prefix}.s3.region"),
                errors,
            );
            ObjectStoreSettings::S3 {
                bucket,
                region,
                endpoint: s3.and_then(|s3| s3.endpoint.clone()),
                path_style: s3.and_then(|s3| s3.path_style).unwrap_or(false),
            }
        }
    }
}

fn object_store_default_root(storage_root: &InterpString, domain: &str) -> InterpString {
    let root = storage_root.as_source();
    let root = root.trim_end_matches('/');
    InterpString::parse(&format!("{root}/objects/{domain}"))
}

fn resolve_integrations(
    layer: Option<&ServerIntegrationsLayer>,
    errors: &mut Vec<ResolveError>,
) -> ServerIntegrationsSettings {
    ServerIntegrationsSettings {
        github:  layer
            .and_then(|integrations| integrations.github.as_ref())
            .map(|github| GithubIntegrationSettings {
                enabled:   github.enabled.unwrap_or(true),
                strategy:  github.strategy.unwrap_or_default(),
                app_id:    github.app_id.clone(),
                client_id: github.client_id.clone(),
                slug:      github.slug.clone(),
                webhooks:  github.webhooks.as_ref().map(|webhooks| {
                    resolve_github_webhooks(webhooks, "server.integrations.github.webhooks", errors)
                }),
            })
            .unwrap_or_default(),
        slack:   layer
            .and_then(|integrations| integrations.slack.as_ref())
            .map(|slack| SlackIntegrationSettings {
                enabled:         slack.enabled.unwrap_or(true),
                default_channel: slack.default_channel.clone(),
            })
            .unwrap_or_default(),
        discord: layer
            .and_then(|integrations| integrations.discord.as_ref())
            .map(|discord| DiscordIntegrationSettings {
                enabled: discord.enabled.unwrap_or(true),
            })
            .unwrap_or_default(),
        teams:   layer
            .and_then(|integrations| integrations.teams.as_ref())
            .map(|teams| TeamsIntegrationSettings {
                enabled: teams.enabled.unwrap_or(true),
            })
            .unwrap_or_default(),
    }
}

fn resolve_github_webhooks(
    layer: &IntegrationWebhooksLayer,
    path: &str,
    errors: &mut Vec<ResolveError>,
) -> IntegrationWebhooksSettings {
    IntegrationWebhooksSettings {
        strategy:     layer.strategy,
        ip_allowlist: resolve_ip_allowlist_override(
            layer.ip_allowlist.as_ref(),
            &format!("{path}.ip_allowlist"),
            true,
            errors,
        ),
    }
}
