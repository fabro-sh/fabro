use std::time::Duration;

use fabro_types::settings::InterpString;
use fabro_types::settings::server::{
    DiscordIntegrationSettings, GithubIntegrationSettings, GithubOauthSettings,
    IntegrationWebhooksSettings, ObjectStoreLocalLayer, ObjectStoreProvider, ObjectStoreS3Layer,
    ObjectStoreSettings, ServerApiLayer, ServerApiSettings, ServerArtifactsLayer,
    ServerArtifactsSettings, ServerAuthApiJwtSettings, ServerAuthApiMtlsSettings,
    ServerAuthApiSettings, ServerAuthLayer, ServerAuthSettings, ServerAuthWebGithubLayer,
    ServerAuthWebProvidersSettings, ServerAuthWebSettings, ServerIntegrationsLayer,
    ServerIntegrationsSettings, ServerLayer, ServerListenLayer, ServerListenSettings,
    ServerListenTlsLayer, ServerLoggingSettings, ServerSchedulerSettings, ServerSettings,
    ServerSlateDbLayer, ServerSlateDbSettings, ServerStorageLayer, ServerStorageSettings,
    ServerWebLayer, ServerWebSettings, SlackIntegrationSettings, TeamsIntegrationSettings,
    TlsConfig,
};
use fabro_util::Home;

use super::{ResolveError, default_interp, parse_socket_addr, require_interp};

pub fn resolve_server(layer: &ServerLayer, errors: &mut Vec<ResolveError>) -> ServerSettings {
    let storage = resolve_storage(layer.storage.as_ref());
    let (listen, valid_tls) = resolve_listen(layer.listen.as_ref(), errors);
    let web = resolve_web(layer.api.as_ref(), layer.web.as_ref());
    let auth = resolve_auth(layer.auth.as_ref(), valid_tls, errors);

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
                .unwrap_or(5),
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
    let ca = require_interp(layer.ca.as_ref(), "server.listen.tls.ca", errors);
    let valid = layer.cert.is_some() && layer.key.is_some() && layer.ca.is_some();

    (Some(TlsConfig { cert, key, ca }), valid)
}

fn resolve_web(_api: Option<&ServerApiLayer>, layer: Option<&ServerWebLayer>) -> ServerWebSettings {
    ServerWebSettings {
        enabled: layer.and_then(|web| web.enabled).unwrap_or(true),
        url:     layer
            .and_then(|web| web.url.clone())
            .unwrap_or_else(|| InterpString::parse("http://localhost:3000")),
    }
}

fn resolve_auth(
    layer: Option<&ServerAuthLayer>,
    valid_tls: bool,
    errors: &mut Vec<ResolveError>,
) -> ServerAuthSettings {
    let api = layer.and_then(|auth| auth.api.as_ref());
    let web = layer.and_then(|auth| auth.web.as_ref());

    let jwt = api.and_then(|api| {
        api.jwt.as_ref().map(|jwt| ServerAuthApiJwtSettings {
            enabled:  jwt.enabled.unwrap_or(true),
            issuer:   jwt.issuer.clone(),
            audience: jwt.audience.clone(),
        })
    });
    let mtls = api.and_then(|api| {
        api.mtls.as_ref().map(|mtls| ServerAuthApiMtlsSettings {
            enabled: mtls.enabled.unwrap_or(true),
            ca:      mtls.ca.clone(),
        })
    });
    if mtls.as_ref().is_some_and(|mtls| mtls.enabled) && !valid_tls {
        errors.push(ResolveError::Invalid {
            path:   "server.auth.api.mtls".to_string(),
            reason: "requires tcp listen with tls cert, key, and ca configured".to_string(),
        });
    }

    ServerAuthSettings {
        api: ServerAuthApiSettings { jwt, mtls },
        web: ServerAuthWebSettings {
            allowed_usernames: web
                .map(|web| web.allowed_usernames.clone())
                .unwrap_or_default(),
            providers:         ServerAuthWebProvidersSettings {
                github: web
                    .and_then(|web| web.providers.as_ref())
                    .and_then(|providers| providers.github.as_ref())
                    .map(resolve_web_github),
            },
        },
    }
}

fn resolve_web_github(layer: &ServerAuthWebGithubLayer) -> GithubOauthSettings {
    GithubOauthSettings {
        enabled:       layer.enabled.unwrap_or(true),
        client_id:     layer.client_id.clone(),
        client_secret: layer.client_secret.clone(),
    }
}

fn resolve_artifacts(
    layer: Option<&ServerArtifactsLayer>,
    storage_root: &InterpString,
    errors: &mut Vec<ResolveError>,
) -> ServerArtifactsSettings {
    let provider = layer
        .and_then(|artifacts| artifacts.provider)
        .unwrap_or(ObjectStoreProvider::Local);

    ServerArtifactsSettings {
        prefix: layer
            .and_then(|artifacts| artifacts.prefix.clone())
            .unwrap_or_else(|| InterpString::parse("artifacts")),
        store:  resolve_object_store(
            provider,
            layer.and_then(|artifacts| artifacts.local.as_ref()),
            layer.and_then(|artifacts| artifacts.s3.as_ref()),
            storage_root,
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
        .unwrap_or(ObjectStoreProvider::Local);

    ServerSlateDbSettings {
        prefix:         layer
            .and_then(|slatedb| slatedb.prefix.clone())
            .unwrap_or_else(|| InterpString::parse("")),
        store:          resolve_object_store(
            provider,
            layer.and_then(|slatedb| slatedb.local.as_ref()),
            layer.and_then(|slatedb| slatedb.s3.as_ref()),
            storage_root,
            "server.slatedb",
            errors,
        ),
        flush_interval: layer
            .and_then(|slatedb| slatedb.flush_interval)
            .map_or_else(|| Duration::from_millis(1), |duration| duration.as_std()),
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

fn resolve_integrations(layer: Option<&ServerIntegrationsLayer>) -> ServerIntegrationsSettings {
    ServerIntegrationsSettings {
        github:  layer
            .and_then(|integrations| integrations.github.as_ref())
            .map(|github| GithubIntegrationSettings {
                enabled:     github.enabled.unwrap_or(true),
                strategy:    github.strategy.unwrap_or_default(),
                app_id:      github.app_id.clone(),
                client_id:   github.client_id.clone(),
                slug:        github.slug.clone(),
                permissions: github.permissions.clone(),
                webhooks:    github
                    .webhooks
                    .as_ref()
                    .map(|webhooks| IntegrationWebhooksSettings {
                        strategy: webhooks.strategy,
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
