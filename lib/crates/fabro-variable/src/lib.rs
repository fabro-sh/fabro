#![expect(
    clippy::disallowed_methods,
    reason = "fabro-variable: sync JSON-file storage; not used on a Tokio hot path"
)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::{fmt, io};

use chrono::{DateTime, Utc};
use fabro_types::{Variable, is_env_style_name};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct VariableEntry {
    value:       String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    created_at:  DateTime<Utc>,
    updated_at:  DateTime<Utc>,
}

#[derive(Debug)]
pub enum Error {
    InvalidName(String),
    NotFound(String),
    Io(std::io::Error),
    Serde(serde_json::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName(name) => write!(f, "invalid variable name: {name}"),
            Self::NotFound(name) => write!(f, "variable not found: {name}"),
            Self::Io(err) => write!(f, "{err}"),
            Self::Serde(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Serde(err) => Some(err),
            Self::InvalidName(_) | Self::NotFound(_) => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for Error {
    fn from(value: serde_json::Error) -> Self {
        Self::Serde(value)
    }
}

#[derive(Debug)]
pub struct VariableStore {
    path:    PathBuf,
    entries: HashMap<String, VariableEntry>,
}

impl VariableStore {
    pub fn load(path: PathBuf) -> Result<Self, Error> {
        let entries = match std::fs::read_to_string(&path) {
            Ok(contents) => serde_json::from_str(&contents)?,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(err) => return Err(io_context("read variables", &path, &err).into()),
        };

        Ok(Self { path, entries })
    }

    pub fn set(
        &mut self,
        name: &str,
        value: &str,
        description: Option<&str>,
    ) -> Result<Variable, Error> {
        self.set_with_policy(name, value, description, false)
    }

    pub fn update_existing(
        &mut self,
        name: &str,
        value: &str,
        description: Option<&str>,
    ) -> Result<Variable, Error> {
        self.set_with_policy(name, value, description, true)
    }

    fn set_with_policy(
        &mut self,
        name: &str,
        value: &str,
        description: Option<&str>,
        require_existing: bool,
    ) -> Result<Variable, Error> {
        Self::validate_name(name)?;

        let now = Utc::now();
        let existing = self.entries.get(name);
        if require_existing && existing.is_none() {
            return Err(Error::NotFound(name.to_string()));
        }
        let (created_at, description) = existing.map_or_else(
            || (now, description.map(str::to_string)),
            |entry| {
                (
                    entry.created_at,
                    description
                        .map(str::to_string)
                        .or_else(|| entry.description.clone()),
                )
            },
        );
        let entry = VariableEntry {
            value: value.to_string(),
            description: description.clone(),
            created_at,
            updated_at: now,
        };
        self.entries.insert(name.to_string(), entry);
        self.write_atomic()?;

        Ok(Variable {
            name: name.to_string(),
            value: value.to_string(),
            description,
            created_at,
            updated_at: now,
        })
    }

    pub fn get(&self, name: &str) -> Option<Variable> {
        self.entries
            .get(name)
            .map(|entry| variable_from_entry(name, entry))
    }

    pub fn get_value(&self, name: &str) -> Option<&str> {
        self.entries.get(name).map(|entry| entry.value.as_str())
    }

    pub fn list(&self) -> Vec<Variable> {
        let mut data = self
            .entries
            .iter()
            .map(|(name, entry)| variable_from_entry(name, entry))
            .collect::<Vec<_>>();
        data.sort_by(|a, b| a.name.cmp(&b.name));
        data
    }

    pub fn remove(&mut self, name: &str) -> Result<(), Error> {
        Self::validate_name(name)?;
        if self.entries.remove(name).is_none() {
            return Err(Error::NotFound(name.to_string()));
        }
        self.write_atomic()?;
        Ok(())
    }

    pub fn validate_name(name: &str) -> Result<(), Error> {
        if is_env_style_name(name) {
            Ok(())
        } else {
            Err(Error::InvalidName(name.to_string()))
        }
    }

    fn write_atomic(&self) -> Result<(), Error> {
        let parent = self
            .path
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        std::fs::create_dir_all(&parent)
            .map_err(|err| io_context("create variables directory", &parent, &err))?;

        let file_name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("variables.json");
        let tmp_path = parent.join(format!(".{file_name}.tmp-{}", ulid::Ulid::new()));
        let json = serde_json::to_vec_pretty(&self.entries)?;
        std::fs::write(&tmp_path, json)
            .map_err(|err| io_context("write variables temp file", &tmp_path, &err))?;
        std::fs::rename(&tmp_path, &self.path).map_err(|err| {
            io_context(
                &format!("rename variables temp file to {}", self.path.display()),
                &tmp_path,
                &err,
            )
        })?;
        Ok(())
    }
}

fn variable_from_entry(name: &str, entry: &VariableEntry) -> Variable {
    Variable {
        name:        name.to_string(),
        value:       entry.value.clone(),
        description: entry.description.clone(),
        created_at:  entry.created_at,
        updated_at:  entry.updated_at,
    }
}

fn io_context(op: &str, path: &Path, source: &io::Error) -> io::Error {
    io::Error::new(source.kind(), format!("{op} {}: {source}", path.display()))
}
