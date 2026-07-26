use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Sender},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    config::{FedoraMirror, VmInstallFedoraArgs},
    installer::{self, InstallControl},
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum JobStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
}

impl JobStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "interrupted" => Ok(Self::Interrupted),
            _ => bail!("unknown job status {value:?}"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FedoraInstallRequest {
    pub name: String,
    pub media_id: String,
    pub image_id: String,
    pub ssh_authorized_key: String,
    #[serde(default = "default_disk_size")]
    pub disk_size: String,
    #[serde(default = "default_memory_mib")]
    pub memory_mib: u64,
    #[serde(default = "default_vcpus")]
    pub vcpus: u32,
    #[serde(default = "default_network")]
    pub network: String,
    pub hostname: Option<String>,
    #[serde(default)]
    pub mirror: FedoraInstallMirror,
    #[serde(default = "default_install_timeout")]
    pub timeout_secs: u64,
    #[serde(default = "default_verify_timeout")]
    pub verify_timeout_secs: u64,
    #[serde(default)]
    pub keep_failed: bool,
}

impl FedoraInstallRequest {
    pub fn validate(&self) -> Result<()> {
        validate_id(&self.name, "VM name")?;
        validate_id(&self.media_id, "media ID")?;
        validate_id(&self.image_id, "image ID")?;
        if self.ssh_authorized_key.trim().is_empty() {
            bail!("SSH authorized key must not be empty");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum FedoraInstallMirror {
    #[default]
    Official,
    Tuna,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct InstallJob {
    pub id: String,
    pub status: JobStatus,
    pub phase: String,
    pub cancel_requested: bool,
    pub request: FedoraInstallRequest,
    pub error: Option<String>,
    pub created_at_ms: i64,
    pub started_at_ms: Option<i64>,
    pub finished_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ManagedResource {
    pub id: String,
    pub size_bytes: u64,
    pub modified_at_ms: Option<i64>,
}

pub enum IsoDeleteOutcome {
    Deleted,
    NotFound,
    InUse(String),
}

pub enum IsoPublishOutcome {
    Created(ManagedResource),
    Exists,
}

#[derive(Clone)]
struct JobStore {
    path: Arc<PathBuf>,
}

impl JobStore {
    fn open(path: PathBuf) -> Result<Self> {
        let store = Self {
            path: Arc::new(path),
        };
        let connection = store.connect()?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS install_jobs (
                 id TEXT PRIMARY KEY,
                 request_json TEXT NOT NULL,
                 status TEXT NOT NULL,
                 phase TEXT NOT NULL,
                 cancel_requested INTEGER NOT NULL DEFAULT 0,
                 error TEXT,
                 created_at_ms INTEGER NOT NULL,
                 started_at_ms INTEGER,
                 finished_at_ms INTEGER
             );",
        )?;
        connection.execute(
            "UPDATE install_jobs
             SET status = 'interrupted', phase = 'interrupted', finished_at_ms = ?1,
                 error = 'server stopped while the job was running'
             WHERE status = 'running'",
            [now_ms()],
        )?;
        Ok(store)
    }

    fn connect(&self) -> Result<Connection> {
        let connection = Connection::open(self.path.as_ref())
            .with_context(|| format!("failed to open job database {}", self.path.display()))?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(connection)
    }

    fn create(&self, request: &FedoraInstallRequest) -> Result<InstallJob> {
        let id = Uuid::new_v4().to_string();
        let request_json = serde_json::to_string(request)?;
        self.connect()?.execute(
            "INSERT INTO install_jobs
             (id, request_json, status, phase, created_at_ms)
             VALUES (?1, ?2, 'queued', 'queued', ?3)",
            params![id, request_json, now_ms()],
        )?;
        self.get(&id)?.context("new job disappeared")
    }

    fn get(&self, id: &str) -> Result<Option<InstallJob>> {
        self.connect()?
            .query_row(
                "SELECT id, request_json, status, phase, cancel_requested, error,
                        created_at_ms, started_at_ms, finished_at_ms
                 FROM install_jobs WHERE id = ?1",
                [id],
                row_to_job,
            )
            .optional()
            .map_err(Into::into)
    }

    fn list(&self) -> Result<Vec<InstallJob>> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT id, request_json, status, phase, cancel_requested, error,
                    created_at_ms, started_at_ms, finished_at_ms
             FROM install_jobs ORDER BY created_at_ms DESC",
        )?;
        statement
            .query_map([], row_to_job)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    fn queued_ids(&self) -> Result<Vec<String>> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT id FROM install_jobs WHERE status = 'queued' ORDER BY created_at_ms",
        )?;
        statement
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    fn active_media_user(&self, media_id: &str) -> Result<Option<String>> {
        Ok(self
            .list()?
            .into_iter()
            .find(|job| {
                job.request.media_id == media_id
                    && matches!(
                        job.status,
                        JobStatus::Queued | JobStatus::Running | JobStatus::Interrupted
                    )
            })
            .map(|job| job.id))
    }

    fn claim(&self, id: &str) -> Result<Option<FedoraInstallRequest>> {
        let connection = self.connect()?;
        let changed = connection.execute(
            "UPDATE install_jobs SET status = 'running', phase = 'planning', started_at_ms = ?2
             WHERE id = ?1 AND status = 'queued' AND cancel_requested = 0",
            params![id, now_ms()],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let json: String = connection.query_row(
            "SELECT request_json FROM install_jobs WHERE id = ?1",
            [id],
            |row| row.get(0),
        )?;
        Ok(Some(serde_json::from_str(&json)?))
    }

    fn set_phase(&self, id: &str, phase: &str) -> Result<()> {
        self.connect()?.execute(
            "UPDATE install_jobs SET phase = ?2 WHERE id = ?1 AND status = 'running'",
            params![id, phase],
        )?;
        Ok(())
    }

    fn finish(&self, id: &str, status: JobStatus, error: Option<&str>) -> Result<()> {
        self.connect()?.execute(
            "UPDATE install_jobs SET status = ?2, phase = ?2, error = ?3, finished_at_ms = ?4
             WHERE id = ?1",
            params![id, status.as_str(), error, now_ms()],
        )?;
        Ok(())
    }

    fn request_cancel(&self, id: &str) -> Result<Option<InstallJob>> {
        let connection = self.connect()?;
        connection.execute(
            "UPDATE install_jobs
             SET cancel_requested = 1,
                 status = CASE WHEN status = 'queued' THEN 'cancelled' ELSE status END,
                 phase = CASE WHEN status = 'queued' THEN 'cancelled' ELSE phase END,
                 finished_at_ms = CASE WHEN status = 'queued' THEN ?2 ELSE finished_at_ms END
             WHERE id = ?1 AND status IN ('queued', 'running')",
            params![id, now_ms()],
        )?;
        drop(connection);
        self.get(id)
    }
}

fn row_to_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<InstallJob> {
    let request_json: String = row.get(1)?;
    let status: String = row.get(2)?;
    let request = serde_json::from_str(&request_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            request_json.len(),
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })?;
    let status = JobStatus::parse(&status).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            status.len(),
            rusqlite::types::Type::Text,
            error.into(),
        )
    })?;
    Ok(InstallJob {
        id: row.get(0)?,
        request,
        status,
        phase: row.get(3)?,
        cancel_requested: row.get(4)?,
        error: row.get(5)?,
        created_at_ms: row.get(6)?,
        started_at_ms: row.get(7)?,
        finished_at_ms: row.get(8)?,
    })
}

#[derive(Clone)]
pub struct JobService {
    store: JobStore,
    sender: Sender<String>,
    controls: Arc<Mutex<std::collections::HashMap<String, Arc<AtomicBool>>>>,
    roots: Arc<JobRoots>,
    resource_lock: Arc<Mutex<()>>,
}

#[derive(Clone)]
pub struct JobRoots {
    pub state: PathBuf,
    pub images: PathBuf,
    pub media: PathBuf,
    pub logs: PathBuf,
    pub connect_uri: String,
}

impl JobService {
    pub fn start(roots: JobRoots) -> Result<Self> {
        prepare_roots(&roots)?;
        let roots = Arc::new(roots);
        let store = JobStore::open(roots.state.join("jobs.sqlite3"))?;
        let (sender, receiver) = mpsc::channel::<String>();
        let controls = Arc::new(Mutex::new(std::collections::HashMap::new()));
        let worker_store = store.clone();
        let worker_controls = controls.clone();
        let worker_roots = roots.clone();
        std::thread::Builder::new()
            .name("qtr-install-worker".to_string())
            .spawn(move || {
                while let Ok(id) = receiver.recv() {
                    run_job(&worker_store, &worker_controls, &worker_roots, &id);
                }
            })
            .context("failed to start install worker")?;
        let service = Self {
            store,
            sender,
            controls,
            roots,
            resource_lock: Arc::new(Mutex::new(())),
        };
        for id in service.store.queued_ids()? {
            service.sender.send(id)?;
        }
        Ok(service)
    }

    pub fn create(&self, request: FedoraInstallRequest) -> Result<InstallJob> {
        let _guard = self.lock_resources()?;
        request.validate()?;
        self.resolve_media(&request.media_id)?;
        let job = self.store.create(&request)?;
        self.sender.send(job.id.clone())?;
        Ok(job)
    }

    pub fn get(&self, id: &str) -> Result<Option<InstallJob>> {
        self.store.get(id)
    }

    pub fn list(&self) -> Result<Vec<InstallJob>> {
        self.store.list()
    }

    pub fn cancel(&self, id: &str) -> Result<Option<InstallJob>> {
        let job = self.store.request_cancel(id)?;
        if let Ok(controls) = self.controls.lock()
            && let Some(control) = controls.get(id)
        {
            control.store(true, Ordering::Relaxed);
        }
        Ok(job)
    }

    pub fn list_images(&self) -> Result<Vec<ManagedResource>> {
        list_resources(&self.roots.images)
    }

    pub fn list_media(&self) -> Result<Vec<ManagedResource>> {
        list_resources(&self.roots.media)
    }

    pub fn resolve_image(&self, id: &str) -> Result<PathBuf> {
        resolve_resource(&self.roots.images, id, "image")
    }

    pub fn resolve_media(&self, id: &str) -> Result<PathBuf> {
        resolve_resource(&self.roots.media, id, "media")
    }

    pub fn create_iso_staging_path(&self) -> Result<PathBuf> {
        let directory = self.roots.media.join(".uploads");
        std::fs::create_dir_all(&directory)
            .with_context(|| format!("failed to create {}", directory.display()))?;
        Ok(directory.join(format!("{}.partial", Uuid::new_v4())))
    }

    pub fn validate_iso_id(&self, id: &str) -> Result<()> {
        validate_iso_id(id)
    }

    pub fn publish_iso(&self, id: &str, staging: &Path) -> Result<IsoPublishOutcome> {
        let _guard = self.lock_resources()?;
        validate_iso_id(id)?;
        let destination = self.roots.media.join(id);
        match std::fs::symlink_metadata(&destination) {
            Ok(_) => return Ok(IsoPublishOutcome::Exists),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        match std::fs::hard_link(staging, &destination) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Ok(IsoPublishOutcome::Exists);
            }
            Err(error) => return Err(error.into()),
        }
        std::fs::remove_file(staging)?;
        Ok(IsoPublishOutcome::Created(resource_from_path(
            id,
            &destination,
        )?))
    }

    pub fn delete_iso<F>(&self, id: &str, vm_users: F) -> Result<IsoDeleteOutcome>
    where
        F: FnOnce(&Path) -> Result<Vec<String>>,
    {
        let _guard = self.lock_resources()?;
        validate_iso_id(id)?;
        let path = self.roots.media.join(id);
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(IsoDeleteOutcome::NotFound);
            }
            Err(error) => return Err(error.into()),
        };
        if !metadata.file_type().is_file() {
            bail!("ISO {id:?} is not a regular file");
        }
        if let Some(job_id) = self.store.active_media_user(id)? {
            return Ok(IsoDeleteOutcome::InUse(format!(
                "ISO is referenced by automated install job {job_id}"
            )));
        }
        if let Some(vm_name) = vm_users(&path)?.into_iter().next() {
            return Ok(IsoDeleteOutcome::InUse(format!(
                "ISO is attached to VM {vm_name}"
            )));
        }
        std::fs::remove_file(path)?;
        Ok(IsoDeleteOutcome::Deleted)
    }

    pub fn with_resource_lock<T>(&self, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        let _guard = self.lock_resources()?;
        operation()
    }

    pub fn serial_log_path(&self, vm_name: &str) -> Result<PathBuf> {
        validate_id(vm_name, "VM name")?;
        Ok(self.roots.logs.join(format!("{vm_name}.serial.log")))
    }

    fn lock_resources(&self) -> Result<std::sync::MutexGuard<'_, ()>> {
        self.resource_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("resource operation lock poisoned"))
    }
}

fn run_job(
    store: &JobStore,
    controls: &Mutex<std::collections::HashMap<String, Arc<AtomicBool>>>,
    roots: &JobRoots,
    id: &str,
) {
    let result = (|| -> Result<()> {
        let cancelled = Arc::new(AtomicBool::new(false));
        controls
            .lock()
            .map_err(|_| anyhow::anyhow!("install control lock poisoned"))?
            .insert(id.to_string(), cancelled.clone());
        let Some(request) = store.claim(id)? else {
            controls.lock().unwrap().remove(id);
            return Ok(());
        };
        let phase_store = store.clone();
        let phase_id = id.to_string();
        let control = InstallControl::new(cancelled.clone(), move |phase| {
            if let Err(error) = phase_store.set_phase(&phase_id, phase) {
                tracing::error!(%error, job_id = %phase_id, "failed to persist install phase");
            }
        });
        let args = match resolve_request(roots, id, &request) {
            Ok(args) => args,
            Err(error) => {
                controls.lock().unwrap().remove(id);
                return Err(error);
            }
        };
        let key_path = args.ssh_key.clone();
        let install_result = installer::install_fedora_with_control(args, &control);
        let _ = std::fs::remove_file(key_path);
        controls.lock().unwrap().remove(id);
        match install_result {
            Ok(()) => store.finish(id, JobStatus::Succeeded, None)?,
            Err(error) if cancelled.load(Ordering::Relaxed) => {
                store.finish(id, JobStatus::Cancelled, Some(&format!("{error:#}")))?
            }
            Err(error) => store.finish(id, JobStatus::Failed, Some(&format!("{error:#}")))?,
        }
        Ok(())
    })();
    if let Err(error) = result {
        tracing::error!(%error, job_id = id, "install worker failed");
        let _ = store.finish(id, JobStatus::Failed, Some(&format!("{error:#}")));
    }
}

fn resolve_request(
    roots: &JobRoots,
    id: &str,
    request: &FedoraInstallRequest,
) -> Result<VmInstallFedoraArgs> {
    let ssh_key = roots.state.join("jobs").join(format!("{id}.pub"));
    std::fs::write(&ssh_key, format!("{}\n", request.ssh_authorized_key.trim()))?;
    Ok(VmInstallFedoraArgs {
        name: request.name.clone(),
        iso: resolve_resource(&roots.media, &request.media_id, "ISO")?,
        disk: roots.images.join(&request.image_id),
        disk_size: request.disk_size.clone(),
        output: roots
            .state
            .join("vms")
            .join(format!("{}.yaml", request.name)),
        serial_log: Some(roots.logs.join(format!("{}.serial.log", request.name))),
        install_log: Some(roots.logs.join(format!("{}.install.log", request.name))),
        ssh_key,
        memory_mib: request.memory_mib,
        vcpus: request.vcpus,
        network: request.network.clone(),
        hostname: request.hostname.clone(),
        mirror: match request.mirror {
            FedoraInstallMirror::Official => FedoraMirror::Official,
            FedoraInstallMirror::Tuna => FedoraMirror::Tuna,
        },
        timeout_secs: request.timeout_secs,
        verify_timeout_secs: request.verify_timeout_secs,
        connect_uri: roots.connect_uri.clone(),
        dry_run: false,
        keep_failed: request.keep_failed,
    })
}

fn prepare_roots(roots: &JobRoots) -> Result<()> {
    for path in [
        roots.state.as_path(),
        roots.images.as_path(),
        roots.logs.as_path(),
        roots.media.as_path(),
        roots.state.join("jobs").as_path(),
        roots.state.join("vms").as_path(),
        roots.media.join(".uploads").as_path(),
    ] {
        std::fs::create_dir_all(path)
            .with_context(|| format!("failed to create {}", path.display()))?;
    }
    cleanup_staging(&roots.media.join(".uploads"))?;
    Ok(())
}

fn list_resources(root: &Path) -> Result<Vec<ManagedResource>> {
    let mut resources = Vec::new();
    for entry in std::fs::read_dir(root)
        .with_context(|| format!("failed to read resource root {}", root.display()))?
    {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if !file_type.is_file() {
            continue;
        }
        let metadata = entry.metadata()?;
        let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let modified_at_ms = metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .and_then(|value| value.as_millis().try_into().ok());
        resources.push(ManagedResource {
            id,
            size_bytes: metadata.len(),
            modified_at_ms,
        });
    }
    resources.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(resources)
}

fn resolve_resource(root: &Path, id: &str, kind: &str) -> Result<PathBuf> {
    validate_id(id, &format!("{kind} ID"))?;
    let path = root.join(id);
    let metadata = path
        .symlink_metadata()
        .with_context(|| format!("{kind} {id:?} does not exist"))?;
    if !metadata.file_type().is_file() {
        bail!("{kind} {id:?} is not a regular file");
    }
    Ok(path)
}

fn validate_iso_id(id: &str) -> Result<()> {
    validate_id(id, "ISO ID")?;
    if id.len() > 255 {
        bail!("ISO ID must not exceed 255 bytes");
    }
    if id.starts_with('.') {
        bail!("ISO ID must not start with a dot");
    }
    if !id.to_ascii_lowercase().ends_with(".iso") {
        bail!("ISO ID must end with .iso");
    }
    Ok(())
}

fn resource_from_path(id: &str, path: &Path) -> Result<ManagedResource> {
    let metadata = path.symlink_metadata()?;
    if !metadata.file_type().is_file() {
        bail!("resource {id:?} is not a regular file");
    }
    let modified_at_ms = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .and_then(|value| value.as_millis().try_into().ok());
    Ok(ManagedResource {
        id: id.to_string(),
        size_bytes: metadata.len(),
        modified_at_ms,
    })
}

fn cleanup_staging(directory: &Path) -> Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.ends_with(".partial") && entry.file_type()?.is_file() {
            std::fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

fn validate_id(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        bail!("{label} must contain only letters, numbers, dot, underscore or hyphen");
    }
    Ok(())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

fn default_disk_size() -> String {
    "40GiB".to_string()
}
fn default_memory_mib() -> u64 {
    4096
}
fn default_vcpus() -> u32 {
    2
}
fn default_network() -> String {
    "default".to_string()
}
fn default_install_timeout() -> u64 {
    7200
}
fn default_verify_timeout() -> u64 {
    300
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> FedoraInstallRequest {
        FedoraInstallRequest {
            name: "fedora-test".to_string(),
            media_id: "Fedora-Server.iso".to_string(),
            image_id: "fedora-test.qcow2".to_string(),
            ssh_authorized_key: "ssh-ed25519 AAAA test".to_string(),
            disk_size: default_disk_size(),
            memory_mib: default_memory_mib(),
            vcpus: default_vcpus(),
            network: default_network(),
            hostname: None,
            mirror: FedoraInstallMirror::Official,
            timeout_secs: default_install_timeout(),
            verify_timeout_secs: default_verify_timeout(),
            keep_failed: false,
        }
    }

    fn store() -> (PathBuf, JobStore) {
        let directory = std::env::temp_dir().join(format!("qtr-job-test-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let store = JobStore::open(directory.join("jobs.sqlite3")).unwrap();
        (directory, store)
    }

    #[test]
    fn persists_and_cancels_queued_jobs() {
        let (directory, store) = store();
        let job = store.create(&request()).unwrap();
        assert_eq!(job.status, JobStatus::Queued);
        assert_eq!(store.list().unwrap().len(), 1);
        assert_eq!(
            store.active_media_user("Fedora-Server.iso").unwrap(),
            Some(job.id.clone())
        );

        let cancelled = store.request_cancel(&job.id).unwrap().unwrap();
        assert_eq!(cancelled.status, JobStatus::Cancelled);
        assert!(cancelled.cancel_requested);
        assert!(cancelled.finished_at_ms.is_some());
        assert!(store
            .active_media_user("Fedora-Server.iso")
            .unwrap()
            .is_none());
        assert!(store.claim(&job.id).unwrap().is_none());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn marks_running_jobs_interrupted_when_store_reopens() {
        let (directory, store) = store();
        let job = store.create(&request()).unwrap();
        assert!(store.claim(&job.id).unwrap().is_some());
        drop(store);

        let reopened = JobStore::open(directory.join("jobs.sqlite3")).unwrap();
        let interrupted = reopened.get(&job.id).unwrap().unwrap();
        assert_eq!(interrupted.status, JobStatus::Interrupted);
        assert_eq!(interrupted.phase, "interrupted");
        assert!(interrupted.finished_at_ms.is_some());
        drop(reopened);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rejects_resource_ids_that_can_escape_roots() {
        let mut request = request();
        request.media_id = "../Fedora.iso".to_string();
        assert!(request.validate().is_err());
        request.media_id = "/tmp/Fedora.iso".to_string();
        assert!(request.validate().is_err());
        request.media_id = "Fedora-Server.iso".to_string();
        assert!(request.validate().is_ok());
    }

    #[test]
    fn lists_only_regular_resources_in_stable_order() {
        let directory = std::env::temp_dir().join(format!("qtr-resource-test-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("b.iso"), b"bb").unwrap();
        std::fs::write(directory.join("a.iso"), b"a").unwrap();
        std::fs::create_dir(directory.join("ignored")).unwrap();
        std::os::unix::fs::symlink(directory.join("a.iso"), directory.join("linked.iso")).unwrap();

        let resources = list_resources(&directory).unwrap();
        assert_eq!(resources.len(), 2);
        assert_eq!(resources[0].id, "a.iso");
        assert_eq!(resources[0].size_bytes, 1);
        assert_eq!(resources[1].id, "b.iso");
        assert!(resolve_resource(&directory, "linked.iso", "ISO").is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn validates_public_iso_ids() {
        assert!(validate_iso_id("Fedora-Server.iso").is_ok());
        assert!(validate_iso_id("Fedora-Server.ISO").is_ok());
        assert!(validate_iso_id("Fedora-Server.img").is_err());
        assert!(validate_iso_id(".hidden.iso").is_err());
        assert!(validate_iso_id("../escape.iso").is_err());
        assert!(validate_iso_id(&format!("{}.iso", "a".repeat(252))).is_err());
    }
}
