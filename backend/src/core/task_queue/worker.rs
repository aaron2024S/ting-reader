//! Worker scheduling, shutdown and timed execution/retry lifecycle.

use super::*;

impl TaskQueue {
    /// Start the task executor
    pub async fn start(self: Arc<Self>) {
        info!(
            message_key = "task.executor.started",
            "Task queue executor started"
        );

        let mut shutdown_rx = {
            let mut guard = self.shutdown_rx.write().await;
            // Take ownership of the receiver
            std::mem::replace(&mut *guard, mpsc::channel(1).1)
        };

        loop {
            tokio::select! {
                _ = shutdown_rx.recv() => {
                    info!("Task queue executor shutting down");
                    break;
                }
                _ = self.process_next_task() => {}
            }
        }
    }

    /// Shutdown the task queue
    pub async fn shutdown(&self) {
        let _ = self.shutdown_tx.send(()).await;
    }

    /// Process the next task in the queue
    async fn process_next_task(self: &Arc<Self>) {
        // Get next task from queue
        let task = {
            let mut queue = self.queue.write().await;
            queue.pop().map(|pt| pt.task)
        };

        if let Some(task) = task {
            // Acquire semaphore permit
            let permit = self.semaphore.clone().acquire_owned().await;

            match permit {
                Ok(permit) => {
                    let self_clone = Arc::clone(self);

                    // Spawn task execution
                    tokio::spawn(async move {
                        self_clone.execute_task(task).await;
                        drop(permit);
                    });
                }
                Err(e) => {
                    error!(error = %e, "Failed to acquire semaphore permit");
                }
            }
        } else {
            // No tasks available, wait a bit
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Execute a task
    async fn execute_task(&self, mut task: Task) {
        let task_id = task.id.clone();

        info!(
            message_key = "task.executing",
            message_params = %serde_json::json!({
                "task_id": task_id,
                "task_name": task.name,
            }),
            task_id = %task_id,
            task_name = %task.name,
            "Executing task"
        );

        // Update status to running
        task.status = TaskStatus::Running;
        if let Err(e) = self.update_task_status(&task).await {
            error!(task_id = %task_id, error = %e, "Failed to update task status");
        }

        // Execute with timeout
        let result = timeout(task.timeout, self.run_task(&task)).await;

        match result {
            Ok(Ok(())) => {
                // Task completed successfully
                task.status = TaskStatus::Completed;
                task.error = None;

                if let Err(e) = self.update_task_status(&task).await {
                    error!(task_id = %task_id, error = %e, "Failed to update task status");
                }

                info!(
                    message_key = "task.completed",
                    message_params = %serde_json::json!({ "task_id": task_id }),
                    task_id = %task_id,
                    "Task completed successfully"
                );
            }
            Ok(Err(e)) => {
                // Task failed
                error!(
                    message_key = "task.failed",
                    message_params = %serde_json::json!({
                        "task_id": task_id,
                        "error": e.to_string(),
                    }),
                    task_id = %task_id,
                    error = %e,
                    "Task execution failed"
                );

                task.retries += 1;
                task.error = Some(e.to_string());

                // Check if we should retry
                if task.retries < task.retry_policy.max_retries {
                    let delay = task.retry_policy.backoff.calculate_delay(task.retries);

                    warn!(
                        message_key = "task.retrying",
                        message_params = %serde_json::json!({
                            "task_id": task_id,
                            "retry": task.retries,
                            "max_retries": task.retry_policy.max_retries,
                            "delay_secs": delay.as_secs(),
                        }),
                        task_id = %task_id,
                        retry = task.retries,
                        max_retries = task.retry_policy.max_retries,
                        delay_secs = delay.as_secs(),
                        "Retrying task"
                    );

                    // Wait before retry
                    tokio::time::sleep(delay).await;

                    // Re-submit task
                    task.status = TaskStatus::Queued;
                    if let Err(e) = self.update_task_status(&task).await {
                        error!(task_id = %task_id, error = %e, "Failed to update task status");
                    }

                    let mut queue = self.queue.write().await;
                    queue.push(PriorityTask { task });
                } else {
                    // Max retries exceeded
                    task.status = TaskStatus::Failed;
                    if let Err(e) = self.update_task_status(&task).await {
                        error!(task_id = %task_id, error = %e, "Failed to update task status");
                    }

                    error!(
                        message_key = "task.max_retries_failed",
                        message_params = %serde_json::json!({
                            "task_id": task_id,
                            "retries": task.retries,
                        }),
                        task_id = %task_id,
                        retries = task.retries,
                        "Task failed after max retries"
                    );
                }
            }
            Err(_) => {
                // Timeout
                error!(
                    message_key = "task.timed_out",
                    message_params = %serde_json::json!({
                        "task_id": task_id,
                        "timeout_secs": task.timeout.as_secs(),
                    }),
                    task_id = %task_id,
                    timeout_secs = task.timeout.as_secs(),
                    "Task execution timed out"
                );

                task.status = TaskStatus::Failed;
                task.error = Some(format!(
                    "Task timed out after {} seconds",
                    task.timeout.as_secs()
                ));

                if let Err(e) = self.update_task_status(&task).await {
                    error!(task_id = %task_id, error = %e, "Failed to update task status");
                }
            }
        }
    }
}
