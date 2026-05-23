#![expect(
    clippy::disallowed_methods,
    reason = "temporary startup config migration uses synchronous file I/O before config is loaded"
)]

use std::fmt;
use std::path::{Path, PathBuf};

use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value};

use crate::{Error, Result, SettingsLayer};

pub(crate) const REMOVAL_NOTE: &str =
    "This temporary compatibility migration will be removed before v1.0.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LegacySandboxMigrationReport {
    pub(crate) contents:    String,
    pub(crate) backup_path: PathBuf,
    pub(crate) warning:     String,
}

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

pub(crate) fn migrate_settings_path(
    path: &Path,
    original_contents: &str,
) -> Result<Option<LegacySandboxMigrationReport>> {
    let Some(next_contents) = migrate_contents(original_contents, path)? else {
        return Ok(None);
    };

    next_contents
        .parse::<SettingsLayer>()
        .map_err(|err| Error::parse_file("Migrated settings file is invalid", path, err))?;

    let backup_path = next_backup_path(path);
    std::fs::write(&backup_path, original_contents).map_err(|source| {
        Error::other(format!(
            "writing legacy sandbox migration backup {}: {source}",
            backup_path.display()
        ))
    })?;
    std::fs::write(path, &next_contents).map_err(|source| {
        Error::other(format!(
            "writing migrated settings file {}: {source}",
            path.display()
        ))
    })?;

    let warning = format!(
        "Migrated legacy [run.sandbox] settings in {} to [run.environment] and [environments.default]. Backup written to {}. {REMOVAL_NOTE}",
        path.display(),
        backup_path.display()
    );

    Ok(Some(LegacySandboxMigrationReport {
        contents: next_contents,
        backup_path,
        warning,
    }))
}

fn migrate_contents(original_contents: &str, path: &Path) -> Result<Option<String>> {
    let Ok(mut doc) = original_contents.parse::<DocumentMut>() else {
        return Ok(None);
    };

    if !has_legacy_run_sandbox(&doc) {
        return Ok(None);
    }
    if has_new_environment_config(&doc) {
        return Err(Error::other(format!(
            "Legacy [run.sandbox] settings in {} could not be auto-migrated because the file already contains [run.environment] or [environments.default]. Remove one config style and retry.",
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
    let has_default_environment = doc
        .get("environments")
        .and_then(Item::as_table)
        .and_then(|envs| envs.get("default"))
        .is_some();
    has_run_environment || has_default_environment
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

    let provider = sandbox.get("provider").and_then(Item::as_str);
    let provider_key = provider.unwrap_or_default().to_ascii_lowercase();
    if provider.is_none() {
        unsupported.push("run.sandbox.provider".to_string());
    }

    match provider_key.as_str() {
        "daytona" => {
            migrate_skip_clone(doc, &sandbox, "daytona");
            reject_provider_table(&sandbox, "docker", &mut unsupported);
        }
        "docker" => {
            migrate_skip_clone(doc, &sandbox, "docker");
            reject_provider_table(&sandbox, "daytona", &mut unsupported);
        }
        _ => {
            reject_provider_table(&sandbox, "daytona", &mut unsupported);
            reject_provider_table(&sandbox, "docker", &mut unsupported);
        }
    }

    set_value(
        path_table(doc, &["run", "environment"]),
        "id",
        Value::from("default"),
    );
    if let Some(provider) = provider {
        set_value(
            path_table(doc, &["environments", "default"]),
            "provider",
            Value::from(provider),
        );
    }

    let env = path_table(doc, &["environments", "default"]);
    if let Some(preserve) = sandbox.get("preserve") {
        if preserve.as_bool().is_some() {
            set_item(
                path_table_in_table(env, &["lifecycle"]),
                "preserve",
                preserve.clone(),
            );
        } else {
            unsupported.push("run.sandbox.preserve".to_string());
        }
    }
    if let Some(env_item) = sandbox.get("env") {
        if is_table_like(env_item) {
            copy_table(env_item, path_table_in_table(env, &["env"]));
        } else {
            unsupported.push("run.sandbox.env".to_string());
        }
    }

    match provider_key.as_str() {
        "daytona" => migrate_daytona(&sandbox, env, &mut unsupported),
        "docker" => migrate_docker(&sandbox, env, &mut unsupported),
        _ => {}
    }

    if !unsupported.is_empty() {
        return Err(MigrationFailure {
            unsupported_keys: unsupported,
        });
    }

    remove_run_sandbox(doc);
    Ok(())
}

fn migrate_skip_clone(doc: &mut DocumentMut, sandbox: &Table, provider: &str) {
    let Some(provider_table) = sandbox.get(provider).and_then(Item::as_table) else {
        return;
    };
    if provider_table
        .get("skip_clone")
        .and_then(Item::as_bool)
        .unwrap_or(false)
    {
        set_value(
            path_table(doc, &["run", "clone"]),
            "enabled",
            Value::from(false),
        );
    }
}

fn reject_provider_table(sandbox: &Table, provider: &str, unsupported: &mut Vec<String>) {
    if let Some(item) = sandbox.get(provider) {
        item_path_keys(&format!("run.sandbox.{provider}"), item, unsupported);
    }
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
                if item.as_bool() != Some(true) {
                    unsupported.push("run.sandbox.daytona.skip_clone".to_string());
                }
            }
            "auto_stop_interval" => {
                if let Some(minutes) = item.as_integer().filter(|minutes| *minutes >= 0) {
                    set_value(
                        path_table_in_table(env, &["lifecycle"]),
                        "auto_stop",
                        Value::from(format!("{minutes}m")),
                    );
                } else {
                    unsupported.push("run.sandbox.daytona.auto_stop_interval".to_string());
                }
            }
            "labels" => {
                if is_table_like(item) {
                    copy_table(item, path_table_in_table(env, &["labels"]));
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
            "name" => set_item(path_table_in_table(env, &["image"]), "ref", item.clone()),
            "cpu" => set_item(
                path_table_in_table(env, &["resources"]),
                "cpu",
                item.clone(),
            ),
            "memory" => set_item(
                path_table_in_table(env, &["resources"]),
                "memory",
                item.clone(),
            ),
            "disk" => set_item(
                path_table_in_table(env, &["resources"]),
                "disk",
                item.clone(),
            ),
            "dockerfile" => set_item(
                path_table_in_table(env, &["image"]),
                "dockerfile",
                item.clone(),
            ),
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
                if item.as_bool() != Some(true) {
                    unsupported.push("run.sandbox.docker.skip_clone".to_string());
                }
            }
            "image" => set_item(path_table_in_table(env, &["image"]), "ref", item.clone()),
            "memory_limit" => set_item(
                path_table_in_table(env, &["resources"]),
                "memory",
                item.clone(),
            ),
            "cpu_quota" => {
                if let Some(cpu_quota) = item.as_integer() {
                    let cpu_count = cpu_quota / 100_000;
                    if cpu_quota > 0 && cpu_quota % 100_000 == 0 && i32::try_from(cpu_count).is_ok()
                    {
                        set_value(
                            path_table_in_table(env, &["resources"]),
                            "cpu",
                            Value::from(cpu_count),
                        );
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

fn path_table<'a>(doc: &'a mut DocumentMut, path: &[&str]) -> &'a mut Table {
    let mut item = doc.as_item_mut();
    for segment in path {
        item = &mut item[segment];
        if !item.is_table() {
            *item = Item::Table(Table::new());
        }
    }
    item.as_table_mut().expect("path item should be a table")
}

fn path_table_in_table<'a>(table: &'a mut Table, path: &[&str]) -> &'a mut Table {
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

fn set_value(table: &mut Table, key: &str, value: Value) {
    table[key] = Item::Value(value);
}

fn set_item(table: &mut Table, key: &str, item: Item) {
    table[key] = item;
}

fn is_table_like(item: &Item) -> bool {
    item.is_table() || item.as_value().and_then(Value::as_inline_table).is_some()
}

fn copy_table(source: &Item, target: &mut Table) {
    if let Some(table) = source.as_table() {
        for (key, item) in table {
            target[key] = item.clone();
        }
        return;
    }

    if let Some(inline_table) = source.as_value().and_then(Value::as_inline_table) {
        for (key, value) in inline_table {
            target[key] = Item::Value(value.clone());
        }
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
                    set_item(&mut migrated_volume, "id", item.clone());
                }
                "mount_path" => {
                    has_mount_path = true;
                    set_item(&mut migrated_volume, "mount_path", item.clone());
                }
                "subpath" => set_item(&mut migrated_volume, "subpath", item.clone()),
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

fn next_backup_path(path: &Path) -> PathBuf {
    let base = path.with_file_name(format!(
        "{}.legacy-sandbox-migration.bak",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("settings.toml")
    ));
    if !base.exists() {
        return base;
    }

    for index in 1.. {
        let candidate = path.with_file_name(format!(
            "{}.legacy-sandbox-migration.{index}.bak",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("settings.toml")
        ));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("unbounded backup suffix search should return")
}

#[cfg(test)]
mod tests {
    use fabro_types::settings::InterpString;
    use fabro_types::settings::run::EnvironmentProvider;

    use super::*;

    fn migrate(source: &str) -> String {
        migrate_contents(source, Path::new("settings.toml"))
            .expect("migration should not error")
            .expect("legacy sandbox should migrate")
    }

    #[test]
    fn provider_only_daytona_config_migrates_to_default_environment() {
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

        assert_eq!(resolved.environment.id, "default");
        assert_eq!(resolved.environment.provider, EnvironmentProvider::Daytona);
        assert!(migrated.contains("[run.environment]"));
        assert!(migrated.contains("[environments.default]"));
        assert!(!migrated.contains("[run.sandbox]"));
    }

    #[test]
    fn non_legacy_config_is_not_migrated() {
        let migrated = migrate_contents("_version = 1\n", Path::new("settings.toml"))
            .expect("non-legacy TOML should not error");

        assert!(migrated.is_none());
    }

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

        assert_eq!(resolved.image.reference.as_deref(), Some("fabro-v11"));
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
            resolved.image.reference.as_deref(),
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
    fn migrate_settings_path_writes_backup_and_rewrites_original() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("settings.toml");
        let original = r#"
_version = 1

[run.sandbox]
provider = "daytona"
"#;
        std::fs::write(&path, original).expect("write fixture");

        let report = migrate_settings_path(&path, original)
            .expect("migration should succeed")
            .expect("legacy config should migrate");

        let rewritten = std::fs::read_to_string(&path).expect("read rewritten settings");
        let backup = std::fs::read_to_string(&report.backup_path).expect("read backup");

        assert_eq!(backup, original);
        assert!(rewritten.contains("[run.environment]"));
        assert!(rewritten.contains("[environments.default]"));
        assert!(report.warning.contains("temporary compatibility migration"));
    }

    #[test]
    fn existing_backup_uses_numbered_suffix() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("settings.toml");
        std::fs::write(
            path.with_file_name("settings.toml.legacy-sandbox-migration.bak"),
            "old",
        )
        .expect("write existing backup");

        let next = next_backup_path(&path);

        assert!(next.ends_with("settings.toml.legacy-sandbox-migration.1.bak"));
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
}
