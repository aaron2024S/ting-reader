use super::*;
use futures::StreamExt;
use std::sync::Arc;
use std::time::Duration;

async fn pending_producer(tasks: &mut TranscodeTasks) -> std::sync::Weak<()> {
    let resource = Arc::new(());
    let weak = Arc::downgrade(&resource);
    let (started, received) = tokio::sync::oneshot::channel();
    tasks.0.push(tokio::spawn(async move {
        let _resource = resource;
        let _ = started.send(());
        std::future::pending::<()>().await;
    }));
    received.await.unwrap();
    weak
}

async fn assert_released(resource: std::sync::Weak<()>) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while resource.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Transcode response retained its input task after cancellation");
}

#[tokio::test]
async fn dropping_unpolled_response_releases_blocked_input() {
    let mut tasks = TranscodeTasks::default();
    let resource = pending_producer(&mut tasks).await;
    let body = Body::from_stream(TranscodeOutput {
        output: Box::pin(futures::stream::pending::<std::io::Result<Bytes>>()),
        tasks,
    });
    drop(body);
    assert_released(resource).await;
}

#[tokio::test]
async fn successful_eof_releases_background_tasks() {
    let mut tasks = TranscodeTasks::default();
    let resource = pending_producer(&mut tasks).await;
    let mut stream = TranscodeOutput {
        output: Box::pin(futures::stream::empty::<std::io::Result<Bytes>>()),
        tasks,
    };
    assert!(stream.next().await.is_none());
    assert_released(resource).await;
}

#[tokio::test]
async fn encoder_error_releases_background_tasks() {
    let mut tasks = TranscodeTasks::default();
    let resource = pending_producer(&mut tasks).await;
    let mut stream = TranscodeOutput {
        output: Box::pin(futures::stream::once(async {
            Err(std::io::Error::other("encoder failed"))
        })),
        tasks,
    };
    assert!(stream.next().await.unwrap().is_err());
    assert_released(resource).await;
}

#[tokio::test]
async fn dropping_handler_resources_releases_tasks_before_a_body_exists() {
    let mut tasks = TranscodeTasks::default();
    let resource = pending_producer(&mut tasks).await;
    drop(tasks);
    assert_released(resource).await;
}
