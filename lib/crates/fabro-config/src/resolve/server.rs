use fabro_types::settings::InterpString;
use fabro_types::settings::server::{
    GithubIntegrationSettings, GithubIntegrationStrategy, IntegrationWebhooksSettings,
    ObjectStoreProvider, ObjectStoreSettings, ServerApiSettings, ServerArtifactsSettings,
    ServerAuthGithubSettings, ServerAuthMethod, ServerAuthSettings, ServerIntegrationsSettings,
    ServerListenSettings, ServerLoggingSettings, ServerNamespace, ServerSandboxProviderSettings,
    ServerSandboxProvidersSettings, ServerSandboxSettings, ServerSchedulerSettings,
    ServerSlateDbSettings, ServerStorageSettings, ServerWebSettings, SlackIntegrationSettings,
    WebhookStrategy,
};
use fabro_util::Home;

use super::{ResolveError, default_interp, parse_socket_addr, require_interp};
use crate::user::default_storage_dir;
use crate::{
    IntegrationWebhooksLayer, ObjectStoreLocalLayer, ObjectStoreS3Layer, ServerApiLayer,
    ServerArtifactsLayer, ServerAuthLayer, ServerIntegrationsLayer, ServerLayer, ServerListenLayer,
    ServerSandboxLayer, ServerSandboxProviderLayer, ServerSlateDbLayer, ServerStorageLayer,
    ServerWebLayer,
};

pub fn resolve_server(layer: &ServerLayer, errors: &mut Vec<ResolveError>) -> ServerNamespace {
    let storage = resolve_storage(layer.storage.as_ref());
    let listen = resolve_listen(layer.listen.as_ref(), errors);
    let web = resolve_web(layer.web.as_ref());
    let auth = resolve_auth(layer.auth.as_ref(), errors);
    let integrations = resolve_integrations(layer.integrations.as_ref());
    validate_github_webhook_strategy(&integrations, layer.api.as_ref(), errors);

    ServerNamespace {
        listen,
        api: ServerApiSettings {
            url: layer.api.as_ref().and_then(|api| api.url.clone()),
        },
        web,
        auth,
        sandbox: resolve_sandbox(layer.sandbox.as_ref()),
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

fn resolve_sandbox(layer: Option<&ServerSandboxLayer>) -> ServerSandboxSettings {
    let providers = layer.and_then(|sandbox| sandbox.providers.as_ref());
    ServerSandboxSettings {
        providers: ServerSandboxProvidersSettings {
            local:   resolve_sandbox_provider(
                providers.and_then(|providers| providers.local.as_ref()),
            ),
            docker:  resolve_sandbox_provider(
                providers.and_then(|providers| providers.docker.as_ref()),
            ),
            daytona: resolve_sandbox_provider(
                providers.and_then(|providers| providers.daytona.as_ref()),
            ),
        },
    }
}

fn resolve_sandbox_provider(
    layer: Option<&ServerSandboxProviderLayer>,
) -> ServerSandboxProviderSettings {
    ServerSandboxProviderSettings {
        enabled: layer.and_then(|provider| provider.enabled).unwrap_or(true),
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

#[expect(
    clippy::disallowed_methods,
    reason = "derives sibling default paths in source form; the result is re-parsed as an \
              InterpString and resolves at consumption"
)]
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
                enabled:   github.enabled.unwrap_or(true),
                strategy:  github.strategy.unwrap_or_default(),
                app_id:    github.app_id.clone(),
                client_id: github.client_id.clone(),
                slug:      github.slug.clone(),
                webhooks:  github.webhooks.as_ref().map(resolve_github_webhooks),
            })
            .unwrap_or_default(),
        slack:  layer
            .and_then(|integrations| integrations.slack.as_ref())
            .map_or(
                SlackIntegrationSettings {
                    enabled:         false,
                    default_channel: None,
                },
                |slack| SlackIntegrationSettings {
                    enabled:         slack.enabled.unwrap_or(true),
                    default_channel: slack.default_channel.clone(),
                },
            ),
    }
}

fn resolve_github_webhooks(layer: &IntegrationWebhooksLayer) -> IntegrationWebhooksSettings {
    IntegrationWebhooksSettings {
        strategy: layer.strategy,
    }
}
