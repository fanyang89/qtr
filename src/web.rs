use std::{
    collections::HashMap,
    path::{Path as FsPath, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    body::Body,
    extract::{
        Path, Query, Request, State,
        rejection::{JsonRejection, QueryRejection},
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade, close_code},
    },
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::any,
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio::{
    fs::OpenOptions,
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
};
use tower_http::{
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};
use utoipa::OpenApi as _;
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa_axum::{router::OpenApiRouter, routes};
use utoipa_swagger_ui::SwaggerUi;
use uuid::Uuid;
use virt::error::clear_error_callback;

use crate::config::GraphicsMode;
use crate::{
    config::WebArgs,
    jobs::{
        FedoraInstallRequest, ImageCreateOutcome, ImageDeleteOutcome, ImageResizeOutcome,
        InstallJob, InstallJobCreateOutcome, IsoDeleteOutcome, IsoPublishOutcome, JobRoots,
        JobService, ManagedImage, ManagedImageStatus, ManagedIso, ManagedIsoStatus,
    },
    network, vm,
};

#[derive(Clone)]
struct AppState {
    connect_uri: String,
    api_token: Arc<str>,
    vnc_tickets: Arc<Mutex<HashMap<String, VncTicket>>>,
    jobs: Option<JobService>,
    max_iso_upload_bytes: u64,
    iso_uploads: Arc<Semaphore>,
}

#[derive(utoipa::OpenApi)]
struct ApiDoc;

#[derive(Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
struct HealthStatus {
    ok: bool,
    libvirt_uri: String,
    version: &'static str,
}

#[derive(Serialize, utoipa::ToSchema)]
struct ProblemDetails {
    #[serde(rename = "type")]
    problem_type: &'static str,
    title: &'static str,
    status: u16,
    detail: String,
}

impl ProblemDetails {
    fn response(status: StatusCode, title: &'static str, detail: impl Into<String>) -> Response {
        let mut response = (
            status,
            Json(Self {
                problem_type: "about:blank",
                title,
                status: status.as_u16(),
                detail: detail.into(),
            }),
        )
            .into_response();
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        response
    }
}

struct VncTicket {
    vm_name: String,
    expires_at: Instant,
}

#[derive(Deserialize)]
struct VncTicketQuery {
    ticket: String,
}

#[derive(Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
struct VncTicketResponse {
    ticket: String,
    expires_in_seconds: u64,
}

#[derive(Debug)]
enum AppError {
    Vm(vm::VmApiError),
    BadRequest(anyhow::Error),
    NotFound,
    Conflict(String),
    PayloadTooLarge,
    UnsupportedMediaType,
    Internal(anyhow::Error),
}

type AppResult<T> = std::result::Result<T, AppError>;

impl AppError {
    fn status(&self) -> StatusCode {
        match self {
            Self::Vm(vm::VmApiError::InvalidRequest(_)) => StatusCode::BAD_REQUEST,
            Self::Vm(vm::VmApiError::NotFound(_)) => StatusCode::NOT_FOUND,
            Self::Vm(vm::VmApiError::Conflict(_)) => StatusCode::CONFLICT,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::UnsupportedMediaType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::Vm(vm::VmApiError::Internal(_)) | Self::Internal(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    }

    fn error(&self) -> &dyn std::fmt::Debug {
        match self {
            Self::Vm(error) => error,
            Self::BadRequest(error) => error,
            Self::NotFound => &"resource not found",
            Self::Conflict(error) => error,
            Self::PayloadTooLarge => &"payload too large",
            Self::UnsupportedMediaType => &"unsupported media type",
            Self::Internal(error) => error,
        }
    }
}

impl From<anyhow::Error> for AppError {
    fn from(error: anyhow::Error) -> Self {
        Self::Internal(error)
    }
}

impl From<vm::VmApiError> for AppError {
    fn from(error: vm::VmApiError) -> Self {
        Self::Vm(error)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        tracing::error!(error = ?self.error(), %status, "request failed");
        let (title, detail) = match &self {
            Self::Vm(vm::VmApiError::InvalidRequest(error)) => ("Bad Request", error.to_string()),
            Self::BadRequest(error) => ("Bad Request", error.to_string()),
            Self::Vm(vm::VmApiError::NotFound(detail)) => ("Not Found", detail.clone()),
            Self::NotFound => ("Not Found", "The resource was not found.".to_string()),
            Self::Conflict(detail) => ("Conflict", detail.clone()),
            Self::PayloadTooLarge => (
                "Payload Too Large",
                "The ISO exceeds the configured upload limit.".to_string(),
            ),
            Self::UnsupportedMediaType => (
                "Unsupported Media Type",
                "ISO uploads require application/octet-stream.".to_string(),
            ),
            Self::Vm(vm::VmApiError::Conflict(error)) => ("Conflict", error.to_string()),
            Self::Vm(vm::VmApiError::Internal(_)) | Self::Internal(_) => (
                "Internal Server Error",
                "The server could not complete the request.".to_string(),
            ),
        };
        ProblemDetails::response(status, title, detail)
    }
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateVmRequest {
    name: String,
    resources: CreateVmResources,
    disks: Vec<CreateVmDisk>,
    network_id: String,
    media_id: Option<String>,
    console: CreateVmConsole,
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateVmResources {
    vcpus: u32,
    #[serde(rename = "memoryMib")]
    memory_mib: u64,
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateVmDisk {
    image_id: String,
    format: crate::config::DiskFormat,
    #[serde(default)]
    bus: vm::VmDiskBus,
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateVmConsole {
    graphics: GraphicsMode,
    #[serde(default)]
    serial_log: bool,
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateImageRequest {
    id: String,
    format: crate::config::DiskFormat,
    size_bytes: u64,
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AttachImageRequest {
    #[serde(default)]
    bus: vm::VmDiskBus,
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AddCdromTrayRequest {
    id: String,
    media_id: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SetCdromMediaRequest {
    media_id: String,
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResizeImageRequest {
    size_bytes: u64,
}

#[derive(Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
struct ManagedImageResponse {
    id: String,
    size_bytes: u64,
    virtual_size_bytes: Option<u64>,
    modified_at_ms: Option<i64>,
    format: Option<crate::config::DiskFormat>,
    status: ManagedImageStatus,
    attachments: Vec<vm::VmImageAttachment>,
    reserved_by_job_id: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
struct ManagedIsoResponse {
    id: String,
    size_bytes: u64,
    modified_at_ms: Option<i64>,
    status: ManagedIsoStatus,
    attachments: Vec<vm::VmIsoAttachment>,
    reserved_by_job_ids: Vec<String>,
}

impl ManagedIsoResponse {
    fn new(
        iso: ManagedIso,
        attachments: Vec<vm::VmIsoAttachment>,
        reserved_by_job_ids: Vec<String>,
    ) -> Self {
        Self {
            id: iso.id,
            size_bytes: iso.size_bytes,
            modified_at_ms: iso.modified_at_ms,
            status: iso.status,
            attachments,
            reserved_by_job_ids,
        }
    }
}

impl ManagedImageResponse {
    fn new(
        image: ManagedImage,
        attachments: Vec<vm::VmImageAttachment>,
        reserved_by_job_id: Option<String>,
    ) -> Self {
        Self {
            id: image.id,
            size_bytes: image.size_bytes,
            virtual_size_bytes: image.virtual_size_bytes,
            modified_at_ms: image.modified_at_ms,
            format: image.format,
            status: image.status,
            attachments,
            reserved_by_job_id,
        }
    }
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateVmRequest {
    name: String,
    #[schema(value_type = Option<Object>)]
    machine: Option<vm::VmMachine>,
    #[schema(value_type = Option<Object>)]
    cpu: Option<vm::VmCpu>,
    #[schema(value_type = Option<Object>)]
    memory: Option<vm::VmMemory>,
    io_threads: Option<vm::VmIoThreads>,
    #[schema(value_type = Vec<Object>)]
    disks: Vec<vm::VmDisk>,
    #[schema(value_type = Option<String>)]
    cdrom: Option<PathBuf>,
    boot: Option<Vec<String>>,
    #[serde(default = "default_memory_gib", rename = "memoryGiB")]
    memory_gib: u64,
    #[serde(default = "default_vcpus")]
    vcpus: u32,
    #[serde(default = "default_network")]
    network: String,
    #[serde(default = "default_graphics")]
    graphics: GraphicsMode,
    #[serde(default = "default_vnc_listen")]
    vnc_listen: String,
    vnc_port: Option<u16>,
    #[schema(value_type = Option<String>)]
    serial_log: Option<PathBuf>,
}

impl UpdateVmRequest {
    fn into_manifest(self) -> vm::VmManifest {
        vm::VmManifest {
            name: self.name,
            machine: self.machine,
            cpu: self.cpu,
            memory: self.memory,
            io_threads: self.io_threads,
            disks: self
                .disks
                .into_iter()
                .map(vm::VmDiskEntry::present)
                .collect(),
            cdrom: self.cdrom,
            cdroms: None,
            boot: self.boot,
            memory_gib: self.memory_gib,
            vcpus: self.vcpus,
            network: Some(self.network),
            interfaces: None,
            graphics: self.graphics,
            vnc_listen: self.vnc_listen,
            vnc_port: self.vnc_port,
            serial_log: self.serial_log,
        }
    }
}

fn default_memory_gib() -> u64 {
    4
}

fn default_vcpus() -> u32 {
    2
}

fn default_network() -> String {
    "default".to_string()
}

fn default_graphics() -> GraphicsMode {
    GraphicsMode::Vnc
}

fn default_vnc_listen() -> String {
    "127.0.0.1".to_string()
}

impl CreateVmRequest {
    fn into_manifest(self, jobs: &JobService) -> Result<vm::VmManifest> {
        if self.disks.is_empty() {
            anyhow::bail!("at least one managed image is required");
        }
        if self.resources.memory_mib == 0 {
            anyhow::bail!("memoryMib must be greater than zero");
        }
        if self.resources.vcpus == 0 {
            anyhow::bail!("vcpus must be greater than zero");
        }
        let disks = self
            .disks
            .into_iter()
            .map(|disk| {
                let image = jobs.inspect_image(&disk.image_id)?;
                let format = image
                    .format
                    .with_context(|| format!("image {:?} is not a valid disk", disk.image_id))?;
                if disk.format != format {
                    anyhow::bail!(
                        "image {:?} uses {} format, not {}",
                        disk.image_id,
                        format.as_qemu_arg(),
                        disk.format.as_qemu_arg()
                    );
                }
                let path = jobs.resolve_image(&disk.image_id)?;
                Ok(vm::VmDiskEntry::present(vm::VmDisk {
                    id: Some(format!("image-{}", Uuid::new_v4().simple())),
                    disk_type: vm::VmDiskType::File,
                    path,
                    format,
                    target: None,
                    bus: disk.bus,
                    cache: None,
                    io: None,
                    discard: None,
                    detect_zeroes: None,
                    readonly: None,
                    serial: Default::default(),
                    io_tune: Default::default(),
                }))
            })
            .collect::<Result<Vec<_>>>()?;
        let cdrom = self
            .media_id
            .as_deref()
            .map(|id| jobs.resolve_media(id))
            .transpose()?;
        let serial_log = self
            .console
            .serial_log
            .then(|| jobs.serial_log_path(&self.name))
            .transpose()?;
        let has_cdrom = cdrom.is_some();
        Ok(vm::VmManifest {
            name: self.name,
            machine: None,
            cpu: None,
            memory: Some(vm::VmMemory {
                size_mib: self.resources.memory_mib,
                max_mib: None,
            }),
            io_threads: None,
            disks,
            cdrom,
            cdroms: None,
            boot: Some(if has_cdrom {
                vec!["cdrom".to_string(), "hd".to_string()]
            } else {
                vec!["hd".to_string()]
            }),
            memory_gib: self.resources.memory_mib.div_ceil(1024),
            vcpus: self.resources.vcpus,
            network: Some(self.network_id),
            interfaces: None,
            graphics: self.console.graphics,
            vnc_listen: "127.0.0.1".to_string(),
            vnc_port: None,
            serial_log,
        })
    }
}

pub fn run(args: WebArgs) -> Result<()> {
    clear_error_callback();
    init_tracing();
    let api_token = load_api_token(&args)?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build Tokio runtime")?;

    runtime.block_on(run_async(args, api_token))
}

async fn run_async(args: WebArgs, api_token: String) -> Result<()> {
    let listen = args.listen;
    if !listen.ip().is_loopback() {
        let warning = format!(
            "qtr web uses unencrypted HTTP at {listen}; use a trusted network or a TLS reverse proxy"
        );
        eprintln!("[qtr] WARNING: {warning}");
        tracing::warn!(warning);
    }
    let jobs = JobService::start(JobRoots {
        state: args.state_dir,
        images: args.image_root,
        media: args.media_root,
        logs: args.log_root,
        connect_uri: args.connect_uri.clone(),
    })?;
    let app = app_with_iso_limit(
        args.connect_uri,
        args.web_dir,
        api_token,
        Some(jobs),
        args.max_iso_upload_bytes,
    );
    let listener = TcpListener::bind(listen)
        .await
        .with_context(|| format!("failed to bind web server at {listen}"))?;

    tracing::info!(%listen, "serving qtr web UI");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("web server failed")
}

fn load_api_token(args: &WebArgs) -> Result<String> {
    let token = match (&args.api_token, &args.api_token_file) {
        (Some(token), None) => token.clone(),
        (None, Some(path)) => std::fs::read_to_string(path)
            .with_context(|| format!("failed to read API token file {}", path.display()))?,
        _ => anyhow::bail!("configure exactly one of --api-token or --api-token-file"),
    };
    let token = token.trim();
    if token.is_empty() {
        anyhow::bail!("API token must not be empty");
    }
    if token.chars().any(char::is_whitespace) {
        anyhow::bail!("API token must not contain whitespace");
    }
    Ok(token.to_string())
}

#[cfg(test)]
fn app(
    connect_uri: String,
    web_dir: PathBuf,
    api_token: String,
    jobs: Option<JobService>,
) -> Router {
    app_with_iso_limit(connect_uri, web_dir, api_token, jobs, 34_359_738_368)
}

fn app_with_iso_limit(
    connect_uri: String,
    web_dir: PathBuf,
    api_token: String,
    jobs: Option<JobService>,
    max_iso_upload_bytes: u64,
) -> Router {
    let state = AppState {
        connect_uri,
        api_token: api_token.into(),
        vnc_tickets: Arc::new(Mutex::new(HashMap::new())),
        jobs,
        max_iso_upload_bytes,
        iso_uploads: Arc::new(Semaphore::new(1)),
    };
    let index_html = web_dir.join("index.html");
    let (api_router, openapi) = documented_api(&state);

    Router::new()
        .merge(api_router)
        .merge(SwaggerUi::new("/docs").url("/api/v1/openapi.json", openapi))
        .route("/api", any(api_not_found))
        .route("/api/{*path}", any(api_not_found))
        .fallback_service(
            ServeDir::new(web_dir)
                .append_index_html_on_directories(true)
                .fallback(ServeFile::new(index_html)),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

fn documented_api(state: &AppState) -> (Router<AppState>, utoipa::openapi::OpenApi) {
    let protected_api = OpenApiRouter::new()
        .routes(routes!(list_vms, create_vm))
        .routes(routes!(get_vm, update_vm, undefine_vm))
        .routes(routes!(start_vm))
        .routes(routes!(shutdown_vm))
        .routes(routes!(destroy_vm))
        .routes(routes!(create_vnc_ticket))
        .routes(routes!(session))
        .routes(routes!(list_install_jobs, create_install_job))
        .routes(routes!(get_install_job))
        .routes(routes!(cancel_install_job))
        .routes(routes!(list_images, create_image))
        .routes(routes!(resize_image, delete_image))
        .routes(routes!(attach_image, detach_image))
        .routes(routes!(add_cdrom_tray))
        .routes(routes!(set_cdrom_media, eject_cdrom_media))
        .routes(routes!(remove_cdrom_tray))
        .routes(routes!(list_media))
        .routes(routes!(upload_iso, delete_iso))
        .routes(routes!(list_networks))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_bearer_token,
        ));
    let api = OpenApiRouter::new()
        .routes(routes!(health))
        .routes(routes!(vnc_ws))
        .merge(protected_api);
    let mut documented = OpenApiRouter::with_openapi(ApiDoc::openapi()).nest("/api/v1", api);
    documented
        .get_openapi_mut()
        .components
        .get_or_insert_default()
        .add_security_scheme(
            "bearerAuth",
            SecurityScheme::Http(HttpBuilder::new().scheme(HttpAuthScheme::Bearer).build()),
        );
    documented.split_for_parts()
}

pub fn openapi_document() -> utoipa::openapi::OpenApi {
    let state = AppState {
        connect_uri: String::new(),
        api_token: Arc::from("unused"),
        vnc_tickets: Arc::new(Mutex::new(HashMap::new())),
        jobs: None,
        max_iso_upload_bytes: 34_359_738_368,
        iso_uploads: Arc::new(Semaphore::new(1)),
    };
    documented_api(&state).1
}

#[utoipa::path(
    get,
    path = "/health",
    tag = "system",
    responses((status = OK, body = HealthStatus))
)]
async fn health(State(state): State<AppState>) -> Json<HealthStatus> {
    let connect_uri = state.connect_uri.clone();
    let ok = run_libvirt(move || vm::list_summaries(&connect_uri))
        .await
        .is_ok();

    Json(HealthStatus {
        ok,
        libvirt_uri: state.connect_uri,
        version: env!("CARGO_PKG_VERSION"),
    })
}

#[utoipa::path(
    get,
    path = "/session",
    tag = "system",
    security(("bearerAuth" = [])),
    responses(
        (status = NO_CONTENT),
        (status = UNAUTHORIZED, body = ProblemDetails, content_type = "application/problem+json")
    )
)]
async fn session() -> StatusCode {
    StatusCode::NO_CONTENT
}

#[utoipa::path(
    get,
    path = "/vms",
    tag = "vms",
    security(("bearerAuth" = [])),
    responses(
        (status = OK, body = [vm::VmSummary]),
        (status = UNAUTHORIZED, body = ProblemDetails, content_type = "application/problem+json"),
        (status = INTERNAL_SERVER_ERROR, body = ProblemDetails, content_type = "application/problem+json")
    )
)]
async fn list_vms(State(state): State<AppState>) -> AppResult<Json<Vec<vm::VmSummary>>> {
    let jobs = job_service(&state)?;
    let connect_uri = state.connect_uri;
    let vms = run_libvirt(move || {
        vm::list_summaries(&connect_uri)?
            .into_iter()
            .map(|summary| managed_vm_summary(&jobs, summary))
            .collect::<Result<Vec<_>>>()
    })
    .await?;
    Ok(Json(vms))
}

#[utoipa::path(
    get,
    path = "/networks",
    tag = "resources",
    security(("bearerAuth" = [])),
    responses(
        (status = OK, body = [network::NetworkSummary]),
        (status = UNAUTHORIZED, body = ProblemDetails, content_type = "application/problem+json"),
        (status = INTERNAL_SERVER_ERROR, body = ProblemDetails, content_type = "application/problem+json")
    )
)]
async fn list_networks(
    State(state): State<AppState>,
) -> AppResult<Json<Vec<network::NetworkSummary>>> {
    let connect_uri = state.connect_uri;
    let networks = run_libvirt(move || network::list_summaries(&connect_uri)).await?;
    Ok(Json(networks))
}

#[utoipa::path(
    get,
    path = "/vms/{name}",
    tag = "vms",
    security(("bearerAuth" = [])),
    params(("name" = String, Path, description = "VM name")),
    responses(
        (status = OK, body = vm::VmSummary),
        (status = NOT_FOUND, body = ProblemDetails, content_type = "application/problem+json"),
        (status = UNAUTHORIZED, body = ProblemDetails, content_type = "application/problem+json")
    )
)]
async fn get_vm(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> AppResult<Json<vm::VmSummary>> {
    let jobs = job_service(&state)?;
    let connect_uri = state.connect_uri;
    let vm = run_libvirt(move || {
        let summary = vm::get_summary(&connect_uri, &name).map_err(anyhow::Error::new)?;
        managed_vm_summary(&jobs, summary)
    })
    .await?;
    Ok(Json(vm))
}

#[utoipa::path(
    post,
    path = "/vms",
    tag = "vms",
    security(("bearerAuth" = [])),
    request_body = CreateVmRequest,
    responses(
        (status = CREATED, body = vm::VmSummary),
        (status = BAD_REQUEST, body = ProblemDetails, content_type = "application/problem+json"),
        (status = UNAUTHORIZED, body = ProblemDetails, content_type = "application/problem+json"),
        (status = CONFLICT, body = ProblemDetails, content_type = "application/problem+json")
    )
)]
async fn create_vm(
    State(state): State<AppState>,
    request: std::result::Result<Json<CreateVmRequest>, JsonRejection>,
) -> AppResult<(StatusCode, Json<vm::VmSummary>)> {
    let request = api_json(request)?;
    let jobs = job_service(&state)?;
    let connect_uri = state.connect_uri;
    let vm = run_libvirt(move || {
        jobs.with_resource_lock(|| {
            let vm_name = request.name.clone();
            let image_ids = request
                .disks
                .iter()
                .map(|disk| disk.image_id.clone())
                .collect::<Vec<_>>();
            if let Some(job_id) = jobs.active_install_user(&vm_name, &image_ids)? {
                return Err(vm::VmApiError::Conflict(anyhow::anyhow!(
                    "VM name or image is reserved by automated install job {job_id}"
                )));
            }
            let manifest = request
                .into_manifest(&jobs)
                .map_err(vm::VmApiError::InvalidRequest)?;
            let network_id = manifest.network.clone().unwrap_or_default();
            network::ensure_active(&connect_uri, &network_id)
                .map_err(vm::VmApiError::InvalidRequest)?;
            let attachments = vm::managed_image_attachments(&connect_uri, jobs.image_root())
                .map_err(vm::VmApiError::Internal)?;
            for attachment in attachments {
                if image_ids.contains(&attachment.image_id) {
                    return Err(vm::VmApiError::Conflict(anyhow::anyhow!(
                        "image {} is already attached to VM {}",
                        attachment.image_id,
                        attachment.vm_name
                    )));
                }
            }
            let summary = vm::create_by_manifest(&connect_uri, manifest)?;
            managed_vm_summary(&jobs, summary).map_err(vm::VmApiError::Internal)
        })
    })
    .await?;
    Ok((StatusCode::CREATED, Json(vm)))
}

#[utoipa::path(
    put,
    path = "/vms/{name}",
    tag = "vms",
    security(("bearerAuth" = [])),
    params(("name" = String, Path, description = "VM name")),
    request_body = UpdateVmRequest,
    responses(
        (status = OK, body = vm::VmSummary),
        (status = BAD_REQUEST, body = ProblemDetails, content_type = "application/problem+json"),
        (status = NOT_FOUND, body = ProblemDetails, content_type = "application/problem+json"),
        (status = UNAUTHORIZED, body = ProblemDetails, content_type = "application/problem+json")
    )
)]
async fn update_vm(
    State(state): State<AppState>,
    Path(name): Path<String>,
    request: std::result::Result<Json<UpdateVmRequest>, JsonRejection>,
) -> AppResult<Json<vm::VmSummary>> {
    let request = api_json(request)?;
    let mut manifest = request.into_manifest();
    manifest.name = name;
    let jobs = job_service(&state)?;
    let connect_uri = state.connect_uri;
    let vm_name = manifest.name.clone();
    let vm = run_libvirt(move || {
        jobs.with_resource_lock(|| {
            let image_ids = jobs.managed_image_ids(
                manifest
                    .disks
                    .iter()
                    .filter_map(vm::VmDiskEntry::as_present)
                    .map(|disk| disk.path.as_path()),
            )?;
            reject_active_install(&jobs, &vm_name, &image_ids)?;
            for attachment in vm::managed_image_attachments(&connect_uri, jobs.image_root())? {
                if attachment.vm_name != vm_name && image_ids.contains(&attachment.image_id) {
                    return Err(vm::VmApiError::Conflict(anyhow::anyhow!(
                        "image {} is already attached to VM {}",
                        attachment.image_id,
                        attachment.vm_name
                    )));
                }
            }
            let summary = vm::apply_by_manifest(&connect_uri, manifest)?;
            managed_vm_summary(&jobs, summary).map_err(vm::VmApiError::Internal)
        })
    })
    .await?;
    Ok(Json(vm))
}

#[utoipa::path(
    post,
    path = "/vms/{name}/start",
    tag = "vm lifecycle",
    security(("bearerAuth" = [])),
    params(("name" = String, Path, description = "VM name")),
    responses(
        (status = NO_CONTENT),
        (status = NOT_FOUND, body = ProblemDetails, content_type = "application/problem+json"),
        (status = UNAUTHORIZED, body = ProblemDetails, content_type = "application/problem+json"),
        (status = CONFLICT, body = ProblemDetails, content_type = "application/problem+json")
    )
)]
async fn start_vm(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> AppResult<StatusCode> {
    let jobs = job_service(&state)?;
    let connect_uri = state.connect_uri;
    run_libvirt(move || {
        jobs.with_resource_lock(|| {
            reject_active_install(&jobs, &name, &[])?;
            vm::start_by_name(&connect_uri, &name)
        })
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/vms/{name}/shutdown",
    tag = "vm lifecycle",
    security(("bearerAuth" = [])),
    params(("name" = String, Path, description = "VM name")),
    responses(
        (status = NO_CONTENT),
        (status = NOT_FOUND, body = ProblemDetails, content_type = "application/problem+json"),
        (status = UNAUTHORIZED, body = ProblemDetails, content_type = "application/problem+json"),
        (status = CONFLICT, body = ProblemDetails, content_type = "application/problem+json")
    )
)]
async fn shutdown_vm(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> AppResult<StatusCode> {
    let jobs = job_service(&state)?;
    let connect_uri = state.connect_uri;
    run_libvirt(move || {
        jobs.with_resource_lock(|| {
            reject_active_install(&jobs, &name, &[])?;
            vm::shutdown_by_name(&connect_uri, &name, false)
        })
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/vms/{name}/destroy",
    tag = "vm lifecycle",
    security(("bearerAuth" = [])),
    params(("name" = String, Path, description = "VM name")),
    responses(
        (status = NO_CONTENT),
        (status = NOT_FOUND, body = ProblemDetails, content_type = "application/problem+json"),
        (status = UNAUTHORIZED, body = ProblemDetails, content_type = "application/problem+json"),
        (status = CONFLICT, body = ProblemDetails, content_type = "application/problem+json")
    )
)]
async fn destroy_vm(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> AppResult<StatusCode> {
    let jobs = job_service(&state)?;
    let connect_uri = state.connect_uri;
    run_libvirt(move || {
        jobs.with_resource_lock(|| {
            reject_active_install(&jobs, &name, &[])?;
            vm::destroy_by_name(&connect_uri, &name)
        })
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    delete,
    path = "/vms/{name}",
    tag = "vms",
    security(("bearerAuth" = [])),
    params(("name" = String, Path, description = "VM name")),
    responses(
        (status = NO_CONTENT),
        (status = NOT_FOUND, body = ProblemDetails, content_type = "application/problem+json"),
        (status = UNAUTHORIZED, body = ProblemDetails, content_type = "application/problem+json"),
        (status = CONFLICT, body = ProblemDetails, content_type = "application/problem+json")
    )
)]
async fn undefine_vm(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> AppResult<StatusCode> {
    let jobs = job_service(&state)?;
    let connect_uri = state.connect_uri;
    run_libvirt(move || {
        jobs.with_resource_lock(|| {
            reject_active_install(&jobs, &name, &[])?;
            vm::undefine_by_name(&connect_uri, &name)
        })
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/install-jobs",
    tag = "install jobs",
    security(("bearerAuth" = [])),
    responses(
        (status = OK, body = [InstallJob]),
        (status = UNAUTHORIZED, body = ProblemDetails, content_type = "application/problem+json"),
        (status = INTERNAL_SERVER_ERROR, body = ProblemDetails, content_type = "application/problem+json")
    )
)]
async fn list_install_jobs(State(state): State<AppState>) -> AppResult<Json<Vec<InstallJob>>> {
    let jobs = job_service(&state)?;
    Ok(Json(run_job_store(move || jobs.list()).await?))
}

#[utoipa::path(
    post,
    path = "/install-jobs",
    tag = "install jobs",
    security(("bearerAuth" = [])),
    request_body = FedoraInstallRequest,
    responses(
        (status = ACCEPTED, body = InstallJob),
        (status = BAD_REQUEST, body = ProblemDetails, content_type = "application/problem+json"),
        (status = UNAUTHORIZED, body = ProblemDetails, content_type = "application/problem+json"),
        (status = CONFLICT, body = ProblemDetails, content_type = "application/problem+json"),
        (status = INTERNAL_SERVER_ERROR, body = ProblemDetails, content_type = "application/problem+json")
    )
)]
async fn create_install_job(
    State(state): State<AppState>,
    request: std::result::Result<Json<FedoraInstallRequest>, JsonRejection>,
) -> AppResult<(StatusCode, Json<InstallJob>)> {
    let request = api_json(request)?;
    request.validate().map_err(AppError::BadRequest)?;
    let jobs = job_service(&state)?;
    let connect_uri = state.connect_uri;
    let outcome = run_job_store(move || {
        jobs.create(request, |name| {
            Ok(vm::list_summaries(&connect_uri)?
                .into_iter()
                .any(|vm| vm.name == name))
        })
    })
    .await?;
    match outcome {
        InstallJobCreateOutcome::Created(job) => Ok((StatusCode::ACCEPTED, Json(*job))),
        InstallJobCreateOutcome::Conflict(detail) => Err(AppError::Conflict(detail)),
    }
}

fn reject_active_install(
    jobs: &JobService,
    vm_name: &str,
    image_ids: &[String],
) -> vm::VmApiResult<()> {
    if let Some(job_id) = jobs.active_install_user(vm_name, image_ids)? {
        return Err(vm::VmApiError::Conflict(anyhow::anyhow!(
            "VM is reserved by automated install job {job_id}"
        )));
    }
    Ok(())
}

#[utoipa::path(
    get,
    path = "/install-jobs/{id}",
    tag = "install jobs",
    security(("bearerAuth" = [])),
    params(("id" = String, Path, description = "Job ID")),
    responses(
        (status = OK, body = InstallJob),
        (status = NOT_FOUND, body = ProblemDetails, content_type = "application/problem+json"),
        (status = UNAUTHORIZED, body = ProblemDetails, content_type = "application/problem+json")
    )
)]
async fn get_install_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<Json<InstallJob>> {
    let jobs = job_service(&state)?;
    let job = run_job_store(move || jobs.get(&id))
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(job))
}

#[utoipa::path(
    post,
    path = "/install-jobs/{id}/cancel",
    tag = "install jobs",
    security(("bearerAuth" = [])),
    params(("id" = String, Path, description = "Job ID")),
    responses(
        (status = ACCEPTED, body = InstallJob),
        (status = NOT_FOUND, body = ProblemDetails, content_type = "application/problem+json"),
        (status = UNAUTHORIZED, body = ProblemDetails, content_type = "application/problem+json")
    )
)]
async fn cancel_install_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<(StatusCode, Json<InstallJob>)> {
    let jobs = job_service(&state)?;
    let job = run_job_store(move || jobs.cancel(&id))
        .await?
        .ok_or(AppError::NotFound)?;
    Ok((StatusCode::ACCEPTED, Json(job)))
}

#[utoipa::path(
    get,
    path = "/images",
    tag = "resources",
    security(("bearerAuth" = [])),
    responses(
        (status = OK, body = [ManagedImageResponse]),
        (status = UNAUTHORIZED, body = ProblemDetails, content_type = "application/problem+json"),
        (status = INTERNAL_SERVER_ERROR, body = ProblemDetails, content_type = "application/problem+json")
    )
)]
async fn list_images(State(state): State<AppState>) -> AppResult<Json<Vec<ManagedImageResponse>>> {
    let jobs = job_service(&state)?;
    let inventory_jobs = jobs.clone();
    let images = run_job_store(move || {
        inventory_jobs
            .list_images()?
            .into_iter()
            .map(|image| {
                let reserved_by_job_id = inventory_jobs.active_image_user(&image.id)?;
                Ok((image, reserved_by_job_id))
            })
            .collect::<Result<Vec<_>>>()
    })
    .await?;
    let connect_uri = state.connect_uri;
    let image_root = jobs.image_root().to_path_buf();
    let attachments =
        run_libvirt(move || vm::managed_image_attachments(&connect_uri, &image_root)).await?;
    Ok(Json(
        images
            .into_iter()
            .map(|(image, reserved_by_job_id)| {
                let image_attachments = attachments
                    .iter()
                    .filter(|attachment| attachment.image_id == image.id)
                    .cloned()
                    .collect();
                ManagedImageResponse::new(image, image_attachments, reserved_by_job_id)
            })
            .collect(),
    ))
}

#[utoipa::path(
    post,
    path = "/images",
    tag = "resources",
    security(("bearerAuth" = [])),
    request_body = CreateImageRequest,
    responses(
        (status = CREATED, body = ManagedImageResponse),
        (status = BAD_REQUEST, body = ProblemDetails, content_type = "application/problem+json"),
        (status = UNAUTHORIZED, body = ProblemDetails, content_type = "application/problem+json"),
        (status = CONFLICT, body = ProblemDetails, content_type = "application/problem+json"),
        (status = INTERNAL_SERVER_ERROR, body = ProblemDetails, content_type = "application/problem+json")
    )
)]
async fn create_image(
    State(state): State<AppState>,
    request: std::result::Result<Json<CreateImageRequest>, JsonRejection>,
) -> AppResult<(StatusCode, Json<ManagedImageResponse>)> {
    let request = api_json(request)?;
    let jobs = job_service(&state)?;
    jobs.validate_image_id(&request.id, request.format)
        .map_err(AppError::BadRequest)?;
    jobs.validate_image_size(request.size_bytes)
        .map_err(AppError::BadRequest)?;
    let outcome =
        run_job_store(move || jobs.create_image(&request.id, request.format, request.size_bytes))
            .await?;
    match outcome {
        ImageCreateOutcome::Created(image) => Ok((
            StatusCode::CREATED,
            Json(ManagedImageResponse::new(image, Vec::new(), None)),
        )),
        ImageCreateOutcome::Conflict(detail) => Err(AppError::Conflict(detail)),
    }
}

#[utoipa::path(
    post,
    path = "/images/{id}/resize",
    tag = "resources",
    security(("bearerAuth" = [])),
    params(("id" = String, Path, description = "Managed image ID")),
    request_body = ResizeImageRequest,
    responses(
        (status = OK, body = ManagedImageResponse),
        (status = BAD_REQUEST, body = ProblemDetails, content_type = "application/problem+json"),
        (status = NOT_FOUND, body = ProblemDetails, content_type = "application/problem+json"),
        (status = UNAUTHORIZED, body = ProblemDetails, content_type = "application/problem+json"),
        (status = CONFLICT, body = ProblemDetails, content_type = "application/problem+json"),
        (status = INTERNAL_SERVER_ERROR, body = ProblemDetails, content_type = "application/problem+json")
    )
)]
async fn resize_image(
    State(state): State<AppState>,
    Path(id): Path<String>,
    request: std::result::Result<Json<ResizeImageRequest>, JsonRejection>,
) -> AppResult<Json<ManagedImageResponse>> {
    let request = api_json(request)?;
    let jobs = job_service(&state)?;
    jobs.validate_existing_image_id(&id)
        .map_err(AppError::BadRequest)?;
    jobs.validate_image_size(request.size_bytes)
        .map_err(AppError::BadRequest)?;
    let connect_uri = state.connect_uri;
    let resize_jobs = jobs.clone();
    let outcome = run_job_store(move || {
        resize_jobs.resize_image(&id, request.size_bytes, |path| {
            vm::active_domain_using_image(&connect_uri, path)
        })
    })
    .await?;
    match outcome {
        ImageResizeOutcome::Resized(image) => {
            Ok(Json(ManagedImageResponse::new(image, Vec::new(), None)))
        }
        ImageResizeOutcome::NotFound => Err(AppError::NotFound),
        ImageResizeOutcome::InUse(detail) | ImageResizeOutcome::Conflict(detail) => {
            Err(AppError::Conflict(detail))
        }
    }
}

#[utoipa::path(
    delete,
    path = "/images/{id}",
    tag = "resources",
    security(("bearerAuth" = [])),
    params(("id" = String, Path, description = "Managed image ID")),
    responses(
        (status = NO_CONTENT),
        (status = BAD_REQUEST, body = ProblemDetails, content_type = "application/problem+json"),
        (status = NOT_FOUND, body = ProblemDetails, content_type = "application/problem+json"),
        (status = UNAUTHORIZED, body = ProblemDetails, content_type = "application/problem+json"),
        (status = CONFLICT, body = ProblemDetails, content_type = "application/problem+json"),
        (status = INTERNAL_SERVER_ERROR, body = ProblemDetails, content_type = "application/problem+json")
    )
)]
async fn delete_image(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    let jobs = job_service(&state)?;
    jobs.validate_existing_image_id(&id)
        .map_err(AppError::BadRequest)?;
    let connect_uri = state.connect_uri;
    let delete_jobs = jobs.clone();
    let outcome = run_job_store(move || {
        delete_jobs.delete_image(&id, |path| vm::domains_using_image(&connect_uri, path))
    })
    .await?;
    match outcome {
        ImageDeleteOutcome::Deleted => Ok(StatusCode::NO_CONTENT),
        ImageDeleteOutcome::NotFound => Err(AppError::NotFound),
        ImageDeleteOutcome::InUse(detail) => Err(AppError::Conflict(detail)),
    }
}

#[utoipa::path(
    put,
    path = "/vms/{name}/disks/{image_id}",
    tag = "vms",
    security(("bearerAuth" = [])),
    params(
        ("name" = String, Path, description = "VM name"),
        ("image_id" = String, Path, description = "Managed image ID")
    ),
    request_body = AttachImageRequest,
    responses(
        (status = OK, body = vm::VmSummary),
        (status = BAD_REQUEST, body = ProblemDetails, content_type = "application/problem+json"),
        (status = NOT_FOUND, body = ProblemDetails, content_type = "application/problem+json"),
        (status = UNAUTHORIZED, body = ProblemDetails, content_type = "application/problem+json"),
        (status = CONFLICT, body = ProblemDetails, content_type = "application/problem+json")
    )
)]
async fn attach_image(
    State(state): State<AppState>,
    Path((name, image_id)): Path<(String, String)>,
    request: std::result::Result<Json<AttachImageRequest>, JsonRejection>,
) -> AppResult<Json<vm::VmSummary>> {
    let request = api_json(request)?;
    let jobs = job_service(&state)?;
    jobs.validate_existing_image_id(&image_id)
        .map_err(AppError::BadRequest)?;
    let connect_uri = state.connect_uri;
    let summary = run_libvirt(move || {
        jobs.with_resource_lock(|| {
            if let Some(job_id) = jobs.active_image_user(&image_id)? {
                return Err(vm::VmApiError::Conflict(anyhow::anyhow!(
                    "image is reserved by automated install job {job_id}"
                )));
            }
            let image = jobs
                .inspect_image(&image_id)
                .map_err(vm::VmApiError::InvalidRequest)?;
            let format = image.format.ok_or_else(|| {
                vm::VmApiError::InvalidRequest(anyhow::anyhow!(
                    "image {image_id:?} is not a valid disk"
                ))
            })?;
            for attachment in vm::managed_image_attachments(&connect_uri, jobs.image_root())? {
                if attachment.image_id == image_id && attachment.vm_name != name {
                    return Err(vm::VmApiError::Conflict(anyhow::anyhow!(
                        "image is already attached to VM {}",
                        attachment.vm_name
                    )));
                }
            }
            let path = jobs
                .resolve_image(&image_id)
                .map_err(vm::VmApiError::InvalidRequest)?;
            let summary =
                vm::attach_managed_image(&connect_uri, &name, &path, format, request.bus)?;
            managed_vm_summary(&jobs, summary).map_err(vm::VmApiError::Internal)
        })
    })
    .await?;
    Ok(Json(summary))
}

#[utoipa::path(
    delete,
    path = "/vms/{name}/disks/{image_id}",
    tag = "vms",
    security(("bearerAuth" = [])),
    params(
        ("name" = String, Path, description = "VM name"),
        ("image_id" = String, Path, description = "Managed image ID")
    ),
    responses(
        (status = OK, body = vm::VmSummary),
        (status = BAD_REQUEST, body = ProblemDetails, content_type = "application/problem+json"),
        (status = NOT_FOUND, body = ProblemDetails, content_type = "application/problem+json"),
        (status = UNAUTHORIZED, body = ProblemDetails, content_type = "application/problem+json"),
        (status = CONFLICT, body = ProblemDetails, content_type = "application/problem+json")
    )
)]
async fn detach_image(
    State(state): State<AppState>,
    Path((name, image_id)): Path<(String, String)>,
) -> AppResult<Json<vm::VmSummary>> {
    let jobs = job_service(&state)?;
    jobs.validate_existing_image_id(&image_id)
        .map_err(AppError::BadRequest)?;
    let connect_uri = state.connect_uri;
    let summary = run_libvirt(move || {
        jobs.with_resource_lock(|| {
            if let Some(job_id) = jobs.active_image_user(&image_id)? {
                return Err(vm::VmApiError::Conflict(anyhow::anyhow!(
                    "image is reserved by automated install job {job_id}"
                )));
            }
            let path = jobs
                .resolve_image(&image_id)
                .map_err(vm::VmApiError::InvalidRequest)?;
            let summary = vm::detach_managed_image(&connect_uri, &name, &path)?;
            managed_vm_summary(&jobs, summary).map_err(vm::VmApiError::Internal)
        })
    })
    .await?;
    Ok(Json(summary))
}

#[utoipa::path(
    post,
    path = "/vms/{name}/cdroms",
    tag = "vms",
    security(("bearerAuth" = [])),
    params(("name" = String, Path, description = "VM name")),
    request_body = AddCdromTrayRequest,
    responses(
        (status = CREATED, body = vm::VmSummary),
        (status = BAD_REQUEST, body = ProblemDetails, content_type = "application/problem+json"),
        (status = NOT_FOUND, body = ProblemDetails, content_type = "application/problem+json"),
        (status = UNAUTHORIZED, body = ProblemDetails, content_type = "application/problem+json"),
        (status = CONFLICT, body = ProblemDetails, content_type = "application/problem+json"),
        (status = INTERNAL_SERVER_ERROR, body = ProblemDetails, content_type = "application/problem+json")
    )
)]
async fn add_cdrom_tray(
    State(state): State<AppState>,
    Path(name): Path<String>,
    request: std::result::Result<Json<AddCdromTrayRequest>, JsonRejection>,
) -> AppResult<(StatusCode, Json<vm::VmSummary>)> {
    let request = api_json(request)?;
    let jobs = job_service(&state)?;
    if let Some(media_id) = request.media_id.as_deref() {
        jobs.validate_iso_id(media_id)
            .map_err(AppError::BadRequest)?;
    }
    let connect_uri = state.connect_uri;
    let summary = run_libvirt(move || {
        jobs.with_resource_lock(|| {
            let media = request
                .media_id
                .as_deref()
                .map(|id| managed_iso_path(&jobs, id))
                .transpose()?;
            let summary =
                vm::add_managed_cdrom(&connect_uri, &name, &request.id, media.as_deref())?;
            managed_vm_summary(&jobs, summary).map_err(vm::VmApiError::Internal)
        })
    })
    .await?;
    Ok((StatusCode::CREATED, Json(summary)))
}

#[utoipa::path(
    put,
    path = "/vms/{name}/cdroms/{tray_id}/media",
    tag = "vms",
    security(("bearerAuth" = [])),
    params(
        ("name" = String, Path, description = "VM name"),
        ("tray_id" = String, Path, description = "Stable CD-ROM tray ID")
    ),
    request_body = SetCdromMediaRequest,
    responses(
        (status = OK, body = vm::VmSummary),
        (status = BAD_REQUEST, body = ProblemDetails, content_type = "application/problem+json"),
        (status = NOT_FOUND, body = ProblemDetails, content_type = "application/problem+json"),
        (status = UNAUTHORIZED, body = ProblemDetails, content_type = "application/problem+json"),
        (status = CONFLICT, body = ProblemDetails, content_type = "application/problem+json"),
        (status = INTERNAL_SERVER_ERROR, body = ProblemDetails, content_type = "application/problem+json")
    )
)]
async fn set_cdrom_media(
    State(state): State<AppState>,
    Path((name, tray_id)): Path<(String, String)>,
    request: std::result::Result<Json<SetCdromMediaRequest>, JsonRejection>,
) -> AppResult<Json<vm::VmSummary>> {
    let request = api_json(request)?;
    let jobs = job_service(&state)?;
    jobs.validate_iso_id(&request.media_id)
        .map_err(AppError::BadRequest)?;
    let connect_uri = state.connect_uri;
    let summary = run_libvirt(move || {
        jobs.with_resource_lock(|| {
            let media = managed_iso_path(&jobs, &request.media_id)?;
            let summary = vm::set_managed_cdrom_media(&connect_uri, &name, &tray_id, Some(&media))?;
            managed_vm_summary(&jobs, summary).map_err(vm::VmApiError::Internal)
        })
    })
    .await?;
    Ok(Json(summary))
}

#[utoipa::path(
    delete,
    path = "/vms/{name}/cdroms/{tray_id}/media",
    tag = "vms",
    security(("bearerAuth" = [])),
    params(
        ("name" = String, Path, description = "VM name"),
        ("tray_id" = String, Path, description = "Stable CD-ROM tray ID")
    ),
    responses(
        (status = OK, body = vm::VmSummary),
        (status = BAD_REQUEST, body = ProblemDetails, content_type = "application/problem+json"),
        (status = NOT_FOUND, body = ProblemDetails, content_type = "application/problem+json"),
        (status = UNAUTHORIZED, body = ProblemDetails, content_type = "application/problem+json"),
        (status = CONFLICT, body = ProblemDetails, content_type = "application/problem+json"),
        (status = INTERNAL_SERVER_ERROR, body = ProblemDetails, content_type = "application/problem+json")
    )
)]
async fn eject_cdrom_media(
    State(state): State<AppState>,
    Path((name, tray_id)): Path<(String, String)>,
) -> AppResult<Json<vm::VmSummary>> {
    let jobs = job_service(&state)?;
    let connect_uri = state.connect_uri;
    let summary = run_libvirt(move || {
        jobs.with_resource_lock(|| {
            let summary = vm::set_managed_cdrom_media(&connect_uri, &name, &tray_id, None)?;
            managed_vm_summary(&jobs, summary).map_err(vm::VmApiError::Internal)
        })
    })
    .await?;
    Ok(Json(summary))
}

#[utoipa::path(
    delete,
    path = "/vms/{name}/cdroms/{tray_id}",
    tag = "vms",
    security(("bearerAuth" = [])),
    params(
        ("name" = String, Path, description = "VM name"),
        ("tray_id" = String, Path, description = "Stable CD-ROM tray ID")
    ),
    responses(
        (status = OK, body = vm::VmSummary),
        (status = BAD_REQUEST, body = ProblemDetails, content_type = "application/problem+json"),
        (status = NOT_FOUND, body = ProblemDetails, content_type = "application/problem+json"),
        (status = UNAUTHORIZED, body = ProblemDetails, content_type = "application/problem+json"),
        (status = CONFLICT, body = ProblemDetails, content_type = "application/problem+json"),
        (status = INTERNAL_SERVER_ERROR, body = ProblemDetails, content_type = "application/problem+json")
    )
)]
async fn remove_cdrom_tray(
    State(state): State<AppState>,
    Path((name, tray_id)): Path<(String, String)>,
) -> AppResult<Json<vm::VmSummary>> {
    let jobs = job_service(&state)?;
    let connect_uri = state.connect_uri;
    let summary = run_libvirt(move || {
        jobs.with_resource_lock(|| {
            let summary = vm::remove_managed_cdrom(&connect_uri, &name, &tray_id)?;
            managed_vm_summary(&jobs, summary).map_err(vm::VmApiError::Internal)
        })
    })
    .await?;
    Ok(Json(summary))
}

#[utoipa::path(
    get,
    path = "/media",
    tag = "resources",
    security(("bearerAuth" = [])),
    responses(
        (status = OK, body = [ManagedIsoResponse]),
        (status = UNAUTHORIZED, body = ProblemDetails, content_type = "application/problem+json"),
        (status = INTERNAL_SERVER_ERROR, body = ProblemDetails, content_type = "application/problem+json")
    )
)]
async fn list_media(State(state): State<AppState>) -> AppResult<Json<Vec<ManagedIsoResponse>>> {
    let jobs = job_service(&state)?;
    let inventory_jobs = jobs.clone();
    let isos = run_job_store(move || {
        inventory_jobs
            .list_media()?
            .into_iter()
            .map(|iso| {
                let reservations = inventory_jobs.active_media_users(&iso.id)?;
                Ok((iso, reservations))
            })
            .collect::<Result<Vec<_>>>()
    })
    .await?;
    let connect_uri = state.connect_uri;
    let media_root = jobs.media_root().to_path_buf();
    let attachments =
        run_libvirt(move || vm::managed_iso_attachments(&connect_uri, &media_root)).await?;
    Ok(Json(
        isos.into_iter()
            .map(|(iso, reservations)| {
                let iso_attachments = attachments
                    .iter()
                    .filter(|attachment| attachment.media_id == iso.id)
                    .cloned()
                    .collect();
                ManagedIsoResponse::new(iso, iso_attachments, reservations)
            })
            .collect(),
    ))
}

struct PartialUpload(PathBuf);

impl Drop for PartialUpload {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[utoipa::path(
    put,
    path = "/media/{id}",
    tag = "resources",
    security(("bearerAuth" = [])),
    params(("id" = String, Path, description = "ISO ID")),
    request_body(content = Vec<u8>, content_type = "application/octet-stream"),
    responses(
        (status = CREATED, body = ManagedIsoResponse),
        (status = BAD_REQUEST, body = ProblemDetails, content_type = "application/problem+json"),
        (status = UNAUTHORIZED, body = ProblemDetails, content_type = "application/problem+json"),
        (status = CONFLICT, body = ProblemDetails, content_type = "application/problem+json"),
        (status = PAYLOAD_TOO_LARGE, body = ProblemDetails, content_type = "application/problem+json"),
        (status = UNSUPPORTED_MEDIA_TYPE, body = ProblemDetails, content_type = "application/problem+json")
    )
)]
async fn upload_iso(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> AppResult<(StatusCode, Json<ManagedIsoResponse>)> {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if content_type != "application/octet-stream" {
        return Err(AppError::UnsupportedMediaType);
    }
    if headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > state.max_iso_upload_bytes)
    {
        return Err(AppError::PayloadTooLarge);
    }

    let jobs = job_service(&state)?;
    jobs.validate_iso_id(&id).map_err(AppError::BadRequest)?;
    let _permit = state
        .iso_uploads
        .clone()
        .acquire_owned()
        .await
        .map_err(|error| AppError::Internal(error.into()))?;
    let staging = jobs.create_iso_staging_path().map_err(AppError::Internal)?;
    let partial = PartialUpload(staging.clone());
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .read(true)
        .open(&staging)
        .await
        .map_err(|error| AppError::Internal(error.into()))?;
    let mut stream = body.into_data_stream();
    let mut written = 0_u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| AppError::BadRequest(error.into()))?;
        written = written
            .checked_add(chunk.len() as u64)
            .ok_or(AppError::PayloadTooLarge)?;
        if written > state.max_iso_upload_bytes {
            return Err(AppError::PayloadTooLarge);
        }
        file.write_all(&chunk)
            .await
            .map_err(|error| AppError::Internal(error.into()))?;
    }
    if written == 0 {
        return Err(AppError::BadRequest(anyhow::anyhow!(
            "ISO upload must not be empty"
        )));
    }
    file.flush()
        .await
        .map_err(|error| AppError::Internal(error.into()))?;
    file.sync_all()
        .await
        .map_err(|error| AppError::Internal(error.into()))?;
    file.seek(std::io::SeekFrom::Start(32_769))
        .await
        .map_err(|error| AppError::BadRequest(error.into()))?;
    let mut signature = [0_u8; 5];
    file.read_exact(&mut signature)
        .await
        .map_err(|_| AppError::BadRequest(anyhow::anyhow!("file is not a valid ISO9660 image")))?;
    if &signature != b"CD001" {
        return Err(AppError::BadRequest(anyhow::anyhow!(
            "file is not a valid ISO9660 image"
        )));
    }
    drop(file);

    let publish_jobs = jobs.clone();
    let publish_id = id.clone();
    let outcome = run_job_store(move || publish_jobs.publish_iso(&publish_id, &staging)).await?;
    let resource = match outcome {
        IsoPublishOutcome::Created(resource) => resource,
        IsoPublishOutcome::Exists => {
            return Err(AppError::Conflict(format!("ISO {id:?} already exists")));
        }
    };
    drop(partial);
    Ok((
        StatusCode::CREATED,
        Json(ManagedIsoResponse::new(resource, Vec::new(), Vec::new())),
    ))
}

#[utoipa::path(
    delete,
    path = "/media/{id}",
    tag = "resources",
    security(("bearerAuth" = [])),
    params(("id" = String, Path, description = "ISO ID")),
    responses(
        (status = NO_CONTENT),
        (status = BAD_REQUEST, body = ProblemDetails, content_type = "application/problem+json"),
        (status = NOT_FOUND, body = ProblemDetails, content_type = "application/problem+json"),
        (status = UNAUTHORIZED, body = ProblemDetails, content_type = "application/problem+json"),
        (status = CONFLICT, body = ProblemDetails, content_type = "application/problem+json")
    )
)]
async fn delete_iso(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    let jobs = job_service(&state)?;
    jobs.validate_iso_id(&id).map_err(AppError::BadRequest)?;
    let connect_uri = state.connect_uri;
    let delete_jobs = jobs.clone();
    let outcome = run_job_store(move || {
        delete_jobs.delete_iso(&id, |path| vm::domains_using_media(&connect_uri, path))
    })
    .await?;
    match outcome {
        IsoDeleteOutcome::Deleted => Ok(StatusCode::NO_CONTENT),
        IsoDeleteOutcome::NotFound => Err(AppError::NotFound),
        IsoDeleteOutcome::InUse(detail) => Err(AppError::Conflict(detail)),
    }
}

fn job_service(state: &AppState) -> AppResult<JobService> {
    state
        .jobs
        .clone()
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("install job service is not configured")))
}

fn managed_vm_summary(jobs: &JobService, mut summary: vm::VmSummary) -> Result<vm::VmSummary> {
    for cdrom in &mut summary.cdroms {
        let Some(source) = cdrom.source_path.as_deref() else {
            continue;
        };
        cdrom.media_id = jobs.managed_media_id(FsPath::new(source))?;
    }
    Ok(summary)
}

fn managed_iso_path(jobs: &JobService, id: &str) -> vm::VmApiResult<PathBuf> {
    let iso = jobs
        .inspect_iso(id)
        .map_err(|_| vm::VmApiError::NotFound(format!("managed ISO {id:?} was not found")))?;
    if iso.status != ManagedIsoStatus::Ready {
        return Err(vm::VmApiError::InvalidRequest(anyhow::anyhow!(
            "managed ISO {id:?} is not a valid ISO9660 image"
        )));
    }
    jobs.resolve_media(id).map_err(vm::VmApiError::Internal)
}

async fn run_job_store<T, F>(task: F) -> AppResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(task)
        .await
        .map_err(|error| {
            AppError::Internal(anyhow::Error::new(error).context("job store task panicked"))
        })?
        .map_err(AppError::Internal)
}

const VNC_TICKET_LIFETIME: Duration = Duration::from_secs(30);

#[utoipa::path(
    post,
    path = "/vms/{name}/vnc-ticket",
    tag = "vm console",
    security(("bearerAuth" = [])),
    params(("name" = String, Path, description = "VM name")),
    responses(
        (status = CREATED, body = VncTicketResponse),
        (status = NOT_FOUND, body = ProblemDetails, content_type = "application/problem+json"),
        (status = UNAUTHORIZED, body = ProblemDetails, content_type = "application/problem+json"),
        (status = CONFLICT, body = ProblemDetails, content_type = "application/problem+json")
    )
)]
async fn create_vnc_ticket(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> AppResult<(StatusCode, Json<VncTicketResponse>)> {
    let connect_uri = state.connect_uri.clone();
    let vm_name = name.clone();
    run_libvirt(move || vm::vnc_endpoint_by_name_api(&connect_uri, &vm_name)).await?;

    let ticket = Uuid::new_v4().to_string();
    let expires_at = Instant::now() + VNC_TICKET_LIFETIME;
    let mut tickets = state.vnc_tickets.lock().map_err(|error| {
        AppError::Internal(anyhow::anyhow!("VNC ticket store lock poisoned: {error}"))
    })?;
    tickets.retain(|_, ticket| ticket.expires_at > Instant::now());
    tickets.insert(
        ticket.clone(),
        VncTicket {
            vm_name: name,
            expires_at,
        },
    );

    Ok((
        StatusCode::CREATED,
        Json(VncTicketResponse {
            ticket,
            expires_in_seconds: VNC_TICKET_LIFETIME.as_secs(),
        }),
    ))
}

#[utoipa::path(
    get,
    path = "/vms/{name}/vnc",
    tag = "vm console",
    params(
        ("name" = String, Path, description = "VM name"),
        ("ticket" = String, Query, description = "One-time VNC ticket")
    ),
    responses(
        (status = 101, description = "WebSocket upgrade"),
        (status = UNAUTHORIZED, body = ProblemDetails, content_type = "application/problem+json"),
        (status = FORBIDDEN, body = ProblemDetails, content_type = "application/problem+json")
    )
)]
async fn vnc_ws(
    State(state): State<AppState>,
    Path(name): Path<String>,
    query: std::result::Result<Query<VncTicketQuery>, QueryRejection>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let Ok(Query(query)) = query else {
        return ProblemDetails::response(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "The VNC ticket query parameter is required.",
        );
    };
    if let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    {
        let host = headers
            .get(header::HOST)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if !origin_matches_host(origin, host) {
            tracing::warn!(%origin, %host, "rejected cross-origin VNC WebSocket");
            return ProblemDetails::response(
                StatusCode::FORBIDDEN,
                "Forbidden",
                "The WebSocket origin does not match the request host.",
            );
        }
    }
    if !consume_vnc_ticket(&state, &name, &query.ticket) {
        return ProblemDetails::response(
            StatusCode::UNAUTHORIZED,
            "Unauthorized",
            "A valid one-time VNC ticket is required.",
        );
    }

    let connect_uri = state.connect_uri;
    ws.on_upgrade(move |socket| async move {
        if let Err(error) = handle_vnc_upgrade(socket, connect_uri, name).await {
            tracing::debug!(%error, "VNC bridge closed with error");
        }
    })
    .into_response()
}

fn consume_vnc_ticket(state: &AppState, vm_name: &str, token: &str) -> bool {
    let Ok(mut tickets) = state.vnc_tickets.lock() else {
        tracing::error!("VNC ticket store lock poisoned");
        return false;
    };
    let Some(ticket) = tickets.get(token) else {
        return false;
    };
    if ticket.expires_at <= Instant::now() {
        tickets.remove(token);
        return false;
    }
    if ticket.vm_name != vm_name {
        return false;
    }
    tickets.remove(token);
    true
}

async fn require_bearer_token(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let supplied_token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| !value.is_empty());
    let authenticated = supplied_token
        .is_some_and(|token| state.api_token.as_bytes().ct_eq(token.as_bytes()).into());

    if authenticated {
        next.run(request).await
    } else {
        ProblemDetails::response(
            StatusCode::UNAUTHORIZED,
            "Unauthorized",
            "A valid bearer token is required.",
        )
    }
}

fn api_json<T>(request: std::result::Result<Json<T>, JsonRejection>) -> AppResult<T> {
    request
        .map(|Json(value)| value)
        .map_err(|error| vm::VmApiError::InvalidRequest(anyhow::anyhow!(error.body_text())).into())
}

async fn api_not_found() -> Response {
    ProblemDetails::response(
        StatusCode::NOT_FOUND,
        "Not Found",
        "The API endpoint was not found.",
    )
}

fn origin_matches_host(origin: &str, host: &str) -> bool {
    let origin_authority = origin
        .trim_end_matches('/')
        .split("://")
        .nth(1)
        .unwrap_or(origin);
    !host.is_empty() && origin_authority.eq_ignore_ascii_case(host)
}

async fn run_libvirt<T, E, F>(task: F) -> AppResult<T>
where
    T: Send + 'static,
    E: Into<AppError> + Send + 'static,
    F: FnOnce() -> std::result::Result<T, E> + Send + 'static,
{
    let result = tokio::task::spawn_blocking(task).await.map_err(|error| {
        AppError::Internal(anyhow::Error::new(error).context("libvirt task panicked"))
    })?;
    result.map_err(Into::into)
}

async fn handle_vnc_upgrade(
    mut socket: WebSocket,
    connect_uri: String,
    name: String,
) -> Result<()> {
    let endpoint =
        match tokio::task::spawn_blocking(move || vm::vnc_endpoint_by_name(&connect_uri, &name))
            .await
            .context("libvirt task panicked")?
        {
            Ok(endpoint) => endpoint,
            Err(error) => {
                close_with_error(&mut socket, &error.to_string()).await;
                return Err(error);
            }
        };

    let host = endpoint.connect_host().to_string();
    let port = match endpoint.port_number() {
        Ok(port) => port,
        Err(error) => {
            close_with_error(&mut socket, &error.to_string()).await;
            return Err(error);
        }
    };
    let tcp = match TcpStream::connect((host.as_str(), port)).await {
        Ok(tcp) => tcp,
        Err(error) => {
            let error = anyhow::Error::new(error)
                .context(format!("failed to connect to VNC endpoint {host}:{port}"));
            close_with_error(&mut socket, &error.to_string()).await;
            return Err(error);
        }
    };

    bridge_vnc(socket, tcp).await
}

async fn close_with_error(socket: &mut WebSocket, reason: &str) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code: close_code::ERROR,
            reason: close_reason(reason).into(),
        })))
        .await;
}

fn close_reason(reason: &str) -> String {
    const MAX_REASON_BYTES: usize = 120;
    if reason.len() <= MAX_REASON_BYTES {
        return reason.to_string();
    }

    let mut end = MAX_REASON_BYTES;
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &reason[..end])
}

async fn bridge_vnc(socket: WebSocket, tcp: TcpStream) -> Result<()> {
    let (mut ws_sender, mut ws_receiver) = socket.split();
    let (mut tcp_reader, mut tcp_writer) = tcp.into_split();

    let browser_to_vnc = async {
        while let Some(message) = ws_receiver.next().await {
            match message.context("failed to receive WebSocket message")? {
                Message::Binary(bytes) => {
                    tcp_writer.write_all(&bytes).await?;
                }
                Message::Close(_) => break,
                _ => {}
            }
        }

        tcp_writer.shutdown().await?;
        Result::<()>::Ok(())
    };

    let vnc_to_browser = async {
        let mut buffer = vec![0; 16 * 1024];
        loop {
            let bytes_read = tcp_reader.read(&mut buffer).await?;
            if bytes_read == 0 {
                break;
            }

            ws_sender
                .send(Message::Binary(buffer[..bytes_read].to_vec().into()))
                .await
                .context("failed to send WebSocket message")?;
        }

        let _ = ws_sender.send(Message::Close(None)).await;
        Result::<()>::Ok(())
    };

    tokio::select! {
        result = browser_to_vnc => result,
        result = vnc_to_browser => result,
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            let _ = signal.recv().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "qtr=info,tower_http=info".into());
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request, routing::get};
    use tower::ServiceExt;

    fn test_state() -> AppState {
        AppState {
            connect_uri: "test:///default".to_string(),
            api_token: Arc::from("test-token"),
            vnc_tickets: Arc::new(Mutex::new(HashMap::new())),
            jobs: None,
            max_iso_upload_bytes: 1024,
            iso_uploads: Arc::new(Semaphore::new(1)),
        }
    }

    fn web_args(api_token: Option<&str>, api_token_file: Option<PathBuf>) -> WebArgs {
        WebArgs {
            listen: "127.0.0.1:8080".parse().unwrap(),
            connect_uri: "test:///default".to_string(),
            web_dir: PathBuf::from("web/dist"),
            api_token: api_token.map(str::to_string),
            api_token_file,
            state_dir: PathBuf::from(".qtr/server"),
            image_root: PathBuf::from(".tmp/disks"),
            media_root: PathBuf::from(".tmp/iso"),
            log_root: PathBuf::from(".tmp/logs"),
            max_iso_upload_bytes: 1024,
        }
    }

    #[test]
    fn loads_api_token_from_value_or_file() {
        assert_eq!(
            load_api_token(&web_args(Some("token"), None)).unwrap(),
            "token"
        );

        let directory = std::env::temp_dir().join(format!("qtr-token-test-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("api-token");
        std::fs::write(&path, "file-token\n").unwrap();
        assert_eq!(
            load_api_token(&web_args(None, Some(path))).unwrap(),
            "file-token"
        );
        assert!(load_api_token(&web_args(Some("bad token"), None)).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn update_request_keeps_disk_detach_out_of_http_api() {
        let present = r#"{
            "name": "vm",
            "disks": [{"id": "root", "path": "/tmp/root.qcow2", "format": "qcow2"}]
        }"#;
        let absent = r#"{
            "name": "vm",
            "disks": [{"id": "root", "state": "absent"}]
        }"#;
        let cdroms = r#"{
            "name": "vm",
            "disks": [{"id": "root", "path": "/tmp/root.qcow2", "format": "qcow2"}],
            "cdroms": []
        }"#;

        assert!(serde_json::from_str::<UpdateVmRequest>(present).is_ok());
        assert!(serde_json::from_str::<UpdateVmRequest>(absent).is_err());
        assert!(serde_json::from_str::<UpdateVmRequest>(cdroms).is_err());
    }

    #[tokio::test]
    async fn typed_errors_map_to_http_statuses() {
        let app = Router::new()
            .route(
                "/invalid",
                get(|| async {
                    Err::<StatusCode, AppError>(
                        vm::VmApiError::InvalidRequest(anyhow::anyhow!("bad input")).into(),
                    )
                }),
            )
            .route(
                "/missing",
                get(|| async {
                    Err::<StatusCode, AppError>(vm::VmApiError::NotFound("missing".into()).into())
                }),
            )
            .route(
                "/conflict",
                get(|| async {
                    Err::<StatusCode, AppError>(
                        vm::VmApiError::Conflict(anyhow::anyhow!("already exists")).into(),
                    )
                }),
            )
            .route(
                "/internal",
                get(|| async {
                    Err::<StatusCode, AppError>(
                        vm::VmApiError::Internal(anyhow::anyhow!("libvirt failed")).into(),
                    )
                }),
            );

        for (path, status, title) in [
            ("/invalid", StatusCode::BAD_REQUEST, "Bad Request"),
            ("/missing", StatusCode::NOT_FOUND, "Not Found"),
            ("/conflict", StatusCode::CONFLICT, "Conflict"),
            (
                "/internal",
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal Server Error",
            ),
        ] {
            let response = app
                .clone()
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), status);
            assert_eq!(
                response.headers()[header::CONTENT_TYPE],
                "application/problem+json"
            );

            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["type"], "about:blank");
            assert_eq!(body["title"], title);
            assert_eq!(body["status"], status.as_u16());
        }
    }

    #[tokio::test]
    async fn bearer_authentication_protects_management_routes() {
        let state = test_state();
        let app = Router::new()
            .route("/", get(|| async { StatusCode::NO_CONTENT }))
            .route_layer(middleware::from_fn_with_state(
                state.clone(),
                require_bearer_token,
            ))
            .with_state(state);

        for authorization in [None, Some("Bearer wrong-token")] {
            let mut request = Request::get("/");
            if let Some(value) = authorization {
                request = request.header(header::AUTHORIZATION, value);
            }
            let response = app
                .clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(
                response.headers()[header::CONTENT_TYPE],
                "application/problem+json"
            );
        }

        let response = app
            .oneshot(
                Request::get("/")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn openapi_document_describes_versioned_bearer_api() {
        let router = app(
            "test:///default".to_string(),
            PathBuf::from("web/dist"),
            "test-token".to_string(),
            None,
        );
        let response = router
            .oneshot(
                Request::get("/api/v1/openapi.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let document: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(document["openapi"], "3.1.0");
        assert!(document["paths"]["/api/v1/vms"].is_object());
        assert!(document["paths"]["/api/v1/install-jobs"].is_object());
        assert!(document["paths"]["/api/v1/images"].is_object());
        assert!(document["paths"]["/api/v1/media"].is_object());
        assert!(document["paths"]["/api/v1/networks"].is_object());
        assert!(document["paths"]["/api/v1/session"].is_object());
        let create_properties = &document["components"]["schemas"]["CreateVmRequest"]["properties"];
        assert!(create_properties["resources"].is_object());
        assert!(create_properties["networkId"].is_object());
        assert!(create_properties["mediaId"].is_object());
        assert!(create_properties["memoryGiB"].is_null());
        let install_properties =
            &document["components"]["schemas"]["FedoraInstallRequest"]["properties"];
        assert!(install_properties["mediaId"].is_object());
        assert!(install_properties["imageId"].is_object());
        assert!(install_properties["iso"].is_null());
        assert!(install_properties["disk"].is_null());
        assert_eq!(
            document["components"]["securitySchemes"]["bearerAuth"]["scheme"],
            "bearer"
        );
    }

    #[tokio::test]
    async fn app_exposes_only_the_versioned_management_api() {
        let router = app(
            "test:///default".to_string(),
            PathBuf::from("web/dist"),
            "test-token".to_string(),
            None,
        );

        let response = router
            .clone()
            .oneshot(Request::get("/api/v1/vms").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = router
            .clone()
            .oneshot(Request::get("/api/vms").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/problem+json"
        );

        let response = router
            .oneshot(
                Request::get("/api/v1/session")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn install_api_uses_managed_resource_ids() {
        let directory = std::env::temp_dir().join(format!("qtr-web-job-test-{}", Uuid::new_v4()));
        let media = directory.join("media");
        let images = directory.join("images");
        std::fs::create_dir_all(&media).unwrap();
        std::fs::create_dir_all(&images).unwrap();
        std::fs::write(media.join("Fedora.iso"), b"iso").unwrap();
        std::fs::write(images.join("reserved.qcow2"), b"disk").unwrap();
        let jobs = JobService::start(JobRoots {
            state: directory.join("state"),
            images,
            media,
            logs: directory.join("logs"),
            connect_uri: "test:///default".to_string(),
        })
        .unwrap();
        let app = app(
            "test:///default".to_string(),
            PathBuf::from("web/dist"),
            "test-token".to_string(),
            Some(jobs),
        );

        let response = app
            .clone()
            .oneshot(
                Request::get("/api/v1/media")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let resources: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(resources[0]["id"], "Fedora.iso");

        for (id, status) in [
            ("wrong.raw", StatusCode::BAD_REQUEST),
            ("reserved.qcow2", StatusCode::CONFLICT),
        ] {
            let request = serde_json::json!({
                "id": id,
                "format": "qcow2",
                "sizeBytes": 1_073_741_824_u64
            });
            let response = app
                .clone()
                .oneshot(
                    Request::post("/api/v1/images")
                        .header(header::AUTHORIZATION, "Bearer test-token")
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(request.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), status);
        }

        let request = serde_json::json!({
            "name": "fedora-test",
            "mediaId": "../Fedora.iso",
            "imageId": "fedora-test.qcow2",
            "sshAuthorizedKey": "ssh-ed25519 AAAA test"
        });
        let response = app
            .clone()
            .oneshot(
                Request::post("/api/v1/install-jobs")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(request.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/problem+json"
        );

        let request = serde_json::json!({
            "name": "fedora-test",
            "mediaId": "Fedora.iso",
            "imageId": "reserved.qcow2",
            "sshAuthorizedKey": "ssh-ed25519 AAAA test"
        });
        let response = app
            .clone()
            .oneshot(
                Request::post("/api/v1/install-jobs")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(request.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);

        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn iso_upload_is_atomic_limited_and_deletable() {
        let directory = std::env::temp_dir().join(format!("qtr-web-iso-test-{}", Uuid::new_v4()));
        let jobs = JobService::start(JobRoots {
            state: directory.join("state"),
            images: directory.join("images"),
            media: directory.join("isos"),
            logs: directory.join("logs"),
            connect_uri: "test:///default".to_string(),
        })
        .unwrap();
        let router = app_with_iso_limit(
            "test:///default".to_string(),
            PathBuf::from("web/dist"),
            "test-token".to_string(),
            Some(jobs),
            40_000,
        );
        let mut iso = vec![0_u8; 32_774];
        iso[32_769..32_774].copy_from_slice(b"CD001");

        let upload = || {
            Request::put("/api/v1/media/test.iso")
                .header(header::AUTHORIZATION, "Bearer test-token")
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .body(Body::from(iso.clone()))
                .unwrap()
        };
        let response = router.clone().oneshot(upload()).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        assert!(directory.join("isos/test.iso").is_file());

        let response = router.clone().oneshot(upload()).await.unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);

        let response = router
            .clone()
            .oneshot(
                Request::put("/api/v1/media/not-an-iso.iso")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .header(header::CONTENT_TYPE, "application/octet-stream")
                    .body(Body::from(vec![0_u8; 32_774]))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(!directory.join("isos/not-an-iso.iso").exists());

        let response = router
            .clone()
            .oneshot(
                Request::put("/api/v1/media/large.iso")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .header(header::CONTENT_TYPE, "application/octet-stream")
                    .header(header::CONTENT_LENGTH, "40001")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

        let response = router
            .clone()
            .oneshot(
                Request::delete("/api/v1/media/test.iso")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(!directory.join("isos/test.iso").exists());

        drop(router);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn managed_image_lifecycle_is_safe_and_complete() {
        let directory = std::env::temp_dir().join(format!("qtr-web-image-test-{}", Uuid::new_v4()));
        let jobs = JobService::start(JobRoots {
            state: directory.join("state"),
            images: directory.join("images"),
            media: directory.join("isos"),
            logs: directory.join("logs"),
            connect_uri: "test:///default".to_string(),
        })
        .unwrap();
        for id in ["root.qcow2", "data.qcow2"] {
            assert!(matches!(
                jobs.create_image(id, crate::config::DiskFormat::Qcow2, 1024 * 1024)
                    .unwrap(),
                ImageCreateOutcome::Created(_)
            ));
        }
        let name = format!("qtr-image-test-{}", Uuid::new_v4());
        let request: CreateVmRequest = serde_json::from_value(serde_json::json!({
            "name": name,
            "resources": { "vcpus": 1, "memoryMib": 512 },
            "disks": [{
                "imageId": "root.qcow2",
                "format": "qcow2",
                "bus": "virtio-blk"
            }],
            "networkId": "default",
            "mediaId": null,
            "console": { "graphics": "none", "serialLog": false }
        }))
        .unwrap();
        vm::create_by_manifest("test:///default", request.into_manifest(&jobs).unwrap()).unwrap();
        let router = app(
            "test:///default".to_string(),
            PathBuf::from("web/dist"),
            "test-token".to_string(),
            Some(jobs),
        );

        let attach = Request::put(format!("/api/v1/vms/{name}/disks/data.qcow2"))
            .header(header::AUTHORIZATION, "Bearer test-token")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"bus":"virtio-blk"}"#))
            .unwrap();
        let response = router.clone().oneshot(attach).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = router
            .clone()
            .oneshot(
                Request::get("/api/v1/images")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let images: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let data = images
            .as_array()
            .unwrap()
            .iter()
            .find(|image| image["id"] == "data.qcow2")
            .unwrap();
        assert_eq!(data["format"], "qcow2");
        assert_eq!(data["attachments"][0]["vmName"], name);

        let response = router
            .clone()
            .oneshot(
                Request::delete("/api/v1/images/data.qcow2")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);

        vm::start_by_name("test:///default", &name).unwrap();
        let response = router
            .clone()
            .oneshot(
                Request::post("/api/v1/images/data.qcow2/resize")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"sizeBytes":2097152}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        vm::destroy_by_name("test:///default", &name).unwrap();

        let response = router
            .clone()
            .oneshot(
                Request::post("/api/v1/images/data.qcow2/resize")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"sizeBytes":2097152}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = router
            .clone()
            .oneshot(
                Request::delete(format!("/api/v1/vms/{name}/disks/data.qcow2"))
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = router
            .clone()
            .oneshot(
                Request::delete("/api/v1/images/data.qcow2")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        vm::undefine_by_name("test:///default", &name).unwrap();
        drop(router);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn managed_cdrom_lifecycle_supports_multiple_trays() {
        let directory = std::env::temp_dir().join(format!("qtr-web-cdrom-test-{}", Uuid::new_v4()));
        let jobs = JobService::start(JobRoots {
            state: directory.join("state"),
            images: directory.join("images"),
            media: directory.join("isos"),
            logs: directory.join("logs"),
            connect_uri: "test:///default".to_string(),
        })
        .unwrap();
        assert!(matches!(
            jobs.create_image("root.qcow2", crate::config::DiskFormat::Qcow2, 1024 * 1024)
                .unwrap(),
            ImageCreateOutcome::Created(_)
        ));
        for id in ["first.iso", "second.iso"] {
            let mut iso = vec![0_u8; 32_774];
            iso[32_769..32_774].copy_from_slice(b"CD001");
            std::fs::write(jobs.media_root().join(id), iso).unwrap();
        }
        let name = format!("qtr-cdrom-test-{}", Uuid::new_v4());
        let request: CreateVmRequest = serde_json::from_value(serde_json::json!({
            "name": name,
            "resources": { "vcpus": 1, "memoryMib": 512 },
            "disks": [{
                "imageId": "root.qcow2",
                "format": "qcow2",
                "bus": "virtio-blk"
            }],
            "networkId": "default",
            "mediaId": null,
            "console": { "graphics": "none", "serialLog": false }
        }))
        .unwrap();
        vm::create_by_manifest("test:///default", request.into_manifest(&jobs).unwrap()).unwrap();
        let router = app(
            "test:///default".to_string(),
            PathBuf::from("web/dist"),
            "test-token".to_string(),
            Some(jobs),
        );

        for (id, media_id) in [("installer", Some("first.iso")), ("tools", None)] {
            let response = router
                .clone()
                .oneshot(
                    Request::post(format!("/api/v1/vms/{name}/cdroms"))
                        .header(header::AUTHORIZATION, "Bearer test-token")
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(
                            serde_json::json!({ "id": id, "mediaId": media_id }).to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::CREATED);
        }

        vm::start_by_name("test:///default", &name).unwrap();

        let response = router
            .clone()
            .oneshot(
                Request::post(format!("/api/v1/vms/{name}/cdroms"))
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"id":"running-add","mediaId":null}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);

        let response = router
            .clone()
            .oneshot(
                Request::put(format!("/api/v1/vms/{name}/cdroms/tools/media"))
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"mediaId":"second.iso"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let summary: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(summary["cdroms"].as_array().unwrap().len(), 2);
        assert!(
            summary["cdroms"]
                .as_array()
                .unwrap()
                .iter()
                .any(|cdrom| { cdrom["id"] == "tools" && cdrom["mediaId"] == "second.iso" })
        );

        let response = router
            .clone()
            .oneshot(
                Request::delete(format!("/api/v1/vms/{name}/cdroms/tools/media"))
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        vm::destroy_by_name("test:///default", &name).unwrap();

        let response = router
            .clone()
            .oneshot(
                Request::delete(format!("/api/v1/vms/{name}/cdroms/installer"))
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        vm::undefine_by_name("test:///default", &name).unwrap();
        drop(router);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn committed_openapi_document_is_current() {
        let generated = format!(
            "{}\n",
            serde_json::to_string_pretty(&openapi_document()).unwrap()
        );
        assert_eq!(generated, include_str!("../openapi/qtr-v1.json"));
    }

    #[test]
    fn vnc_tickets_are_scoped_and_single_use() {
        let state = test_state();
        state.vnc_tickets.lock().unwrap().insert(
            "ticket".to_string(),
            VncTicket {
                vm_name: "vm-one".to_string(),
                expires_at: Instant::now() + Duration::from_secs(1),
            },
        );

        assert!(!consume_vnc_ticket(&state, "vm-two", "ticket"));
        assert!(consume_vnc_ticket(&state, "vm-one", "ticket"));
        assert!(!consume_vnc_ticket(&state, "vm-one", "ticket"));
    }

    #[test]
    fn create_vm_request_resolves_only_managed_resources() {
        let directory = std::env::temp_dir().join(format!("qtr-create-vm-test-{}", Uuid::new_v4()));
        let media = directory.join("media");
        let images = directory.join("images");
        std::fs::create_dir_all(&media).unwrap();
        std::fs::create_dir_all(&images).unwrap();
        std::fs::write(media.join("installer.iso"), b"iso").unwrap();
        crate::disk::create_image(
            &images.join("system.qcow2"),
            crate::config::DiskFormat::Qcow2,
            "1048576",
        )
        .unwrap();
        let jobs = JobService::start(JobRoots {
            state: directory.join("state"),
            images: images.clone(),
            media: media.clone(),
            logs: directory.join("logs"),
            connect_uri: "test:///default".to_string(),
        })
        .unwrap();
        let request: CreateVmRequest = serde_json::from_value(serde_json::json!({
            "name": "managed-vm",
            "resources": { "vcpus": 2, "memoryMib": 1536 },
            "disks": [{
                "imageId": "system.qcow2",
                "format": "qcow2",
                "bus": "virtio-blk"
            }],
            "networkId": "default",
            "mediaId": "installer.iso",
            "console": { "graphics": "vnc", "serialLog": true }
        }))
        .unwrap();
        let manifest = request.into_manifest(&jobs).unwrap();
        assert_eq!(manifest.memory.unwrap().size_mib, 1536);
        assert_eq!(manifest.boot.unwrap(), ["cdrom", "hd"]);
        assert_eq!(manifest.cdrom.unwrap(), media.join("installer.iso"));
        assert_eq!(
            manifest.disks[0].as_present().unwrap().path,
            images.join("system.qcow2")
        );
        assert_eq!(
            manifest.serial_log.unwrap(),
            directory.join("logs/managed-vm.serial.log")
        );

        drop(jobs);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn origin_matches_host_accepts_same_origin() {
        assert!(origin_matches_host(
            "http://127.0.0.1:8080",
            "127.0.0.1:8080"
        ));
        assert!(origin_matches_host(
            "https://vm.example.com",
            "vm.example.com"
        ));
        assert!(origin_matches_host(
            "http://LOCALHOST:8080/",
            "localhost:8080"
        ));
        assert!(origin_matches_host("http://[::1]:8080", "[::1]:8080"));
    }

    #[test]
    fn origin_matches_host_rejects_foreign_origins() {
        assert!(!origin_matches_host(
            "http://evil.example.com",
            "127.0.0.1:8080"
        ));
        assert!(!origin_matches_host(
            "http://127.0.0.1:8080.evil.example.com",
            "127.0.0.1:8080"
        ));
        assert!(!origin_matches_host(
            "http://127.0.0.1:9090",
            "127.0.0.1:8080"
        ));
        assert!(!origin_matches_host("http://127.0.0.1:8080", ""));
    }
}
