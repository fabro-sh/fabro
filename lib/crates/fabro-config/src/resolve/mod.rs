mod error;
mod server;

use fabro_types::settings::{ServerSettings, SettingsFile};

pub use error::ResolveError;
pub use server::resolve_server;

pub fn resolve_server_from_file(file: &SettingsFile) -> Result<ServerSettings, Vec<ResolveError>> {
    let mut errors = Vec::new();
    let layer = file.server.as_ref().cloned().unwrap_or_default();
    let resolved = resolve_server(&layer, &mut errors);
    if errors.is_empty() {
        Ok(resolved)
    } else {
        Err(errors)
    }
}

pub(crate) fn require_interp(
    value: Option<&fabro_types::settings::InterpString>,
    path: &str,
    errors: &mut Vec<ResolveError>,
) -> fabro_types::settings::InterpString {
    value.cloned().unwrap_or_else(|| {
        errors.push(ResolveError::Missing {
            path: path.to_string(),
        });
        fabro_types::settings::InterpString::parse("")
    })
}

pub(crate) fn parse_socket_addr(
    value: &fabro_types::settings::InterpString,
    path: &str,
    errors: &mut Vec<ResolveError>,
) -> std::net::SocketAddr {
    let source = value.as_source();
    match source.parse::<std::net::SocketAddr>() {
        Ok(address) => address,
        Err(err) => {
            errors.push(ResolveError::ParseFailure {
                path: path.to_string(),
                reason: err.to_string(),
            });
            std::net::SocketAddr::from(([127, 0, 0, 1], 0))
        }
    }
}

pub(crate) fn default_interp(
    path: impl AsRef<std::path::Path>,
) -> fabro_types::settings::InterpString {
    fabro_types::settings::InterpString::parse(&path.as_ref().to_string_lossy())
}
