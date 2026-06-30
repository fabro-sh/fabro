use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use fabro_db::DbPool;
use fabro_types::{Variable, is_env_style_name};
use sqlx::Row as _;
use sqlx::sqlite::SqliteRow;
use tokio::fs;
use tracing::info;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid variable name: {0}")]
    InvalidName(String),

    #[error("variable not found: {0}")]
    NotFound(String),

    #[error("database error")]
    Db(#[from] sqlx::Error),

    #[error("reading legacy variables file {path}")]
    LegacyRead {
        path:   PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("parsing legacy variables file {path}")]
    LegacyParse {
        path:   PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("legacy variables file {path} contains invalid variable name: {name}")]
    LegacyInvalidName { path: PathBuf, name: String },

    #[error("renaming legacy variables file {source_path} to backup {backup_path}")]
    LegacyBackup {
        source_path: PathBuf,
        backup_path: PathBuf,
        #[source]
        source:      std::io::Error,
    },

    #[error("parsing variable timestamp for {name}.{column}")]
    Timestamp {
        name:   String,
        column: &'static str,
        #[source]
        source: chrono::ParseError,
    },

    #[error("variable row count {count} exceeds SQLite integer range")]
    RowCountOverflow { count: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportReport {
    pub source_path:    PathBuf,
    pub backup_path:    PathBuf,
    pub imported_rows:  i64,
    pub skipped_rows:   i64,
    pub variable_names: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct LegacyVariableEntry {
    value:       String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    created_at:  DateTime<Utc>,
    updated_at:  DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct VariableStore {
    pool: DbPool,
}

impl VariableStore {
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }

    pub async fn set(
        &self,
        name: &str,
        value: &str,
        description: Option<&str>,
    ) -> Result<Variable, Error> {
        self.set_with_policy(name, value, description, false).await
    }

    pub async fn update_existing(
        &self,
        name: &str,
        value: &str,
        description: Option<&str>,
    ) -> Result<Variable, Error> {
        self.set_with_policy(name, value, description, true).await
    }

    async fn set_with_policy(
        &self,
        name: &str,
        value: &str,
        description: Option<&str>,
        require_existing: bool,
    ) -> Result<Variable, Error> {
        Self::validate_name(name)?;

        let existing = sqlx::query("SELECT description, created_at FROM variables WHERE name = ?")
            .bind(name)
            .fetch_optional(&self.pool)
            .await?;

        if require_existing && existing.is_none() {
            return Err(Error::NotFound(name.to_string()));
        }

        let now = Utc::now();
        let (created_at, description) = match existing {
            Some(row) => {
                let created_at_text = row.get::<String, _>("created_at");
                let created_at = parse_timestamp(name, "created_at", &created_at_text)?;
                let existing_description: Option<String> = row.get("description");
                (
                    created_at,
                    description.map(str::to_string).or(existing_description),
                )
            }
            None => (now, description.map(str::to_string)),
        };
        let created_at_text = created_at.to_rfc3339();
        let updated_at_text = now.to_rfc3339();

        sqlx::query(
            r"
            INSERT INTO variables (name, value, description, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?)
            ON CONFLICT(name) DO UPDATE SET
                value = excluded.value,
                description = excluded.description,
                updated_at = excluded.updated_at
            ",
        )
        .bind(name)
        .bind(value)
        .bind(description.as_deref())
        .bind(created_at_text)
        .bind(updated_at_text)
        .execute(&self.pool)
        .await?;

        Ok(Variable {
            name: name.to_string(),
            value: value.to_string(),
            description,
            created_at,
            updated_at: now,
        })
    }

    pub async fn get(&self, name: &str) -> Result<Option<Variable>, Error> {
        let row = sqlx::query(
            "SELECT name, value, description, created_at, updated_at FROM variables WHERE name = ?",
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;

        row.as_ref().map(variable_from_row).transpose()
    }

    pub async fn list(&self) -> Result<Vec<Variable>, Error> {
        let rows = sqlx::query(
            "SELECT name, value, description, created_at, updated_at FROM variables ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?;

        rows.iter().map(variable_from_row).collect()
    }

    /// Snapshot every variable as a `name -> value` map, dropping descriptions
    /// and timestamps. Used to seed the template render context (`{{ vars.*
    /// }}`) at run creation.
    pub async fn value_map(&self) -> Result<HashMap<String, String>, Error> {
        let rows = sqlx::query("SELECT name, value FROM variables")
            .fetch_all(&self.pool)
            .await?;

        Ok(rows
            .into_iter()
            .map(|row| (row.get("name"), row.get("value")))
            .collect())
    }

    pub async fn remove(&self, name: &str) -> Result<(), Error> {
        Self::validate_name(name)?;
        let result = sqlx::query("DELETE FROM variables WHERE name = ?")
            .bind(name)
            .execute(&self.pool)
            .await?;

        if result.rows_affected() == 0 {
            return Err(Error::NotFound(name.to_string()));
        }

        Ok(())
    }

    pub fn validate_name(name: &str) -> Result<(), Error> {
        if is_env_style_name(name) {
            Ok(())
        } else {
            Err(Error::InvalidName(name.to_string()))
        }
    }
}

pub async fn import_legacy_json_once(
    pool: &DbPool,
    source_path: impl AsRef<Path>,
) -> Result<Option<ImportReport>, Error> {
    let source_path = source_path.as_ref();
    let contents = match fs::read_to_string(source_path).await {
        Ok(contents) => contents,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(Error::LegacyRead {
                path: source_path.to_path_buf(),
                source,
            });
        }
    };

    let entries = parse_legacy_entries(source_path, &contents)?;
    let mut names = entries.keys().cloned().collect::<Vec<_>>();
    names.sort();

    for name in &names {
        if !is_env_style_name(name) {
            return Err(Error::LegacyInvalidName {
                path: source_path.to_path_buf(),
                name: name.clone(),
            });
        }
    }

    let mut transaction = pool.begin().await?;
    let mut imported_names = Vec::new();
    let mut skipped_rows = 0usize;
    for name in &names {
        let entry = &entries[name];
        let result = sqlx::query(
            r"
            INSERT INTO variables (name, value, description, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?)
            ON CONFLICT(name) DO NOTHING
            ",
        )
        .bind(name)
        .bind(&entry.value)
        .bind(entry.description.as_deref())
        .bind(entry.created_at.to_rfc3339())
        .bind(entry.updated_at.to_rfc3339())
        .execute(&mut *transaction)
        .await?;

        if result.rows_affected() == 0 {
            skipped_rows += 1;
        } else {
            imported_names.push(name.clone());
        }
    }

    transaction.commit().await?;
    let backup_path = rename_imported_legacy_file(source_path).await?;

    let report = ImportReport {
        source_path: source_path.to_path_buf(),
        backup_path,
        imported_rows: row_count(imported_names.len())?,
        skipped_rows: row_count(skipped_rows)?,
        variable_names: imported_names,
    };

    info!(
        source_path = %source_path.display(),
        backup_path = %report.backup_path.display(),
        imported_rows = report.imported_rows,
        skipped_rows = report.skipped_rows,
        variable_names = ?report.variable_names,
        "imported legacy variables json into sqlite"
    );

    Ok(Some(report))
}

fn parse_legacy_entries(
    path: &Path,
    contents: &str,
) -> Result<HashMap<String, LegacyVariableEntry>, Error> {
    serde_json::from_str(contents).map_err(|source| Error::LegacyParse {
        path: path.to_path_buf(),
        source,
    })
}

async fn rename_imported_legacy_file(source_path: &Path) -> Result<PathBuf, Error> {
    let backup_path = legacy_backup_path(source_path, Utc::now());
    fs::rename(source_path, &backup_path)
        .await
        .map_err(|source| Error::LegacyBackup {
            source_path: source_path.to_path_buf(),
            backup_path: backup_path.clone(),
            source,
        })?;
    Ok(backup_path)
}

fn legacy_backup_path(source_path: &Path, imported_at: DateTime<Utc>) -> PathBuf {
    let timestamp = imported_at.format("%Y%m%dT%H%M%S%fZ");
    let mut file_name = source_path
        .file_name()
        .map_or_else(|| OsString::from("variables.json"), OsString::from);
    file_name.push(format!(".imported-{timestamp}.bak"));
    source_path.with_file_name(file_name)
}

fn row_count(count: usize) -> Result<i64, Error> {
    i64::try_from(count).map_err(|_| Error::RowCountOverflow { count })
}

fn variable_from_row(row: &SqliteRow) -> Result<Variable, Error> {
    let name = row.get::<String, _>("name");
    let created_at_text = row.get::<String, _>("created_at");
    let updated_at_text = row.get::<String, _>("updated_at");
    let created_at = parse_timestamp(&name, "created_at", &created_at_text)?;
    let updated_at = parse_timestamp(&name, "updated_at", &updated_at_text)?;

    Ok(Variable {
        name,
        value: row.get("value"),
        description: row.get("description"),
        created_at,
        updated_at,
    })
}

fn parse_timestamp(name: &str, column: &'static str, value: &str) -> Result<DateTime<Utc>, Error> {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|source| Error::Timestamp {
            name: name.to_string(),
            column,
            source,
        })
}
