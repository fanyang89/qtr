use super::*;
use crate::runs::{self, Run, RunResource};
use virt::{connect::Connect, domain::Domain};

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub(super) enum RunCreateRequest {
    ImageClone {
        #[serde(rename = "sourceImageId")]
        source_image_id: String,
        request: CloneImageRequest,
    },
    CloudInit {
        request: CloudInitSeedRequest,
    },
    Vm {
        request: CreateVmRequest,
    },
}

async fn locked<T: Send + 'static>(
    jobs: JobService,
    operation: impl FnOnce(JobService) -> AppResult<T> + Send + 'static,
) -> AppResult<T> {
    let permit = jobs
        .runs
        .gate
        .clone()
        .acquire_owned()
        .await
        .map_err(|error| AppError::Internal(error.into()))?;
    // Keep serialization inside the worker even if the HTTP request is cancelled.
    run_libvirt(move || {
        let _permit = permit;
        operation(jobs)
    })
    .await
}

fn conflict(error: anyhow::Error) -> AppError {
    AppError::Conflict(error.to_string())
}

#[utoipa::path(put, path = "/runs/{id}", tag = "runs", security(("bearerAuth" = [])),
    params(("id" = String, Path, description = "Stable caller-chosen run ID")),
    responses((status = OK, body = Run), (status = BAD_REQUEST, body = ProblemDetails),
        (status = UNAUTHORIZED, body = ProblemDetails), (status = INTERNAL_SERVER_ERROR, body = ProblemDetails)))]
pub(super) async fn create_run(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<Json<Run>> {
    runs::validate_run_id(&id).map_err(AppError::BadRequest)?;
    let jobs = job_service(&state)?;
    locked(jobs, move |jobs| Ok(Json(jobs.runs.create(&id)?))).await
}

#[utoipa::path(get, path = "/runs", tag = "runs", security(("bearerAuth" = [])),
    responses((status = OK, body = [Run]), (status = UNAUTHORIZED, body = ProblemDetails),
        (status = INTERNAL_SERVER_ERROR, body = ProblemDetails)))]
pub(super) async fn list_runs(State(state): State<AppState>) -> AppResult<Json<Vec<Run>>> {
    locked(job_service(&state)?, |jobs| Ok(Json(jobs.runs.list()?))).await
}

#[utoipa::path(get, path = "/runs/{id}", tag = "runs", security(("bearerAuth" = [])),
    params(("id" = String, Path)),
    responses((status = OK, body = Run), (status = NOT_FOUND, body = ProblemDetails),
        (status = UNAUTHORIZED, body = ProblemDetails), (status = INTERNAL_SERVER_ERROR, body = ProblemDetails)))]
pub(super) async fn get_run(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<Json<Run>> {
    locked(job_service(&state)?, move |jobs| {
        Ok(Json(jobs.runs.get(&id)?.ok_or(AppError::NotFound)?))
    })
    .await
}

fn owned_domain(uri: &str, resource: &RunResource) -> AppResult<Option<Domain>> {
    let connection = Connect::open(Some(uri)).map_err(|e| AppError::Internal(e.into()))?;
    match Domain::lookup_by_name(&connection, &resource.id) {
        Ok(domain) => {
            if domain
                .get_uuid_string()
                .map_err(|e| AppError::Internal(e.into()))?
                != resource.token
            {
                return Err(AppError::Conflict(format!(
                    "VM identity mismatch: {}",
                    resource.id
                )));
            }
            Ok(Some(domain))
        }
        Err(e) if e.code() == virt::error::ErrorNumber::NoDomain => Ok(None),
        Err(e) => Err(AppError::Internal(e.into())),
    }
}

fn check_resource(jobs: &JobService, uri: &str, resource: &RunResource) -> AppResult<bool> {
    match resource.kind.as_str() {
        "vm" => Ok(owned_domain(uri, resource)?.is_some()),
        "image" => runs::check_file(jobs.image_root(), resource).map_err(conflict),
        "media" => runs::check_file(jobs.media_root(), resource).map_err(conflict),
        _ => Err(AppError::Internal(anyhow::anyhow!(
            "unknown recorded resource kind"
        ))),
    }
}

#[utoipa::path(post, path = "/runs/{id}/resources", tag = "runs", security(("bearerAuth" = [])),
    params(("id" = String, Path)), request_body = RunCreateRequest,
    responses((status = OK, body = RunResource), (status = BAD_REQUEST, body = ProblemDetails),
        (status = NOT_FOUND, body = ProblemDetails), (status = UNAUTHORIZED, body = ProblemDetails),
        (status = CONFLICT, body = ProblemDetails), (status = PAYLOAD_TOO_LARGE, body = ProblemDetails),
        (status = INTERNAL_SERVER_ERROR, body = ProblemDetails)))]
pub(super) async fn create_resource(
    State(state): State<AppState>,
    Path(id): Path<String>,
    request: std::result::Result<Json<RunCreateRequest>, JsonRejection>,
) -> AppResult<Json<RunResource>> {
    let request = api_json(request)?;
    let jobs = job_service(&state)?;
    let uri = state.connect_uri;
    // Bound seed generation with the same semaphore as the ordinary seed API.
    let iso_permit = if matches!(request, RunCreateRequest::CloudInit { .. }) {
        Some(
            state
                .iso_uploads
                .acquire_owned()
                .await
                .map_err(|e| AppError::Internal(e.into()))?,
        )
    } else {
        None
    };
    locked(jobs, move |jobs| {
        let _iso_permit = iso_permit;
        let (kind, resource_id) = match &request {
            RunCreateRequest::ImageClone {
                source_image_id,
                request,
            } => {
                jobs.validate_existing_image_id(source_image_id)
                    .map_err(AppError::BadRequest)?;
                jobs.validate_image_id(&request.id, crate::config::DiskFormat::Qcow2)
                    .map_err(AppError::BadRequest)?;
                ("image", request.id.clone())
            }
            RunCreateRequest::CloudInit { request } => {
                request.validate().map_err(AppError::BadRequest)?;
                ("media", request.id.clone())
            }
            RunCreateRequest::Vm { request } => {
                runs::validate_run_id(&request.name).map_err(AppError::BadRequest)?;
                ("vm", request.name.clone())
            }
        };
        jobs.runs.get(&id)?.ok_or(AppError::NotFound)?;
        let intent = serde_json::to_string(&request).map_err(|e| AppError::Internal(e.into()))?;
        let mut resource = jobs
            .runs
            .reserve(&id, kind, &resource_id, &intent)
            .map_err(conflict)?;
        if jobs.with_resource_lock(|| check_resource(&jobs, &uri, &resource))? {
            jobs.runs.resource_state(&resource, "ready")?;
            resource.state = "ready".to_string();
            return Ok(Json(resource));
        }
        if resource.state != "creating" {
            return Err(AppError::Conflict(
                "recorded resource is missing; clean up the run instead of recreating it"
                    .to_string(),
            ));
        }
        let root = match kind {
            "image" => Some(jobs.image_root()),
            "media" => Some(jobs.media_root()),
            _ => None,
        };
        if root.is_some_and(|root| runs::pin_path(root, &resource).exists()) {
            return Err(AppError::Conflict(
                "unpublished or removed pinned resource; clean up the run".to_string(),
            ));
        }
        match request {
            RunCreateRequest::ImageClone {
                source_image_id,
                request,
            } => {
                match jobs.clone_image_observed(
                    &source_image_id,
                    &request.id,
                    |path| vm::domains_using_image(&uri, path),
                    |path| runs::pin_file(jobs.image_root(), &resource, path),
                )? {
                    ImageCloneOutcome::Created(_) => {}
                    ImageCloneOutcome::NotFound => return Err(AppError::NotFound),
                    ImageCloneOutcome::InUse(detail) | ImageCloneOutcome::Conflict(detail) => {
                        return Err(AppError::Conflict(detail));
                    }
                }
            }
            RunCreateRequest::CloudInit { request } => {
                match jobs.create_cloud_init_seed_observed(&request, |path| {
                    runs::pin_file(jobs.media_root(), &resource, path)
                })? {
                    IsoPublishOutcome::Created(_) => {}
                    IsoPublishOutcome::Exists => {
                        return Err(AppError::Conflict("media already exists".to_string()));
                    }
                }
            }
            RunCreateRequest::Vm { request } => {
                jobs.with_resource_lock(|| -> AppResult<()> {
                    let image_ids = request
                        .disks
                        .iter()
                        .map(|d| d.image_id.clone())
                        .collect::<Vec<_>>();
                    reject_active_install(&jobs, &request.name, &image_ids)?;
                    reject_writable_backing_images(&jobs, &image_ids)?;
                    let manifest = request.into_manifest(&jobs).map_err(AppError::BadRequest)?;
                    network::ensure_active(&uri, manifest.network.as_deref().unwrap_or_default())
                        .map_err(AppError::BadRequest)?;
                    for attachment in vm::managed_image_attachments(&uri, jobs.image_root())? {
                        if image_ids.contains(&attachment.image_id) {
                            return Err(AppError::Conflict(format!(
                                "image {} is already attached",
                                attachment.image_id
                            )));
                        }
                    }
                    vm::define_new_by_manifest(&uri, manifest, &resource.token)?;
                    Ok(())
                })?;
            }
        }
        if let Some(root) = root {
            std::fs::File::open(root)
                .and_then(|file| file.sync_all())
                .context("failed to sync resource directory")?;
        }
        jobs.runs.resource_state(&resource, "ready")?;
        resource.state = "ready".to_string();
        Ok(Json(resource))
    })
    .await
}

#[utoipa::path(post, path = "/runs/{id}/cleanup", tag = "runs", security(("bearerAuth" = [])),
    params(("id" = String, Path)),
    responses((status = OK, body = Run), (status = NOT_FOUND, body = ProblemDetails),
        (status = UNAUTHORIZED, body = ProblemDetails), (status = CONFLICT, body = ProblemDetails),
        (status = INTERNAL_SERVER_ERROR, body = ProblemDetails)))]
pub(super) async fn cleanup_run(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<Json<Run>> {
    let uri = state.connect_uri.clone();
    locked(job_service(&state)?, move |jobs| {
        let run = jobs.runs.get(&id)?.ok_or(AppError::NotFound)?;
        if run.state == "closed" {
            return Ok(Json(run));
        }
        jobs.runs.run_state(&id, "cleaning")?;
        let mut resources = run
            .resources
            .into_iter()
            .filter(|r| r.state != "deleted")
            .collect::<Vec<_>>();
        // Reverse creation order within each kind; never delete a base before its overlays.
        resources.reverse();
        resources.sort_by_key(|r| match r.kind.as_str() {
            "vm" => 0,
            "media" => 1,
            _ => 2,
        });
        jobs.with_resource_lock(|| -> AppResult<()> {
            for resource in &resources {
                check_resource(&jobs, &uri, resource)?;
            }
            Ok(())
        })?;
        for resource in resources {
            match resource.kind.as_str() {
                "vm" => jobs.with_resource_lock(|| -> AppResult<()> {
                    reject_active_install(&jobs, &resource.id, &[])?;
                    if let Some(domain) = owned_domain(&uri, &resource)? {
                        if domain
                            .is_active()
                            .map_err(|e| AppError::Internal(e.into()))?
                        {
                            domain.destroy().map_err(|e| AppError::Internal(e.into()))?;
                        }
                        domain
                            .undefine()
                            .map_err(|e| AppError::Internal(e.into()))?;
                    }
                    Ok(())
                })?,
                "image" => {
                    match jobs
                        .delete_image_checked(
                            &resource.id,
                            |path| vm::domains_using_image(&uri, path),
                            || runs::check_file(jobs.image_root(), &resource).map(|_| ()),
                        )
                        .map_err(conflict)?
                    {
                        ImageDeleteOutcome::Deleted | ImageDeleteOutcome::NotFound => {}
                        ImageDeleteOutcome::InUse(detail) => {
                            return Err(AppError::Conflict(detail));
                        }
                    }
                    std::fs::File::open(jobs.image_root())
                        .and_then(|file| file.sync_all())
                        .context("failed to sync resource directory")?;
                    runs::unpin_file(jobs.image_root(), &resource)?;
                }
                "media" => {
                    match jobs
                        .delete_iso_checked(
                            &resource.id,
                            |path| vm::domains_using_media(&uri, path),
                            || runs::check_file(jobs.media_root(), &resource).map(|_| ()),
                        )
                        .map_err(conflict)?
                    {
                        IsoDeleteOutcome::Deleted | IsoDeleteOutcome::NotFound => {}
                        IsoDeleteOutcome::InUse(detail) => return Err(AppError::Conflict(detail)),
                    }
                    std::fs::File::open(jobs.media_root())
                        .and_then(|file| file.sync_all())
                        .context("failed to sync resource directory")?;
                    runs::unpin_file(jobs.media_root(), &resource)?;
                }
                _ => return Err(AppError::Internal(anyhow::anyhow!("unknown resource kind"))),
            }
            jobs.runs.resource_state(&resource, "deleted")?;
        }
        jobs.runs.run_state(&id, "closed")?;
        Ok(Json(jobs.runs.get(&id)?.ok_or(AppError::NotFound)?))
    })
    .await
}

#[cfg(test)]
mod tests;
