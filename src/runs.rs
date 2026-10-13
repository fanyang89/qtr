//! Durable run intents. File pins keep inode identities alive across crashes.
use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use tokio::sync::Semaphore;
use uuid::Uuid;

struct StateLock(File);

impl Drop for StateLock {
    fn drop(&mut self) {
        // Explicit unlock also releases locks briefly inherited by forked children.
        let _ = self.0.unlock();
    }
}

#[derive(Clone)]
pub struct RunStore {
    _lock: Arc<StateLock>,
    path: Arc<PathBuf>,
    pub gate: Arc<Semaphore>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub id: String,
    pub state: String,
    pub resources: Vec<RunResource>,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunResource {
    pub kind: String,
    pub id: String,
    pub state: String,
    #[serde(skip)]
    pub token: String,
    #[serde(skip)]
    pub request: String,
}

impl RunStore {
    pub fn open(state: &Path) -> Result<Self> {
        let directory = state.join("runs");
        private_directory(&directory)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(directory.join("server.lock"))?;
        lock.try_lock()
            .context("run state is already open by another qtr server")?;
        let store = Self {
            _lock: Arc::new(StateLock(lock)),
            path: Arc::new(directory.join("runs.sqlite3")),
            gate: Arc::new(Semaphore::new(1)),
        };
        let connection = store.connect()?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS runs (
                 id TEXT PRIMARY KEY, state TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS run_resources (
                 seq INTEGER PRIMARY KEY AUTOINCREMENT,
                 run_id TEXT NOT NULL REFERENCES runs(id),
                 kind TEXT NOT NULL, id TEXT NOT NULL,
                 state TEXT NOT NULL, token TEXT NOT NULL,
                 request TEXT NOT NULL, UNIQUE(kind, id)
             );",
        )?;
        fs::set_permissions(store.path.as_ref(), fs::Permissions::from_mode(0o600))?;
        Ok(store)
    }

    fn connect(&self) -> Result<Connection> {
        let connection = Connection::open(self.path.as_ref())?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")?;
        Ok(connection)
    }

    pub fn create(&self, id: &str) -> Result<Run> {
        validate_run_id(id)?;
        self.connect()?.execute(
            "INSERT INTO runs(id, state) VALUES (?1, 'active') ON CONFLICT(id) DO NOTHING",
            [id],
        )?;
        self.get(id)?.context("run disappeared")
    }

    pub fn list(&self) -> Result<Vec<Run>> {
        let connection = self.connect()?;
        let mut query = connection.prepare("SELECT id FROM runs ORDER BY id")?;
        let ids = query
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.iter()
            .map(|id| self.get(id)?.context("run disappeared"))
            .collect()
    }

    pub fn get(&self, id: &str) -> Result<Option<Run>> {
        let connection = self.connect()?;
        let state = connection
            .query_row("SELECT state FROM runs WHERE id=?1", [id], |row| row.get(0))
            .optional()?;
        let Some(state) = state else { return Ok(None) };
        let mut query = connection.prepare(
            "SELECT kind, id, state, token, request FROM run_resources WHERE run_id=?1 ORDER BY seq",
        )?;
        let resources = query
            .query_map([id], |row| {
                Ok(RunResource {
                    kind: row.get(0)?,
                    id: row.get(1)?,
                    state: row.get(2)?,
                    token: row.get(3)?,
                    request: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Some(Run {
            id: id.to_string(),
            state,
            resources,
        }))
    }

    /// Callers serialize this with provisioning and cleanup using `gate`.
    pub fn reserve(
        &self,
        run_id: &str,
        kind: &str,
        id: &str,
        request: &str,
    ) -> Result<RunResource> {
        let run = self.get(run_id)?.context("run not found")?;
        if run.state != "active" {
            bail!("run is not active");
        }
        if let Some(existing) = run
            .resources
            .into_iter()
            .find(|r| r.kind == kind && r.id == id)
        {
            if existing.request != request {
                bail!("resource intent differs from the recorded request");
            }
            if existing.state == "deleted" {
                bail!("resource has already been deleted");
            }
            return Ok(existing);
        }
        let resource = RunResource {
            kind: kind.to_string(),
            id: id.to_string(),
            state: "creating".to_string(),
            token: Uuid::new_v4().to_string(),
            request: request.to_string(),
        };
        self.connect()?.execute(
            "INSERT INTO run_resources(run_id, kind, id, state, token, request) VALUES (?1, ?2, ?3, 'creating', ?4, ?5)",
            params![run_id, kind, id, resource.token, request],
        ).context("resource ID is already reserved by another run")?;
        Ok(resource)
    }

    pub fn resource_state(&self, resource: &RunResource, state: &str) -> Result<()> {
        self.connect()?.execute(
            "UPDATE run_resources SET state=?1 WHERE token=?2",
            params![state, resource.token],
        )?;
        Ok(())
    }

    pub fn run_state(&self, id: &str, state: &str) -> Result<()> {
        self.connect()?
            .execute("UPDATE runs SET state=?1 WHERE id=?2", params![state, id])?;
        Ok(())
    }
}

pub fn validate_run_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 63
        || !id.as_bytes()[0].is_ascii_alphanumeric()
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
    {
        bail!(
            "run ID must be 1-63 ASCII letters, numbers, dots, underscores or hyphens, starting with a letter or number"
        );
    }
    Ok(())
}

fn private_directory(path: &Path) -> Result<()> {
    match DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
        bail!(
            "{} must be a private, non-symlink directory",
            path.display()
        );
    }
    Ok(())
}

pub fn pin_path(root: &Path, resource: &RunResource) -> PathBuf {
    root.join(".run-pins").join(&resource.token)
}

/// Called while the resource mutation lock is held, before publication.
pub fn pin_file(root: &Path, resource: &RunResource, staging: &Path) -> Result<()> {
    let directory = root.join(".run-pins");
    private_directory(&directory)?;
    let pin = pin_path(root, resource);
    File::open(staging)?.sync_all()?;
    fs::hard_link(staging, &pin)?;
    File::open(directory)?.sync_all()?;
    Ok(())
}

/// Missing is safe to clean; an existing file without a matching pin is not.
pub fn check_file(root: &Path, resource: &RunResource) -> Result<bool> {
    let path = root.join(&resource.id);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    let pin =
        fs::symlink_metadata(pin_path(root, resource)).context("resource has no ownership pin")?;
    if !metadata.is_file()
        || !pin.is_file()
        || metadata.dev() != pin.dev()
        || metadata.ino() != pin.ino()
    {
        bail!("resource identity mismatch: {}", resource.id);
    }
    Ok(true)
}

pub fn unpin_file(root: &Path, resource: &RunResource) -> Result<()> {
    match fs::remove_file(pin_path(root, resource)) {
        Ok(()) => File::open(root.join(".run-pins"))?.sync_all()?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests;
