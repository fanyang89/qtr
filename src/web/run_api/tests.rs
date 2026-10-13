use super::*;
use axum::{body::Body, http::Request};
use serde_json::{Value, json};
use tower::ServiceExt;

fn roots(directory: &FsPath) -> JobRoots {
    JobRoots {
        state: directory.join("state"),
        images: directory.join("images"),
        media: directory.join("media"),
        logs: directory.join("logs"),
        connect_uri: "test:///default".to_string(),
    }
}

fn router(jobs: JobService) -> Router {
    app(
        "test:///default".to_string(),
        PathBuf::from("web/dist"),
        "test-token".to_string(),
        Some(jobs),
    )
}

async fn call(
    router: &Router,
    method: &str,
    path: &str,
    data: Value,
    expected: StatusCode,
) -> Value {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(format!("/api/v1{path}"))
                .header(header::AUTHORIZATION, "Bearer test-token")
                .header(header::CONTENT_TYPE, "application/json")
                .body(if data.is_null() {
                    Body::empty()
                } else {
                    Body::from(data.to_string())
                })
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    assert_eq!(status, expected, "{method} {path}: {body}");
    body
}

fn setup() -> (PathBuf, JobService, Router) {
    let directory = std::env::temp_dir().join(format!("qtr-run-api-test-{}", Uuid::new_v4()));
    let jobs = JobService::start(roots(&directory)).unwrap();
    jobs.create_image("base.raw", crate::config::DiskFormat::Raw, 1024 * 1024)
        .unwrap();
    let router = router(jobs.clone());
    (directory, jobs, router)
}

fn clone_request(id: &str) -> Value {
    json!({"kind":"imageClone", "sourceImageId":"base.raw", "request":{"id":id}})
}

#[tokio::test]
async fn restart_recovers_published_intent_and_cleanup_is_idempotent() {
    let (directory, jobs, app) = setup();
    let staged = jobs.image_root().join(".uploads/live.partial");
    std::fs::write(&staged, b"in-flight upload").unwrap();
    assert!(JobService::start(roots(&directory)).is_err());
    assert!(
        staged.exists(),
        "a second server must not clean active staging files"
    );
    let response = app
        .clone()
        .oneshot(
            Request::put("/api/v1/runs/test-run")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    call(&app, "PUT", "/runs/test-run", Value::Null, StatusCode::OK).await;
    call(
        &app,
        "POST",
        "/runs/test-run/resources",
        clone_request("node.qcow2"),
        StatusCode::OK,
    )
    .await;
    let resource = jobs
        .runs
        .get("test-run")
        .unwrap()
        .unwrap()
        .resources
        .remove(0);
    jobs.runs.resource_state(&resource, "creating").unwrap();
    drop(app);
    drop(jobs);
    let jobs = JobService::start(roots(&directory)).unwrap();
    let app = router(jobs.clone());
    let replay = call(
        &app,
        "POST",
        "/runs/test-run/resources",
        clone_request("node.qcow2"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(replay["state"], "ready");
    let mut different = clone_request("node.qcow2");
    different["sourceImageId"] = json!("other.raw");
    call(
        &app,
        "POST",
        "/runs/test-run/resources",
        different,
        StatusCode::CONFLICT,
    )
    .await;
    call(&app, "PUT", "/runs/other-run", Value::Null, StatusCode::OK).await;
    call(
        &app,
        "POST",
        "/runs/other-run/resources",
        clone_request("node.qcow2"),
        StatusCode::CONFLICT,
    )
    .await;
    jobs.create_image("unrelated.raw", crate::config::DiskFormat::Raw, 1024)
        .unwrap();
    for _ in 0..2 {
        let run = call(
            &app,
            "POST",
            "/runs/test-run/cleanup",
            Value::Null,
            StatusCode::OK,
        )
        .await;
        assert_eq!(run["state"], "closed");
        assert_eq!(run["resources"][0]["state"], "deleted");
    }
    assert!(!jobs.image_root().join("node.qcow2").exists());
    assert!(!runs::pin_path(jobs.image_root(), &resource).exists());
    assert!(jobs.image_root().join("base.raw").exists());
    assert!(jobs.image_root().join("unrelated.raw").exists());
    call(
        &app,
        "POST",
        "/runs/test-run/resources",
        clone_request("new.qcow2"),
        StatusCode::CONFLICT,
    )
    .await;
    let response = call(&app, "GET", "/runs", Value::Null, StatusCode::OK).await;
    assert_eq!(response.as_array().unwrap().len(), 2);
    assert!(!response.to_string().contains("sourceImageId"));
    drop(app);
    drop(jobs);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn replacement_and_unowned_files_are_never_deleted() {
    let (directory, jobs, app) = setup();
    call(&app, "PUT", "/runs/test-run", Value::Null, StatusCode::OK).await;
    call(
        &app,
        "POST",
        "/runs/test-run/resources",
        clone_request("node.qcow2"),
        StatusCode::OK,
    )
    .await;
    let path = jobs.image_root().join("node.qcow2");
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, b"foreign replacement").unwrap();
    call(
        &app,
        "POST",
        "/runs/test-run/cleanup",
        Value::Null,
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(std::fs::read(&path).unwrap(), b"foreign replacement");
    call(
        &app,
        "POST",
        "/runs/test-run/resources",
        clone_request("new.qcow2"),
        StatusCode::CONFLICT,
    )
    .await;
    // A different run cannot adopt an already-existing base image as its clone.
    call(
        &app,
        "PUT",
        "/runs/collision-run",
        Value::Null,
        StatusCode::OK,
    )
    .await;
    let request =
        json!({"kind":"imageClone", "sourceImageId":"base.raw", "request":{"id":"existing.qcow2"}});
    jobs.create_image("existing.qcow2", crate::config::DiskFormat::Qcow2, 1024)
        .unwrap();
    call(
        &app,
        "POST",
        "/runs/collision-run/resources",
        request,
        StatusCode::CONFLICT,
    )
    .await;
    call(
        &app,
        "POST",
        "/runs/collision-run/cleanup",
        Value::Null,
        StatusCode::CONFLICT,
    )
    .await;
    assert!(jobs.image_root().join("existing.qcow2").exists());
    drop(app);
    drop(jobs);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn vm_cleanup_uses_uuid_and_stops_before_deleting_mismatched_resources() {
    // libvirt's test:///default state is process-global. Domain replacement must
    // not race inventory scans in the existing parallel API tests.
    const CHILD: &str = "QTR_RUN_VM_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "web::run_api::tests::vm_cleanup_uses_uuid_and_stops_before_deleting_mismatched_resources",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let (directory, jobs, app) = setup();
    let connection = Connect::open(Some("test:///default")).unwrap();
    let name = format!("qtr-run-{}", Uuid::new_v4());
    call(&app, "PUT", "/runs/test-run", Value::Null, StatusCode::OK).await;
    call(
        &app,
        "POST",
        "/runs/test-run/resources",
        clone_request("node.qcow2"),
        StatusCode::OK,
    )
    .await;
    let request = json!({"kind":"vm", "request": {
        "name":name, "resources":{"vcpus":1,"memoryMib":128},
        "disks":[{"imageId":"node.qcow2","format":"qcow2","bus":"virtio-blk"}],
        "networkId":"default", "console":{"graphics":"none","serialLog":false}
    }});
    call(
        &app,
        "POST",
        "/runs/test-run/resources",
        request.clone(),
        StatusCode::OK,
    )
    .await;
    call(
        &app,
        "POST",
        "/runs/test-run/resources",
        request,
        StatusCode::OK,
    )
    .await;
    let domain = Domain::lookup_by_name(&connection, &name).unwrap();
    let uuid = domain.get_uuid_string().unwrap();
    let xml = domain
        .get_xml_desc(virt::sys::VIR_DOMAIN_XML_INACTIVE)
        .unwrap();
    domain.undefine().unwrap();
    let replacement_uuid = Uuid::new_v4().to_string();
    let replacement =
        Domain::define_xml(&connection, &xml.replace(&uuid, &replacement_uuid)).unwrap();
    replacement.create().unwrap();
    call(
        &app,
        "POST",
        "/runs/test-run/cleanup",
        Value::Null,
        StatusCode::CONFLICT,
    )
    .await;
    assert!(replacement.is_active().unwrap());
    assert!(jobs.image_root().join("node.qcow2").exists());
    replacement.destroy().unwrap();
    replacement.undefine().unwrap();
    let original = Domain::define_xml(&connection, &xml).unwrap();
    original.create().unwrap();
    call(
        &app,
        "POST",
        "/runs/test-run/cleanup",
        Value::Null,
        StatusCode::OK,
    )
    .await;
    assert!(Domain::lookup_by_name(&connection, &name).is_err());
    assert!(!jobs.image_root().join("node.qcow2").exists());
    assert!(jobs.image_root().join("base.raw").exists());
    drop(app);
    drop(jobs);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn cleanup_recovers_a_crash_between_pinning_and_publication() {
    let (directory, jobs, app) = setup();
    jobs.runs.create("interrupted").unwrap();
    let resource = jobs
        .runs
        .reserve("interrupted", "image", "unpublished.qcow2", "{}")
        .unwrap();
    let staging = jobs.image_root().join(".uploads/interrupted.partial");
    std::fs::write(&staging, b"unpublished data").unwrap();
    runs::pin_file(jobs.image_root(), &resource, &staging).unwrap();
    drop(app);
    drop(jobs);
    let jobs = JobService::start(roots(&directory)).unwrap();
    assert!(!staging.exists());
    assert!(runs::pin_path(jobs.image_root(), &resource).exists());
    let app = router(jobs.clone());
    call(
        &app,
        "POST",
        "/runs/interrupted/cleanup",
        Value::Null,
        StatusCode::OK,
    )
    .await;
    assert!(!runs::pin_path(jobs.image_root(), &resource).exists());
    assert!(jobs.image_root().join("base.raw").exists());
    drop(app);
    drop(jobs);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn cancelled_request_keeps_run_gate_until_worker_finishes() {
    let (directory, jobs, app) = setup();
    let worker_jobs = jobs.clone();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let task = tokio::spawn(async move {
        locked(worker_jobs, move |_| {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(())
        })
        .await
    });
    tokio::time::timeout(Duration::from_secs(2), started_rx)
        .await
        .unwrap()
        .unwrap();
    task.abort();
    tokio::task::yield_now().await;
    assert!(jobs.runs.gate.try_acquire().is_err());
    release_tx.send(()).unwrap();
    let permit = tokio::time::timeout(Duration::from_secs(2), jobs.runs.gate.acquire())
        .await
        .unwrap()
        .unwrap();
    drop(permit);
    drop(app);
    drop(jobs);
    std::fs::remove_dir_all(directory).unwrap();
}
