use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Extracted configuration from a Docker Compose service.
#[derive(Debug, Clone, Default)]
pub(crate) struct ComposeServiceConfig {
    pub image: Option<String>,
    pub build: Option<ComposeBuild>,
    pub ports: Vec<u16>,
    pub environment: HashMap<String, String>,
    pub user: Option<String>,
}

/// Build configuration from a Docker Compose service.
#[derive(Debug, Clone)]
pub(crate) struct ComposeBuild {
    pub context: String,
    pub dockerfile: Option<String>,
}

/// Parse a Docker Compose file and extract config for the named service.
pub(crate) fn parse_compose(
    compose_path: &Path,
    service_name: &str,
) -> Result<ComposeServiceConfig, String> {
    let contents = std::fs::read_to_string(compose_path)
        .map_err(|e| format!("failed to read compose file: {e}"))?;

    let doc: serde_yaml::Value =
        serde_yaml::from_str(&contents).map_err(|e| format!("failed to parse YAML: {e}"))?;

    let service = doc
        .get("services")
        .and_then(|s| s.get(service_name))
        .ok_or_else(|| format!("service '{service_name}' not found in compose file"))?;

    let image = service
        .get("image")
        .and_then(|v| v.as_str())
        .map(String::from);

    let build = parse_build(service);
    let ports = parse_ports(service);
    let environment = parse_environment(service);

    let user = service
        .get("user")
        .and_then(|v| v.as_str())
        .map(String::from);

    Ok(ComposeServiceConfig {
        image,
        build,
        ports,
        environment,
        user,
    })
}

fn parse_build(service: &serde_yaml::Value) -> Option<ComposeBuild> {
    let build_val = service.get("build")?;

    if let Some(context) = build_val.as_str() {
        return Some(ComposeBuild {
            context: context.to_string(),
            dockerfile: None,
        });
    }

    if build_val.is_mapping() {
        let context = build_val
            .get("context")
            .and_then(|v| v.as_str())
            .unwrap_or(".")
            .to_string();
        let dockerfile = build_val
            .get("dockerfile")
            .and_then(|v| v.as_str())
            .map(String::from);
        return Some(ComposeBuild {
            context,
            dockerfile,
        });
    }

    None
}

fn parse_ports(service: &serde_yaml::Value) -> Vec<u16> {
    let Some(ports_val) = service.get("ports") else {
        return Vec::new();
    };
    let Some(ports_seq) = ports_val.as_sequence() else {
        return Vec::new();
    };

    ports_seq
        .iter()
        .filter_map(|entry| {
            if let Some(n) = entry.as_u64() {
                return u16::try_from(n).ok();
            }
            if let Some(s) = entry.as_str() {
                // Formats: "8080:80", "3000", "8080:80/tcp"
                let s = s.split('/').next().unwrap_or(s); // strip protocol
                return if let Some((_host, container)) = s.split_once(':') {
                    container.parse::<u16>().ok()
                } else {
                    s.parse::<u16>().ok()
                };
            }
            None
        })
        .collect()
}

fn parse_environment(service: &serde_yaml::Value) -> HashMap<String, String> {
    let Some(env_val) = service.get("environment") else {
        return HashMap::new();
    };

    // Array form: ["KEY=VALUE", ...]
    if let Some(seq) = env_val.as_sequence() {
        return seq
            .iter()
            .filter_map(|v| {
                let s = v.as_str()?;
                let (key, value) = s.split_once('=')?;
                Some((key.to_string(), value.to_string()))
            })
            .collect();
    }

    // Object form: { KEY: VALUE, ... }
    if let Some(mapping) = env_val.as_mapping() {
        return mapping
            .iter()
            .filter_map(|(k, v)| {
                let key = k.as_str()?.to_string();
                let value = match v {
                    serde_yaml::Value::String(s) => s.clone(),
                    serde_yaml::Value::Number(n) => n.to_string(),
                    serde_yaml::Value::Bool(b) => b.to_string(),
                    serde_yaml::Value::Null => String::new(),
                    _ => return None,
                };
                Some((key, value))
            })
            .collect();
    }

    HashMap::new()
}

/// Parse multiple Docker Compose files and merge config for the named service.
/// Later files override earlier files for image/build/user; ports accumulate (deduped);
/// environment keys from later files override earlier ones.
pub(crate) fn parse_compose_multi(
    compose_paths: &[PathBuf],
    service_name: &str,
) -> Result<ComposeServiceConfig, String> {
    let mut merged = ComposeServiceConfig::default();
    let mut found_service = false;

    for path in compose_paths {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read compose file {}: {e}", path.display()))?;

        let doc: serde_yaml::Value = serde_yaml::from_str(&contents)
            .map_err(|e| format!("failed to parse YAML {}: {e}", path.display()))?;

        let Some(service) = doc.get("services").and_then(|s| s.get(service_name)) else {
            continue;
        };
        found_service = true;

        if let Some(image) = service.get("image").and_then(|v| v.as_str()) {
            merged.image = Some(image.to_string());
        }

        if let Some(build) = parse_build(service) {
            merged.build = Some(build);
        }

        if let Some(user) = service.get("user").and_then(|v| v.as_str()) {
            merged.user = Some(user.to_string());
        }

        for port in parse_ports(service) {
            if !merged.ports.contains(&port) {
                merged.ports.push(port);
            }
        }

        for (k, v) in parse_environment(service) {
            merged.environment.insert(k, v);
        }
    }

    if !found_service {
        return Err(format!(
            "service '{service_name}' not found in any compose file"
        ));
    }

    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_compose(content: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    #[test]
    fn service_with_image_only() {
        let f = write_compose(
            r"
services:
  web:
    image: nginx:latest
",
        );
        let cfg = parse_compose(f.path(), "web").unwrap();
        assert_eq!(cfg.image.as_deref(), Some("nginx:latest"));
        assert!(cfg.build.is_none());
        assert!(cfg.ports.is_empty());
        assert!(cfg.environment.is_empty());
        assert!(cfg.user.is_none());
    }

    #[test]
    fn service_with_build_string() {
        let f = write_compose(
            r"
services:
  app:
    build: ./src
",
        );
        let cfg = parse_compose(f.path(), "app").unwrap();
        let build = cfg.build.unwrap();
        assert_eq!(build.context, "./src");
        assert!(build.dockerfile.is_none());
    }

    #[test]
    fn service_with_build_object() {
        let f = write_compose(
            r"
services:
  app:
    build:
      context: ./app
      dockerfile: Dockerfile.dev
",
        );
        let cfg = parse_compose(f.path(), "app").unwrap();
        let build = cfg.build.unwrap();
        assert_eq!(build.context, "./app");
        assert_eq!(build.dockerfile.as_deref(), Some("Dockerfile.dev"));
    }

    #[test]
    fn ports_various_formats() {
        let f = write_compose(
            r#"
services:
  web:
    image: nginx
    ports:
      - "8080:80"
      - "3000"
      - 5432
      - "9090:9090/tcp"
"#,
        );
        let cfg = parse_compose(f.path(), "web").unwrap();
        assert_eq!(cfg.ports, vec![80, 3000, 5432, 9090]);
    }

    #[test]
    fn environment_as_array() {
        let f = write_compose(
            r#"
services:
  app:
    image: myapp
    environment:
      - "DATABASE_URL=postgres://localhost/db"
      - "DEBUG=true"
"#,
        );
        let cfg = parse_compose(f.path(), "app").unwrap();
        assert_eq!(cfg.environment.len(), 2);
        assert_eq!(cfg.environment["DATABASE_URL"], "postgres://localhost/db");
        assert_eq!(cfg.environment["DEBUG"], "true");
    }

    #[test]
    fn environment_as_object() {
        let f = write_compose(
            r"
services:
  app:
    image: myapp
    environment:
      RAILS_ENV: production
      PORT: 3000
",
        );
        let cfg = parse_compose(f.path(), "app").unwrap();
        assert_eq!(cfg.environment.len(), 2);
        assert_eq!(cfg.environment["RAILS_ENV"], "production");
        assert_eq!(cfg.environment["PORT"], "3000");
    }

    #[test]
    fn service_not_found() {
        let f = write_compose(
            r"
services:
  web:
    image: nginx
",
        );
        let err = parse_compose(f.path(), "missing").unwrap_err();
        assert!(err.contains("service 'missing' not found"));
    }

    #[test]
    fn file_not_found() {
        let err = parse_compose(Path::new("/nonexistent/docker-compose.yml"), "web").unwrap_err();
        assert!(err.contains("failed to read compose file"));
    }

    #[test]
    fn service_with_user() {
        let f = write_compose(
            r#"
services:
  app:
    image: myapp
    user: "1000:1000"
"#,
        );
        let cfg = parse_compose(f.path(), "app").unwrap();
        assert_eq!(cfg.user.as_deref(), Some("1000:1000"));
    }

    #[test]
    fn multi_compose_merge() {
        let base = write_compose(
            r#"
services:
  app:
    image: node:20
    ports:
      - "3000:3000"
    environment:
      - "NODE_ENV=development"
"#,
        );
        let over = write_compose(
            r#"
services:
  app:
    image: node:22
    ports:
      - "3000:3000"
      - "9229:9229"
    environment:
      - "DEBUG=true"
"#,
        );
        let paths = vec![base.path().to_path_buf(), over.path().to_path_buf()];
        let cfg = parse_compose_multi(&paths, "app").unwrap();
        assert_eq!(cfg.image.as_deref(), Some("node:22"));
        assert_eq!(cfg.ports, vec![3000, 9229]);
        assert_eq!(cfg.environment["NODE_ENV"], "development");
        assert_eq!(cfg.environment["DEBUG"], "true");
    }

    #[test]
    fn multi_compose_service_not_found() {
        let f = write_compose(
            r"
services:
  web:
    image: nginx
",
        );
        let paths = vec![f.path().to_path_buf()];
        let err = parse_compose_multi(&paths, "missing").unwrap_err();
        assert!(err.contains("service 'missing' not found"));
    }

    #[test]
    fn multi_compose_skips_file_without_service() {
        let base = write_compose(
            r"
services:
  db:
    image: postgres:15
",
        );
        let over = write_compose(
            r"
services:
  app:
    image: node:22
",
        );
        let paths = vec![base.path().to_path_buf(), over.path().to_path_buf()];
        let cfg = parse_compose_multi(&paths, "app").unwrap();
        assert_eq!(cfg.image.as_deref(), Some("node:22"));
    }
}
