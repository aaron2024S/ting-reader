use super::*;
use std::io::Write;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;

fn fixture_process(mode: &str) -> Child {
    Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "core::audio::ffmpeg::tests::stream_process_fixture",
            "--nocapture",
        ])
        .env("TING_AUDIO_STREAM_TEST_MODE", mode)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap()
}

#[test]
fn stream_process_fixture() {
    let Ok(mode) = std::env::var("TING_AUDIO_STREAM_TEST_MODE") else {
        return;
    };
    std::io::stdout().write_all(b"encoder-ready\n").unwrap();
    std::io::stdout().flush().unwrap();
    match mode.as_str() {
        "success" => {
            std::thread::sleep(Duration::from_millis(100));
            std::io::stdout().write_all(b"encoder-finished\n").unwrap();
        }
        "failure" => std::process::exit(7),
        "running" => loop {
            std::thread::sleep(Duration::from_secs(1));
        },
        _ => panic!("Unknown encoder fixture mode"),
    }
}

#[tokio::test]
async fn response_stream_keeps_encoder_alive_until_all_output_is_read() {
    let stream = AudioService::output_stream(fixture_process("success")).unwrap();
    // The handler has already returned, but the body may be polled later.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let output = tokio::time::timeout(Duration::from_secs(5), async move {
        futures::pin_mut!(stream);
        let mut output = Vec::new();
        while let Some(chunk) = stream.next().await {
            output.extend_from_slice(&chunk.unwrap());
        }
        output
    })
    .await
    .unwrap();
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("encoder-ready"));
    assert!(output.contains("encoder-finished"));
}

#[tokio::test]
async fn dropping_response_stream_terminates_encoder() {
    let mut child = fixture_process("running");
    let mut stderr = child.stderr.take().unwrap();
    let mut stream = Box::pin(AudioService::output_stream(child).unwrap());
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut output = Vec::new();
        while !String::from_utf8_lossy(&output).contains("encoder-ready") {
            output.extend_from_slice(&stream.next().await.unwrap().unwrap());
        }
    })
    .await
    .unwrap();
    drop(stream);
    // The sleeping fixture keeps stderr open until its process is killed.
    tokio::time::timeout(Duration::from_secs(5), stderr.read_to_end(&mut Vec::new()))
        .await
        .expect("Encoder survived response cancellation")
        .unwrap();
}

#[tokio::test]
async fn encoder_failure_is_reported_as_a_stream_error() {
    let stream = AudioService::output_stream(fixture_process("failure")).unwrap();
    let error = tokio::time::timeout(Duration::from_secs(5), async move {
        futures::pin_mut!(stream);
        while let Some(chunk) = stream.next().await {
            if let Err(error) = chunk {
                return error;
            }
        }
        panic!("Failed encoder returned a successful EOF");
    })
    .await
    .unwrap();
    assert!(error.to_string().contains("FFmpeg exited with"));
    assert!(error.to_string().contains('7'));
}

#[test]
fn binary_paths_are_fixed_beside_the_service_executable() {
    let executable = Path::new("root/bin/ting-reader");
    assert_eq!(
        bundled_path(executable, "ffmpeg").unwrap(),
        Path::new("root/bin/bin").join(format!("ffmpeg{}", std::env::consts::EXE_SUFFIX))
    );
}

#[test]
fn process_factories_use_the_bundled_tool_names() {
    let executable = Path::new("root/ting-reader.exe");
    let ffmpeg = bundled_path(executable, "ffmpeg").unwrap();
    let ffprobe = bundled_path(executable, "ffprobe").unwrap();
    assert!(ffmpeg.ends_with(format!("ffmpeg{}", std::env::consts::EXE_SUFFIX)));
    assert!(ffprobe.ends_with(format!("ffprobe{}", std::env::consts::EXE_SUFFIX)));
}
