//! Task inspection and administrative control endpoints.

use crate::api::models::{
    BatchDeleteTasksRequest, BatchDeleteTasksResponse, CancelTaskResponse, ClearTasksQuery,
    ClearTasksResponse, DeleteTaskResponse, TaskDetailResponse, TaskInfoResponse, TasksQuery,
};
use crate::api::state::AppState;
use crate::core::app::error::Result;
use axum::{
    Json,
    extract::{Path, Query, State},
    response::IntoResponse,
};

/// Handler for GET /api/v1/tasks - List all tasks
pub async fn list_tasks(
    State(state): State<AppState>,
    user: crate::auth::middleware::AuthUser,
    Query(query): Query<TasksQuery>,
) -> Result<impl IntoResponse> {
    crate::api::require_admin(&user)?;

    let (task_records, _total) = state
        .task_queue
        .list_tasks_with_filters(
            query.status,
            query.page,
            query.page_size,
            query.sort_by,
            query.sort_order,
        )
        .await?;

    let tasks: Vec<TaskInfoResponse> = task_records
        .into_iter()
        .map(|record| TaskInfoResponse {
            id: record.id,
            task_type: record.task_type,
            status: record.status,
            payload: record.payload,
            message: record.message,
            message_key: record.message_key,
            message_params: record.message_params,
            error: record.error,
            retries: record.retries,
            max_retries: record.max_retries,
            created_at: record.created_at,
            started_at: None,  // TODO: Add started_at to TaskRecord
            finished_at: None, // TODO: Add finished_at to TaskRecord
        })
        .collect();

    Ok(Json(tasks))
}

/// Handler for GET /api/v1/tasks/:id - Get task details
pub async fn get_task(
    State(state): State<AppState>,
    user: crate::auth::middleware::AuthUser,
    Path(id): Path<String>,
) -> Result<impl IntoResponse> {
    crate::api::require_admin(&user)?;

    let task_record = state.task_queue.get_task(&id).await?;

    let payload = if let Some(ref payload_str) = task_record.payload {
        serde_json::from_str(payload_str).ok()
    } else {
        None
    };

    let result = if let Some(ref message_str) = task_record.message {
        serde_json::from_str(message_str).ok()
    } else {
        None
    };

    let response = TaskDetailResponse {
        id: task_record.id,
        task_type: task_record.task_type,
        status: task_record.status,
        payload,
        message: task_record.message,
        message_key: task_record.message_key,
        message_params: task_record.message_params,
        result,
        error: task_record.error,
        retries: task_record.retries,
        max_retries: task_record.max_retries,
        created_at: task_record.created_at,
        started_at: None,
        finished_at: None,
    };

    Ok(Json(response))
}

/// Handler for POST /api/v1/tasks/:id/cancel - Cancel a task
pub async fn cancel_task(
    State(state): State<AppState>,
    user: crate::auth::middleware::AuthUser,
    Path(id): Path<String>,
) -> Result<impl IntoResponse> {
    crate::api::require_admin(&user)?;

    state.task_queue.cancel(&id).await?;

    Ok(Json(CancelTaskResponse {
        message: format!("Task {} cancelled successfully", id),
    }))
}

/// Handler for DELETE /api/v1/tasks/:id - Delete a task
pub async fn delete_task(
    State(state): State<AppState>,
    user: crate::auth::middleware::AuthUser,
    Path(id): Path<String>,
) -> Result<impl IntoResponse> {
    crate::api::require_admin(&user)?;

    state.task_queue.delete_task(&id).await?;

    Ok(Json(DeleteTaskResponse {
        message: format!("Task {} deleted successfully", id),
    }))
}

/// Handler for DELETE /api/v1/tasks - Clear tasks
pub async fn clear_tasks(
    State(state): State<AppState>,
    user: crate::auth::middleware::AuthUser,
    Query(query): Query<ClearTasksQuery>,
) -> Result<impl IntoResponse> {
    crate::api::require_admin(&user)?;

    let count = state.task_queue.clear_tasks(query.status).await?;

    Ok(Json(ClearTasksResponse {
        message: format!("Cleared {} tasks", count),
        count,
    }))
}

/// Handler for POST /api/v1/tasks/batch-delete - Batch delete tasks
pub async fn batch_delete_tasks(
    State(state): State<AppState>,
    user: crate::auth::middleware::AuthUser,
    Json(req): Json<BatchDeleteTasksRequest>,
) -> Result<impl IntoResponse> {
    crate::api::require_admin(&user)?;

    let count = state.task_queue.delete_tasks(req.ids).await?;

    Ok(Json(BatchDeleteTasksResponse {
        message: format!("Deleted {} tasks", count),
        count,
    }))
}
