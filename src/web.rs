use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use axum::{
    Json, Router,
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
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
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
use crate::{config::WebArgs, vm};

#[derive(Clone)]
struct AppState {
    connect_uri: String,
    api_token: Arc<str>,
    vnc_tickets: Arc<Mutex<HashMap<String, VncTicket>>>,
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
    detail: &'static str,
}

impl ProblemDetails {
    fn response(status: StatusCode, title: &'static str, detail: &'static str) -> Response {
        let mut response = (
            status,
            Json(Self {
                problem_type: "about:blank",
                title,
                status: status.as_u16(),
                detail,
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
    Internal(anyhow::Error),
}

type AppResult<T> = std::result::Result<T, AppError>;

impl AppError {
    fn status(&self) -> StatusCode {
        match self {
            Self::Vm(vm::VmApiError::InvalidRequest(_)) => StatusCode::BAD_REQUEST,
            Self::Vm(vm::VmApiError::NotFound(_)) => StatusCode::NOT_FOUND,
            Self::Vm(vm::VmApiError::Conflict(_)) => StatusCode::CONFLICT,
            Self::Vm(vm::VmApiError::Internal(_)) | Self::Internal(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    }

    fn error(&self) -> &dyn std::fmt::Debug {
        match self {
            Self::Vm(error) => error,
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
        let (title, detail) = match status {
            StatusCode::BAD_REQUEST => ("Bad Request", "The request is invalid."),
            StatusCode::NOT_FOUND => ("Not Found", "The VM was not found."),
            StatusCode::CONFLICT => ("Conflict", "The VM state conflicts with the request."),
            _ => (
                "Internal Server Error",
                "The server could not complete the request.",
            ),
        };
        ProblemDetails::response(status, title, detail)
    }
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateVmRequest {
    name: String,
    io_threads: Option<vm::VmIoThreads>,
    #[schema(value_type = Vec<Object>)]
    disks: Vec<vm::VmDisk>,
    #[schema(value_type = Option<String>)]
    cdrom: Option<PathBuf>,
    boot: Option<Vec<String>>,
    #[serde(rename = "memoryGiB")]
    memory_gib: u64,
    vcpus: u32,
    network: String,
    graphics: GraphicsMode,
    vnc_listen: String,
    vnc_port: Option<u16>,
    #[schema(value_type = Option<String>)]
    serial_log: Option<PathBuf>,
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
    fn into_manifest(self) -> vm::VmManifest {
        vm::VmManifest {
            name: self.name,
            machine: None,
            cpu: None,
            memory: None,
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

pub fn run(args: WebArgs) -> Result<()> {
    clear_error_callback();
    init_tracing();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build Tokio runtime")?;

    runtime.block_on(run_async(args))
}

async fn run_async(args: WebArgs) -> Result<()> {
    let listen = args.listen;
    if !listen.ip().is_loopback() {
        let warning = format!(
            "qtr web uses unencrypted HTTP at {listen}; use a trusted network or a TLS reverse proxy"
        );
        eprintln!("[qtr] WARNING: {warning}");
        tracing::warn!(warning);
    }
    let app = app(args.connect_uri, args.web_dir, args.api_token);
    let listener = TcpListener::bind(listen)
        .await
        .with_context(|| format!("failed to bind web server at {listen}"))?;

    tracing::info!(%listen, "serving qtr web UI");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("web server failed")
}

fn app(connect_uri: String, web_dir: PathBuf, api_token: String) -> Router {
    let state = AppState {
        connect_uri,
        api_token: api_token.into(),
        vnc_tickets: Arc::new(Mutex::new(HashMap::new())),
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
    let connect_uri = state.connect_uri;
    let vms = run_libvirt(move || vm::list_summaries(&connect_uri)).await?;
    Ok(Json(vms))
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
    let connect_uri = state.connect_uri;
    let vm = run_libvirt(move || vm::get_summary(&connect_uri, &name)).await?;
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
    let manifest = request.into_manifest();
    let connect_uri = state.connect_uri;
    let vm = run_libvirt(move || vm::create_by_manifest(&connect_uri, manifest)).await?;
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
    let connect_uri = state.connect_uri;
    let vm = run_libvirt(move || vm::apply_by_manifest(&connect_uri, manifest)).await?;
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
    let connect_uri = state.connect_uri;
    run_libvirt(move || vm::start_by_name(&connect_uri, &name)).await?;
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
    let connect_uri = state.connect_uri;
    run_libvirt(move || vm::shutdown_by_name(&connect_uri, &name, false)).await?;
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
    let connect_uri = state.connect_uri;
    run_libvirt(move || vm::destroy_by_name(&connect_uri, &name)).await?;
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
    let connect_uri = state.connect_uri;
    run_libvirt(move || vm::undefine_by_name(&connect_uri, &name)).await?;
    Ok(StatusCode::NO_CONTENT)
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
        }
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
        let app = app(
            "test:///default".to_string(),
            PathBuf::from("web/dist"),
            "test-token".to_string(),
        );
        let response = app
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
        assert_eq!(
            document["components"]["securitySchemes"]["bearerAuth"]["scheme"],
            "bearer"
        );
    }

    #[tokio::test]
    async fn app_exposes_only_the_versioned_management_api() {
        let app = app(
            "test:///default".to_string(),
            PathBuf::from("web/dist"),
            "test-token".to_string(),
        );

        let response = app
            .clone()
            .oneshot(Request::get("/api/v1/vms").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .oneshot(Request::get("/api/vms").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/problem+json"
        );
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
