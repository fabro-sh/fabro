use fabro_types::settings::InterpString;
use fabro_types::settings::server::{
    DiscordIntegrationSettings, GithubIntegrationSettings, IntegrationWebhooksSettings,
    ObjectStoreLocalLayer, ObjectStoreProvider, ObjectStoreS3Layer, ObjectStoreSettings,
    ServerApiLayer, ServerApiSettings, ServerArtifactsLayer, ServerArtifactsSettings,
    ServerAuthGithubSettings, ServerAuthLayer, ServerAuthMethod, ServerAuthSettings,
    ServerIntegrationsLayer, ServerIntegrationsSettings, ServerLayer, ServerListenLayer,
    ServerListenSettings, ServerListenTlsLayer, ServerLoggingSettings, ServerSchedulerSettings,
    ServerSettings, ServerSlateDbLayer, ServerSlateDbSettings, ServerStorageLayer,
    ServerStorageSettings, ServerWebLayer, ServerWebSettings, SlackIntegrationSettings,
    TeamsIntegrationSettings, TlsConfig,
};
use fabro_util::Home;

use super::{ResolveError, default_interp, parse_socket_addr, require_interp};

pub fn resolve_server(layer: &ServerLayer, errors: &mut Vec<ResolveError>) -> ServerSettings {
    let storage = resolve_storage(layer.storage.as_ref());
    let (listen, _valid_tls) = resolve_listen(layer.listen.as_ref(), errors);
    let web = resolve_web(layer.api.as_ref(), layer.web.as_ref());
    let auth = resolve_auth(layer.auth.as_ref(), errors);

    ServerSettings {
        listen,
        api: ServerApiSettings {
            url: layer.api.as_ref().and_then(|api| api.url.clone()),
        },
        web,
        auth,
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
            level: layer
                .logging
                .as_ref()
                .and_then(|logging| logging.level.clone()),
        },
        integrations: resolve_integrations(layer.integrations.as_ref()),
    }
}

fn resolve_storage(layer: Option<&ServerStorageLayer>) -> ServerStorageSettings {
    ServerStorageSettings {
        root: layer
            .and_then(|storage| storage.root.clone())
            .unwrap_or_else(|| default_interp(Home::from_env().storage_dir())),
    }
}

fn resolve_listen(
    layer: Option<&ServerListenLayer>,
    errors: &mut Vec<ResolveError>,
) -> (ServerListenSettings, bool) {
    match layer {
        None => (
            ServerListenSettings::Unix {
                path: default_interp(Home::from_env().socket_path()),
            },
            false,
        ),
        Some(ServerListenLayer::Unix { path }) => (
            ServerListenSettings::Unix {
                path: path
                    .clone()
                    .unwrap_or_else(|| default_interp(Home::from_env().socket_path())),
            },
            false,
        ),
        Some(ServerListenLayer::Tcp { address, tls }) => {
            let address = parse_socket_addr(
                &require_interp(address.as_ref(), "server.listen.address", errors),
                "server.listen.address",
                errors,
            );
            let (tls, valid_tls) = resolve_tls(tls.as_ref(), errors);
            (ServerListenSettings::Tcp { address, tls }, valid_tls)
        }
    }
}

fn resolve_tls(
    layer: Option<&ServerListenTlsLayer>,
    errors: &mut Vec<ResolveError>,
) -> (Option<TlsConfig>, bool) {
    let Some(layer) = layer else {
        return (None, false);
    };

    let cert = require_interp(layer.cert.as_ref(), "server.listen.tls.cert", errors);
    let key = require_interp(layer.key.as_ref(), "server.listen.tls.key", errors);
    let valid = layer.cert.is_some() && layer.key.is_some();

    (Some(TlsConfig { cert, key }), valid)
}

fn resolve_web(_api: Option<&ServerApiLayer>, layer: Option<&ServerWebLayer>) -> ServerWebSettings {
    let layer = layer.expect("defaults.toml should provide server.web defaults");

    ServerWebSettings {
        enabled: layer
            .enabled
            .expect("defaults.toml should provide server.web.enabled"),
        url: layer
            .url
            .clone()
            .expect("defaults.toml should provide server.web.url"),
    }
}

fn resolve_auth(
    layer: Option<&ServerAuthLayer>,
    errors: &mut Vec<ResolveError>,
) -> ServerAuthSettings {
    let mut methods = layer
        .and_then(|auth| auth.methods.clone())
        .unwrap_or_else(|| vec![ServerAuthMethod::DevToken]);
    if methods.is_empty() {
        errors.push(ResolveError::Invalid {
            path: "server.auth.methods".to_string(),
            reason: "must not be empty".to_string(),
        });
    }
    methods.dedup();

    let github = layer
        .and_then(|auth| auth.github.as_ref())
        .cloned()
        .unwrap_or_default();
    if methods.contains(&ServerAuthMethod::Github) && github.allowed_usernames.is_empty() {
        errors.push(ResolveError::Invalid {
            path: "server.auth.github.allowed_usernames".to_string(),
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
        store: resolve_object_store(
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

fn resolve_integrations(layer: Option<&ServerIntegrationsLayer>) -> ServerIntegrationsSettings {
    ServerIntegrationsSettings {
        github: layer
            .and_then(|integrations| integrations.github.as_ref())
            .map(|github| GithubIntegrationSettings {
                enabled: github.enabled.unwrap_or(true),
                strategy: github.strategy.unwrap_or_default(),
                app_id: github.app_id.clone(),
                client_id: github.client_id.clone(),
                slug: github.slug.clone(),
                permissions: github.permissions.clone(),
                webhooks: github
                    .webhooks
                    .as_ref()
                    .map(|webhooks| IntegrationWebhooksSettings {
                        strategy: webhooks.strategy,
                    }),
            })
            .unwrap_or_default(),
        slack: layer
            .and_then(|integrations| integrations.slack.as_ref())
            .map(|slack| SlackIntegrationSettings {
                enabled: slack.enabled.unwrap_or(true),
                default_channel: slack.default_channel.clone(),
            })
            .unwrap_or_default(),
        discord: layer
            .and_then(|integrations| integrations.discord.as_ref())
            .map(|discord| DiscordIntegrationSettings {
                enabled: discord.enabled.unwrap_or(true),
            })
            .unwrap_or_default(),
        teams: layer
            .and_then(|integrations| integrations.teams.as_ref())
            .map(|teams| TeamsIntegrationSettings {
                enabled: teams.enabled.unwrap_or(true),
            })
            .unwrap_or_default(),
    }
}
