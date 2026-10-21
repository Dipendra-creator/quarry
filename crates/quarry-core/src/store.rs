//! Saved connections, query history and saved ("favorite") queries.
//! Plain JSON files in the config dir; passwords go to the OS keychain.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::model::{ConnectionConfig, ConnectionGroup};

const KEYRING_SERVICE: &str = "quarry";

/// `$QUARRY_CONFIG_DIR`, or `<os config dir>/Quarry`.
pub fn config_dir() -> PathBuf {
    std::env::var_os("QUARRY_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("Quarry"))
}

fn read_json<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> Result<T> {
    match std::fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s).with_context(|| format!("invalid {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e.into()),
    }
}

/// Writes through a temp file so a crash never leaves a truncated file.
fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(value)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ConnectionList {
    #[serde(default)]
    pub groups: Vec<ConnectionGroup>,
    #[serde(default)]
    pub connections: Vec<ConnectionConfig>,
}

pub struct ConnectionStore {
    path: PathBuf,
    pub list: ConnectionList,
}

impl ConnectionStore {
    pub fn load() -> Result<Self> {
        Self::load_from(config_dir().join("connections.json"))
    }

    pub fn load_from(path: PathBuf) -> Result<Self> {
        let list = read_json(&path)?;
        Ok(Self { path, list })
    }

    pub fn save(&self) -> Result<()> {
        write_json(&self.path, &self.list)
    }

    /// Finds a connection by id, or by name (case-insensitive).
    pub fn find(&self, id_or_name: &str) -> Option<&ConnectionConfig> {
        self.list.connections.iter().find(|c| c.id == id_or_name).or_else(|| {
            self.list
                .connections
                .iter()
                .find(|c| c.display_name().eq_ignore_ascii_case(id_or_name))
        })
    }

    /// The connection with its passwords filled in from the keychain.
    pub fn resolve(&self, id_or_name: &str) -> Option<ConnectionConfig> {
        let mut c = self.find(id_or_name)?.clone();
        if c.password.is_none() && c.save_password {
            c.password = load_password(&c.id);
        }
        let id = c.id.clone();
        if let Some(ssh) = &mut c.ssh {
            ssh.password = ssh.password.take().or_else(|| load_password(&format!("{id}:ssh")));
            ssh.key_passphrase = ssh.key_passphrase.take().or_else(|| load_password(&format!("{id}:sshkey")));
        }
        Some(c)
    }

    /// Inserts or replaces by id and saves. The password goes to the keychain.
    pub fn upsert(&mut self, config: ConnectionConfig) -> Result<()> {
        if config.save_password {
            if let Some(pw) = &config.password {
                store_password(&config.id, pw)?;
            }
        } else {
            delete_password(&config.id);
        }
        if let Some(ssh) = &config.ssh {
            if let Some(pw) = ssh.password.as_deref().filter(|p| !p.is_empty()) {
                store_password(&format!("{}:ssh", config.id), pw)?;
            }
            if let Some(pw) = ssh.key_passphrase.as_deref().filter(|p| !p.is_empty()) {
                store_password(&format!("{}:sshkey", config.id), pw)?;
            }
        }
        match self.list.connections.iter_mut().find(|c| c.id == config.id) {
            Some(existing) => *existing = config,
            None => self.list.connections.push(config),
        }
        self.save()
    }

    pub fn remove(&mut self, id: &str) -> Result<()> {
        self.list.connections.retain(|c| c.id != id);
        delete_password(id);
        delete_password(&format!("{id}:ssh"));
        delete_password(&format!("{id}:sshkey"));
        self.save()
    }

    pub fn add_group(&mut self, name: &str) -> Result<ConnectionGroup> {
        let g = ConnectionGroup { id: uuid::Uuid::new_v4().to_string(), name: name.into(), expanded: true };
        self.list.groups.push(g.clone());
        self.save()?;
        Ok(g)
    }

    pub fn remove_group(&mut self, id: &str) -> Result<()> {
        self.list.groups.retain(|g| g.id != id);
        for c in &mut self.list.connections {
            if c.group_id.as_deref() == Some(id) {
                c.group_id = None;
            }
        }
        self.save()
    }
}

fn entry(id: &str) -> keyring::Result<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, id)
}

pub fn store_password(id: &str, password: &str) -> Result<()> {
    entry(id)
        .and_then(|e| e.set_password(password))
        .context("could not save the password in the system keychain")
}

pub fn load_password(id: &str) -> Option<String> {
    entry(id).and_then(|e| e.get_password()).ok()
}

pub fn delete_password(id: &str) {
    if let Ok(e) = entry(id) {
        let _ = e.delete_credential();
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or_default()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub connection_id: String,
    pub database: String,
    pub sql: String,
    /// Unix seconds.
    pub at: u64,
}

/// Executed queries, newest last, one JSON object per line.
pub struct History {
    path: PathBuf,
}

impl History {
    pub fn open() -> Self {
        Self { path: config_dir().join("history.jsonl") }
    }

    pub fn append(&self, connection_id: &str, database: &str, sql: &str) -> Result<()> {
        use std::io::Write as _;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let entry = HistoryEntry { connection_id: connection_id.into(), database: database.into(), sql: sql.into(), at: now() };
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&self.path)?;
        writeln!(f, "{}", serde_json::to_string(&entry)?)?;
        Ok(())
    }

    /// Newest first.
    pub fn entries(&self, connection_id: &str, limit: usize) -> Vec<HistoryEntry> {
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return vec![];
        };
        text.lines()
            .rev()
            .filter_map(|l| serde_json::from_str::<HistoryEntry>(l).ok())
            .filter(|e| e.connection_id == connection_id)
            .take(limit)
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedQuery {
    pub id: String,
    pub connection_id: String,
    pub name: String,
    pub sql: String,
}

/// The "Queries" sidebar tab.
pub struct SavedQueries {
    path: PathBuf,
    pub queries: Vec<SavedQuery>,
}

impl SavedQueries {
    pub fn load() -> Result<Self> {
        let path = config_dir().join("queries.json");
        Ok(Self { queries: read_json(&path)?, path })
    }

    pub fn for_connection(&self, connection_id: &str) -> Vec<&SavedQuery> {
        self.queries.iter().filter(|q| q.connection_id == connection_id).collect()
    }

    pub fn upsert(&mut self, q: SavedQuery) -> Result<()> {
        match self.queries.iter_mut().find(|e| e.id == q.id) {
            Some(e) => *e = q,
            None => self.queries.push(q),
        }
        write_json(&self.path, &self.queries)
    }

    pub fn remove(&mut self, id: &str) -> Result<()> {
        self.queries.retain(|q| q.id != id);
        write_json(&self.path, &self.queries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DriverKind;

    #[test]
    fn password_never_hits_disk() {
        let path = std::env::temp_dir().join(format!("quarry-store-{}.json", uuid::Uuid::new_v4()));
        let mut store = ConnectionStore::load_from(path.clone()).unwrap();
        let mut c = ConnectionConfig::new(DriverKind::Postgres);
        c.name = "Local".into();
        c.password = Some("hunter2".into());
        c.save_password = false;
        store.upsert(c.clone()).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("hunter2"));
        let again = ConnectionStore::load_from(path.clone()).unwrap();
        assert_eq!(again.find("local").unwrap().id, c.id);
        assert_eq!(again.find("local").unwrap().password, None);
        let _ = std::fs::remove_file(path);
    }
}
