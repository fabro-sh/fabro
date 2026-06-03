use std::fmt;
use std::path::Path;
use std::str::FromStr;

use fabro_types::settings::run::EnvironmentProvider;
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value};

use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
struct MigrationFailure {
    unsupported_keys: Vec<String>,
}

impl fmt::Display for MigrationFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Unsupported keys:")?;
        for key in &self.unsupported_keys {
            writeln!(f, "  - {key}")?;
        }
        writeln!(f)?;
        write!(
            f,
            "Rename legacy sandbox configuration to [run.environment] and [environments.<slug>]. See docs/public/execution/environments.mdx."
        )
    }
}

pub(crate) fn migrate_contents(original_contents: &str, path: &Path) -> Result<Option<String>> {
    let Ok(mut doc) = original_contents.parse::<DocumentMut>() else {
        return Ok(None);
    };

    if !has_legacy_run_sandbox(&doc) {
        return Ok(None);
    }
    if has_new_environment_config(&doc) {
        return Err(Error::other(format!(
            "Legacy [run.sandbox] settings in {} could not be auto-migrated because the file already contains [run.environment] or [environments]. Remove one config style and retry.",
            path.display()
        )));
    }

    migrate_document(&mut doc).map_err(|failure| {
        Error::other(format!(
            "Legacy [run.sandbox] settings in {} could not be auto-migrated.\n\n{}",
            path.display(),
            failure
        ))
    })?;

    Ok(Some(doc.to_string()))
}

fn has_legacy_run_sandbox(doc: &DocumentMut) -> bool {
    doc.get("run")
        .and_then(Item::as_table)
        .and_then(|run| run.get("sandbox"))
        .is_some()
}

fn has_new_environment_config(doc: &DocumentMut) -> bool {
    let has_run_environment = doc
        .get("run")
        .and_then(Item::as_table)
        .and_then(|run| run.get("environment"))
        .is_some();
    let has_environment_catalog = doc.get("environments").and_then(Item::as_table).is_some();
    has_run_environment || has_environment_catalog
}

fn migrate_document(doc: &mut DocumentMut) -> std::result::Result<(), MigrationFailure> {
    let Some(sandbox_item) = doc
        .get("run")
        .and_then(Item::as_table)
        .and_then(|run| run.get("sandbox"))
    else {
        return Ok(());
    };
    let Some(sandbox) = sandbox_item.as_table().cloned() else {
        return Err(MigrationFailure {
            unsupported_keys: vec!["run.sandbox".to_string()],
        });
    };

    let mut unsupported = Vec::new();
    for (key, item) in &sandbox {
        if !matches!(key, "provider" | "preserve" | "env" | "daytona" | "docker") {
            item_path_keys(&format!("run.sandbox.{key}"), item, &mut unsupported);
        }
    }

    let provider_str = sandbox.get("provider").and_then(Item::as_str);
    let active_provider = provider_str.and_then(|provider| {
        if let Ok(provider) = EnvironmentProvider::from_str(provider) {
            Some(provider)
        } else {
            unsupported.push("run.sandbox.provider".to_string());
            None
        }
    });
    if provider_str.is_none() {
        unsupported.push("run.sandbox.provider".to_string());
    }

    // Inspect each provider's table once: if it's the active provider, capture
    // skip_clone; otherwise, report it as unsupported.
    let mut disable_clone = false;
    for provider in [EnvironmentProvider::Daytona, EnvironmentProvider::Docker] {
        let key: &'static str = provider.into();
        let Some(item) = sandbox.get(key) else {
            continue;
        };
        if Some(provider) == active_provider {
            if item
                .as_table()
                .and_then(|table| table.get("skip_clone"))
                .and_then(Item::as_bool)
                .unwrap_or(false)
            {
                disable_clone = true;
            }
        } else {
            item_path_keys(&format!("run.sandbox.{key}"), item, &mut unsupported);
        }
    }

    if disable_clone {
        ensure_table(doc.as_table_mut(), &["run", "clone"])["enabled"] =
            Item::Value(Value::from(false));
    }
    let environment_id = match active_provider {
        Some(EnvironmentProvider::Daytona) => "daytona",
        _ => "default",
    };
    ensure_table(doc.as_table_mut(), &["run", "environment"])["id"] =
        Item::Value(Value::from(environment_id));

    let env = ensure_table(doc.as_table_mut(), &["environments", environment_id]);
    if let Some(p) = provider_str {
        env["provider"] = Item::Value(Value::from(p));
    }
    if let Some(preserve) = sandbox.get("preserve") {
        if preserve.as_bool().is_some() {
            ensure_table(env, &["lifecycle"])["preserve"] = preserve.clone();
        } else {
            unsupported.push("run.sandbox.preserve".to_string());
        }
    }
    if let Some(env_item) = sandbox.get("env") {
        if env_item.is_table_like() {
            copy_table(env_item, ensure_table(env, &["env"]));
        } else {
            unsupported.push("run.sandbox.env".to_string());
        }
    }

    match active_provider {
        Some(EnvironmentProvider::Daytona) => migrate_daytona(&sandbox, env, &mut unsupported),
        Some(EnvironmentProvider::Docker) => migrate_docker(&sandbox, env, &mut unsupported),
        _ => {}
    }

    if !unsupported.is_empty() {
        unsupported.sort();
        unsupported.dedup();
        return Err(MigrationFailure {
            unsupported_keys: unsupported,
        });
    }

    remove_run_sandbox(doc);
    Ok(())
}

fn migrate_daytona(sandbox: &Table, env: &mut Table, unsupported: &mut Vec<String>) {
    let Some(daytona_item) = sandbox.get("daytona") else {
        return;
    };
    let Some(daytona) = daytona_item.as_table() else {
        unsupported.push("run.sandbox.daytona".to_string());
        return;
    };

    for (key, item) in daytona {
        match key {
            "skip_clone" => {
                if item.as_bool().is_none() {
                    unsupported.push("run.sandbox.daytona.skip_clone".to_string());
                }
            }
            "auto_stop_interval" => {
                if let Some(minutes) = item.as_integer().filter(|minutes| *minutes >= 0) {
                    ensure_table(env, &["lifecycle"])["auto_stop"] =
                        Item::Value(Value::from(format!("{minutes}m")));
                } else {
                    unsupported.push("run.sandbox.daytona.auto_stop_interval".to_string());
                }
            }
            "labels" => {
                if item.is_table_like() {
                    copy_table(item, ensure_table(env, &["labels"]));
                } else {
                    unsupported.push("run.sandbox.daytona.labels".to_string());
                }
            }
            "snapshot" => migrate_daytona_snapshot(item, env, unsupported),
            "volumes" => copy_array_of_tables_with_volume_id(item, env, unsupported),
            _ => item_path_keys(&format!("run.sandbox.daytona.{key}"), item, unsupported),
        }
    }
}

fn migrate_daytona_snapshot(snapshot_item: &Item, env: &mut Table, unsupported: &mut Vec<String>) {
    let Some(snapshot) = snapshot_item.as_table() else {
        unsupported.push("run.sandbox.daytona.snapshot".to_string());
        return;
    };

    for (key, item) in snapshot {
        match key {
            "name" => {}
            "cpu" => ensure_table(env, &["resources"])["cpu"] = item.clone(),
            "memory" => ensure_table(env, &["resources"])["memory"] = item.clone(),
            "disk" => ensure_table(env, &["resources"])["disk"] = item.clone(),
            "dockerfile" => ensure_table(env, &["image"])["dockerfile"] = item.clone(),
            _ => item_path_keys(
                &format!("run.sandbox.daytona.snapshot.{key}"),
                item,
                unsupported,
            ),
        }
    }
}

fn migrate_docker(sandbox: &Table, env: &mut Table, unsupported: &mut Vec<String>) {
    let Some(docker_item) = sandbox.get("docker") else {
        return;
    };
    let Some(docker) = docker_item.as_table() else {
        unsupported.push("run.sandbox.docker".to_string());
        return;
    };

    for (key, item) in docker {
        match key {
            "skip_clone" => {
                if item.as_bool().is_none() {
                    unsupported.push("run.sandbox.docker.skip_clone".to_string());
                }
            }
            "image" => ensure_table(env, &["image"])["docker"] = item.clone(),
            "memory_limit" => ensure_table(env, &["resources"])["memory"] = item.clone(),
            "cpu_quota" => {
                if let Some(cpu_quota) = item.as_integer() {
                    let cpu_count = cpu_quota / 100_000;
                    if cpu_quota > 0 && cpu_quota % 100_000 == 0 && i32::try_from(cpu_count).is_ok()
                    {
                        ensure_table(env, &["resources"])["cpu"] =
                            Item::Value(Value::from(cpu_count));
                    } else {
                        unsupported.push("run.sandbox.docker.cpu_quota".to_string());
                    }
                } else {
                    unsupported.push("run.sandbox.docker.cpu_quota".to_string());
                }
            }
            _ => item_path_keys(&format!("run.sandbox.docker.{key}"), item, unsupported),
        }
    }
}

fn ensure_table<'a>(table: &'a mut Table, path: &[&str]) -> &'a mut Table {
    let mut table = table;
    for segment in path {
        let item = &mut table[segment];
        if !item.is_table() {
            *item = Item::Table(Table::new());
        }
        table = item.as_table_mut().expect("path item should be a table");
    }
    table
}

fn copy_table(source: &Item, target: &mut Table) {
    let Some(src) = source.as_table_like() else {
        return;
    };
    for (key, item) in src.iter() {
        target[key] = item.clone();
    }
}

fn copy_array_of_tables_with_volume_id(
    source: &Item,
    target: &mut Table,
    unsupported: &mut Vec<String>,
) {
    let Some(volumes) = source.as_array_of_tables() else {
        unsupported.push("run.sandbox.daytona.volumes".to_string());
        return;
    };

    let mut migrated = ArrayOfTables::new();
    for volume in volumes {
        let mut migrated_volume = Table::new();
        let mut has_id = false;
        let mut has_mount_path = false;
        for (key, item) in volume {
            match key {
                "volume_id" => {
                    has_id = true;
                    migrated_volume["id"] = item.clone();
                }
                "mount_path" => {
                    has_mount_path = true;
                    migrated_volume["mount_path"] = item.clone();
                }
                "subpath" => migrated_volume["subpath"] = item.clone(),
                _ => item_path_keys(
                    &format!("run.sandbox.daytona.volumes.{key}"),
                    item,
                    unsupported,
                ),
            }
        }
        if !has_id {
            unsupported.push("run.sandbox.daytona.volumes.volume_id".to_string());
        }
        if !has_mount_path {
            unsupported.push("run.sandbox.daytona.volumes.mount_path".to_string());
        }
        migrated.push(migrated_volume);
    }
    target["volumes"] = Item::ArrayOfTables(migrated);
}

fn item_path_keys(prefix: &str, item: &Item, out: &mut Vec<String>) {
    if let Some(table) = item.as_table() {
        if table.is_empty() {
            out.push(prefix.to_string());
        }
        for (key, child) in table {
            item_path_keys(&format!("{prefix}.{key}"), child, out);
        }
        return;
    }

    if let Some(array) = item.as_array_of_tables() {
        if array.is_empty() {
            out.push(prefix.to_string());
            return;
        }
        for table in array {
            if table.is_empty() {
                out.push(prefix.to_string());
            }
            for (key, child) in table {
                item_path_keys(&format!("{prefix}.{key}"), child, out);
            }
        }
        return;
    }

    out.push(prefix.to_string());
}

fn remove_run_sandbox(doc: &mut DocumentMut) {
    if let Some(run) = doc.get_mut("run").and_then(Item::as_table_mut) {
        run.remove("sandbox");
    }
}

#[cfg(test)]
mod tests {
    use fabro_types::settings::InterpString;

    use super::*;
    use crate::SettingsLayer;

    fn migrate(source: &str) -> String {
        migrate_contents(source, Path::new("settings.toml"))
            .expect("migration should not error")
            .expect("legacy sandbox should migrate")
    }

    #[test]
    fn provider_only_daytona_config_migrates_to_daytona_environment() {
        let migrated = migrate(
            r#"
_version = 1

[run.sandbox]
provider = "daytona"
"#,
        );

        let settings = migrated
            .parse::<SettingsLayer>()
            .expect("migrated TOML should parse");
        let resolved = crate::WorkflowSettingsBuilder::from_layer(&settings)
            .expect("migrated settings should resolve")
            .run;

        assert_eq!(resolved.environment.id, "daytona");
        assert_eq!(resolved.environment.provider, EnvironmentProvider::Daytona);
        assert!(migrated.contains("[run.environment]"));
        assert!(migrated.contains("[environments.daytona]"));
        assert!(!migrated.contains("[run.sandbox]"));
    }

    #[test]
    fn non_legacy_config_is_not_migrated() {
        let migrated = migrate_contents("_version = 1\n", Path::new("settings.toml"))
            .expect("non-legacy TOML should not error");

        assert!(migrated.is_none());
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "test asserts the raw template source"
    )]
    #[test]
    fn daytona_snapshot_labels_lifecycle_and_volumes_migrate() {
        let migrated = migrate(
            r#"
_version = 1

[run.sandbox]
provider = "daytona"
preserve = true

[run.sandbox.env]
NODE_ENV = "development"

[run.sandbox.daytona]
auto_stop_interval = 30

[run.sandbox.daytona.labels]
repo = "fabro-sh/fabro"

[run.sandbox.daytona.snapshot]
name = "fabro-v11"
cpu = 8
memory = "16GB"
disk = "20GB"
dockerfile = { path = "Dockerfile" }

[[run.sandbox.daytona.volumes]]
volume_id = "vol_auth"
mount_path = "/home/daytona/.config"
subpath = "agents"
"#,
        );

        let settings = migrated
            .parse::<SettingsLayer>()
            .expect("migrated TOML should parse");
        let resolved = crate::WorkflowSettingsBuilder::from_layer(&settings)
            .expect("migrated settings should resolve")
            .run
            .environment;

        assert_eq!(resolved.id, "daytona");
        assert_eq!(resolved.image.docker.as_deref(), None);
        assert!(resolved.image.dockerfile.is_some());
        assert_eq!(resolved.resources.cpu, Some(8));
        assert_eq!(
            resolved.resources.memory.map(|size| size.as_bytes()),
            Some(16_000_000_000)
        );
        assert_eq!(
            resolved.resources.disk.map(|size| size.as_bytes()),
            Some(20_000_000_000)
        );
        assert!(resolved.lifecycle.preserve);
        assert_eq!(
            resolved
                .lifecycle
                .auto_stop
                .map(|duration| duration.as_std().as_secs()),
            Some(1800)
        );
        assert_eq!(
            resolved.labels.get("repo").map(String::as_str),
            Some("fabro-sh/fabro")
        );
        assert_eq!(
            resolved.env.get("NODE_ENV").map(InterpString::as_source),
            Some("development".to_string())
        );
        assert_eq!(resolved.volumes.len(), 1);
        assert_eq!(resolved.volumes[0].id, "vol_auth");
        assert_eq!(resolved.volumes[0].mount_path, "/home/daytona/.config");
        assert_eq!(resolved.volumes[0].subpath.as_deref(), Some("agents"));
    }

    #[test]
    fn docker_image_memory_and_cpu_quota_migrate() {
        let migrated = migrate(
            r#"
_version = 1

[run.sandbox]
provider = "docker"

[run.sandbox.docker]
image = "buildpack-deps:noble"
memory_limit = "4GB"
cpu_quota = 200000
"#,
        );

        let settings = migrated
            .parse::<SettingsLayer>()
            .expect("migrated TOML should parse");
        let resolved = crate::WorkflowSettingsBuilder::from_layer(&settings)
            .expect("migrated settings should resolve")
            .run
            .environment;

        assert_eq!(resolved.provider, EnvironmentProvider::Docker);
        assert_eq!(
            resolved.image.docker.as_deref(),
            Some("buildpack-deps:noble")
        );
        assert_eq!(resolved.resources.cpu, Some(2));
        assert_eq!(
            resolved.resources.memory.map(|size| size.as_bytes()),
            Some(4_000_000_000)
        );
    }

    #[test]
    fn provider_skip_clone_true_migrates_to_run_clone_disabled() {
        let migrated = migrate(
            r#"
_version = 1

[run.sandbox]
provider = "docker"

[run.sandbox.docker]
skip_clone = true
"#,
        );

        let settings = migrated
            .parse::<SettingsLayer>()
            .expect("migrated TOML should parse");
        let resolved = crate::WorkflowSettingsBuilder::from_layer(&settings)
            .expect("migrated settings should resolve")
            .run;

        assert!(!resolved.clone.enabled);
    }

    #[test]
    fn existing_new_environment_config_is_ambiguous() {
        let err = migrate_contents(
            r#"
_version = 1

[run.environment]
id = "default"

[run.sandbox]
provider = "daytona"
"#,
            Path::new("settings.toml"),
        )
        .expect_err("mixed old and new config should fail");

        assert!(
            err.to_string()
                .contains("already contains [run.environment]")
        );
    }

    #[test]
    fn unsupported_keys_are_reported_with_full_paths() {
        let err = migrate_contents(
            r#"
_version = 1

[run.sandbox]
provider = "daytona"

[run.sandbox.daytona]
unknown = true
"#,
            Path::new("settings.toml"),
        )
        .expect_err("unsupported keys should fail migration");

        let rendered = err.to_string();
        assert!(rendered.contains("run.sandbox.daytona.unknown"));
        assert!(rendered.contains("docs/public/execution/environments.mdx"));
    }

    #[test]
    fn unsupported_docker_cpu_quota_is_reported() {
        let err = migrate_contents(
            r#"
_version = 1

[run.sandbox]
provider = "docker"

[run.sandbox.docker]
cpu_quota = 250000
"#,
            Path::new("settings.toml"),
        )
        .expect_err("non-divisible cpu quota should fail migration");

        assert!(err.to_string().contains("run.sandbox.docker.cpu_quota"));
    }

    #[test]
    fn unsupported_provider_value_is_reported_before_rewriting() {
        let err = migrate_contents(
            r#"
_version = 1

[run.sandbox]
provider = "unknown"
"#,
            Path::new("settings.toml"),
        )
        .expect_err("unknown provider should fail migration");

        assert!(err.to_string().contains("run.sandbox.provider"));
    }

    #[test]
    fn explicit_skip_clone_false_is_accepted_as_default() {
        let migrated = migrate(
            r#"
_version = 1

[run.sandbox]
provider = "docker"

[run.sandbox.docker]
skip_clone = false
"#,
        );

        let settings = migrated
            .parse::<SettingsLayer>()
            .expect("migrated TOML should parse");
        let resolved = crate::WorkflowSettingsBuilder::from_layer(&settings)
            .expect("migrated settings should resolve")
            .run;

        assert!(resolved.clone.enabled);
    }
}
