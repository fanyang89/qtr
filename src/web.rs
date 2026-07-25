use std::path::PathBuf;

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::{
        Path, State,
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade, close_code},
    },
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde::Serialize;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tower_http::{
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};
use virt::error::clear_error_callback;

use crate::config::GraphicsMode;
use crate::{config::WebArgs, vm};

#[derive(Clone)]
struct AppState {
    connect_uri: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HealthStatus {
    ok: bool,
    libvirt_uri: String,
    version: &'static str,
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
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
        let message = match status {
            StatusCode::BAD_REQUEST => "invalid request",
            StatusCode::NOT_FOUND => "VM not found",
            StatusCode::CONFLICT => "VM state conflicts with the request",
            _ => "internal server error",
        };
        let body = Json(ErrorBody {
            error: message.to_string(),
        });
        (status, body).into_response()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateVmRequest {
    name: String,
    io_threads: Option<vm::VmIoThreads>,
    disks: Vec<vm::VmDisk>,
    cdrom: Option<PathBuf>,
    boot: Option<Vec<String>>,
    #[serde(rename = "memoryGiB")]
    memory_gib: u64,
    vcpus: u32,
    network: String,
    graphics: GraphicsMode,
    vnc_listen: String,
    vnc_port: Option<u16>,
    serial_log: Option<PathBuf>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateVmRequest {
    name: String,
    machine: Option<vm::VmMachine>,
    cpu: Option<vm::VmCpu>,
    memory: Option<vm::VmMemory>,
    io_threads: Option<vm::VmIoThreads>,
    disks: Vec<vm::VmDisk>,
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
            boot: self.boot,
            memory_gib: self.memory_gib,
            vcpus: self.vcpus,
            network: self.network,
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
            boot: self.boot,
            memory_gib: self.memory_gib,
            vcpus: self.vcpus,
            network: self.network,
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
            "qtr web API has no authentication; binding to {listen} exposes full VM control to the network"
        );
        eprintln!("[qtr] WARNING: {warning}");
        tracing::warn!(warning);
    }
    let app = app(args.connect_uri, args.web_dir);
    let listener = TcpListener::bind(listen)
        .await
        .with_context(|| format!("failed to bind web server at {listen}"))?;

    tracing::info!(%listen, "serving qtr web UI");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("web server failed")
}

fn app(connect_uri: String, web_dir: PathBuf) -> Router {
    let state = AppState { connect_uri };
    let index_html = web_dir.join("index.html");
    let api = Router::new()
        .route("/health", get(health))
        .route("/vms", get(list_vms).post(create_vm))
        .route(
            "/vms/{name}",
            get(get_vm).put(update_vm).delete(undefine_vm),
        )
        .route("/vms/{name}/start", post(start_vm))
        .route("/vms/{name}/shutdown", post(shutdown_vm))
        .route("/vms/{name}/destroy", post(destroy_vm))
        .route("/vms/{name}/vnc", get(vnc_ws));

    Router::new()
        .nest("/api", api)
        .fallback_service(
            ServeDir::new(web_dir)
                .append_index_html_on_directories(true)
                .fallback(ServeFile::new(index_html)),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

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

async fn list_vms(State(state): State<AppState>) -> AppResult<Json<Vec<vm::VmSummary>>> {
    let connect_uri = state.connect_uri;
    let vms = run_libvirt(move || vm::list_summaries(&connect_uri)).await?;
    Ok(Json(vms))
}

async fn get_vm(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> AppResult<Json<vm::VmSummary>> {
    let connect_uri = state.connect_uri;
    let vm = run_libvirt(move || vm::get_summary(&connect_uri, &name)).await?;
    Ok(Json(vm))
}

async fn create_vm(
    State(state): State<AppState>,
    Json(request): Json<CreateVmRequest>,
) -> AppResult<(StatusCode, Json<vm::VmSummary>)> {
    let manifest = request.into_manifest();
    let connect_uri = state.connect_uri;
    let vm = run_libvirt(move || vm::create_by_manifest(&connect_uri, manifest)).await?;
    Ok((StatusCode::CREATED, Json(vm)))
}

async fn update_vm(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(request): Json<UpdateVmRequest>,
) -> AppResult<Json<vm::VmSummary>> {
    let mut manifest = request.into_manifest();
    manifest.name = name;
    let connect_uri = state.connect_uri;
    let vm = run_libvirt(move || vm::apply_by_manifest(&connect_uri, manifest)).await?;
    Ok(Json(vm))
}

async fn start_vm(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> AppResult<StatusCode> {
    let connect_uri = state.connect_uri;
    run_libvirt(move || vm::start_by_name(&connect_uri, &name)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn shutdown_vm(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> AppResult<StatusCode> {
    let connect_uri = state.connect_uri;
    run_libvirt(move || vm::shutdown_by_name(&connect_uri, &name, false)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn destroy_vm(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> AppResult<StatusCode> {
    let connect_uri = state.connect_uri;
    run_libvirt(move || vm::destroy_by_name(&connect_uri, &name)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn undefine_vm(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> AppResult<StatusCode> {
    let connect_uri = state.connect_uri;
    run_libvirt(move || vm::undefine_by_name(&connect_uri, &name)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn vnc_ws(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
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
            return StatusCode::FORBIDDEN.into_response();
        }
    }

    let connect_uri = state.connect_uri;
    ws.on_upgrade(move |socket| async move {
        if let Err(error) = handle_vnc_upgrade(socket, connect_uri, name).await {
            tracing::debug!(%error, "VNC bridge closed with error");
        }
    })
    .into_response()
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
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

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

        for (path, status, message) in [
            ("/invalid", StatusCode::BAD_REQUEST, "invalid request"),
            ("/missing", StatusCode::NOT_FOUND, "VM not found"),
            (
                "/conflict",
                StatusCode::CONFLICT,
                "VM state conflicts with the request",
            ),
            (
                "/internal",
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal server error",
            ),
        ] {
            let response = app
                .clone()
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), status);

            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["error"], message);
        }
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
