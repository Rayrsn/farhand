use protocol::{
    decode_json, read_frame, write_frame, write_json_frame, HelloAckPayload, HelloPayload,
    LogPayload, MsgType, ResultPayload, RunPayload, CURRENT_PROTOCOL_VERSION,
};
use std::fs;
use tempfile::tempdir;
use tokio::net::{TcpListener, TcpStream};

async fn spawn_test_server(token: Option<String>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();

    let handle = tokio::spawn(async move {
        let _ = fhd::run_server(listener, token, None).await;
    });

    (addr, handle)
}

#[tokio::test]
async fn test_e2e_successful_pipeline_execution() {
    let secret_token = "valid-secret-123".to_string();
    let (server_addr, _server_handle) = spawn_test_server(Some(secret_token.clone())).await;

    // 1. Create a dummy project
    let project_dir = tempdir().unwrap();
    fs::write(project_dir.path().join("file.txt"), b"sample project file").unwrap();

    // 2. Connect client
    let mut stream = TcpStream::connect(&server_addr).await.unwrap();

    // 3. Send HELLO
    let hello = HelloPayload {
        token: secret_token,
        project: "test-project".into(),
        protocol_version: CURRENT_PROTOCOL_VERSION,
    };
    write_json_frame(&mut stream, MsgType::Hello, &hello).await.unwrap();

    // 4. Expect HELLO_ACK
    let (msg_type, payload) = read_frame(&mut stream).await.unwrap();
    assert_eq!(msg_type, MsgType::HelloAck);
    let ack: HelloAckPayload = decode_json(&payload).unwrap();
    assert!(ack.ok);
    assert!(ack.error.is_none());

    // 5. Pack and send FILES
    let paths = vec!["file.txt".to_string()];
    let tar_gz = fileset::pack_tar(project_dir.path(), &paths).unwrap();
    write_frame(&mut stream, MsgType::Files, &tar_gz).await.unwrap();

    // 6. Send RUN
    let run = RunPayload {
        argv: vec!["echo".into(), "hello-from-remote-agent".into()],
        outputs: None,
        cwd: None,
        template: None,
        no_cache: false,
    };
    write_json_frame(&mut stream, MsgType::Run, &run).await.unwrap();

    // 7. Receive LOGs and RESULT
    let mut received_output = String::new();
    let exit_code;
    loop {
        let (msg_type, payload) = read_frame(&mut stream).await.unwrap();
        match msg_type {
            MsgType::Log => {
                let log: LogPayload = decode_json(&payload).unwrap();
                received_output.push_str(&log.data);
            }
            MsgType::Result => {
                let res: ResultPayload = decode_json(&payload).unwrap();
                exit_code = res.exit_code;
                break;
            }
            other => panic!("Unexpected frame during execution: {:?}", other),
        }
    }

    assert_eq!(exit_code, 0);
    assert!(
        received_output.contains("hello-from-remote-agent"),
        "Expected output not found in: {}",
        received_output
    );
}

#[tokio::test]
async fn test_e2e_command_failure_exit_code() {
    let (server_addr, _server_handle) = spawn_test_server(None).await;

    let _project_dir = tempdir().unwrap();
    let mut stream = TcpStream::connect(&server_addr).await.unwrap();

    let hello = HelloPayload {
        token: "".into(),
        project: "test-fail".into(),
        protocol_version: CURRENT_PROTOCOL_VERSION,
    };
    write_json_frame(&mut stream, MsgType::Hello, &hello).await.unwrap();

    let (msg_type, payload) = read_frame(&mut stream).await.unwrap();
    assert_eq!(msg_type, MsgType::HelloAck);
    let ack: HelloAckPayload = decode_json(&payload).unwrap();
    assert!(ack.ok);

    // Send empty files archive
    write_frame(&mut stream, MsgType::Files, &[]).await.unwrap();

    // Run command exiting with code 42
    let cmd_argv = vec!["exit".into(), "42".into()];

    let run = RunPayload {
        argv: cmd_argv,
        outputs: None,
        cwd: None,
        template: None,
        no_cache: false,
    };
    write_json_frame(&mut stream, MsgType::Run, &run).await.unwrap();

    let exit_code;
    loop {
        let (msg_type, payload) = read_frame(&mut stream).await.unwrap();
        match msg_type {
            MsgType::Log => {}
            MsgType::Result => {
                let res: ResultPayload = decode_json(&payload).unwrap();
                exit_code = res.exit_code;
                break;
            }
            _ => {}
        }
    }

    assert_eq!(exit_code, 42);
}

#[tokio::test]
async fn test_e2e_invalid_token_rejection() {
    let (server_addr, _server_handle) = spawn_test_server(Some("super-secret".into())).await;

    let mut stream = TcpStream::connect(&server_addr).await.unwrap();

    let hello = HelloPayload {
        token: "wrong-token".into(),
        project: "test-auth".into(),
        protocol_version: CURRENT_PROTOCOL_VERSION,
    };
    write_json_frame(&mut stream, MsgType::Hello, &hello).await.unwrap();

    let (msg_type, payload) = read_frame(&mut stream).await.unwrap();
    assert_eq!(msg_type, MsgType::HelloAck);
    let ack: HelloAckPayload = decode_json(&payload).unwrap();
    assert!(!ack.ok);
    assert!(ack.error.unwrap().contains("Unauthorized"));
}
