use protocol::{
    decode_json, read_frame, write_frame, write_json_frame, FileEntry, HelloAckPayload,
    HelloPayload, LogPayload, ManifestPayload, MsgType, NeedPayload, ResultPayload, RunPayload,
    CURRENT_PROTOCOL_VERSION,
};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::tempdir;
use tokio::net::{TcpListener, TcpStream};

async fn spawn_test_server(token: Option<String>, workdir: PathBuf) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();

    let handle = tokio::spawn(async move {
        let _ = fhd::run_server(listener, token, workdir, None).await;
    });

    (addr, handle)
}

async fn client_roundtrip(
    server_addr: &str,
    token: &str,
    project_name: &str,
    project_dir: &Path,
    cmd_argv: &[String],
) -> (NeedPayload, String, i32) {
    let mut stream = TcpStream::connect(server_addr).await.unwrap();

    // 1. HELLO
    let hello = HelloPayload {
        token: token.to_string(),
        project: project_name.to_string(),
        protocol_version: CURRENT_PROTOCOL_VERSION,
    };
    write_json_frame(&mut stream, MsgType::Hello, &hello).await.unwrap();

    // 2. HELLO_ACK
    let (msg_type, payload) = read_frame(&mut stream).await.unwrap();
    assert_eq!(msg_type, MsgType::HelloAck);
    let ack: HelloAckPayload = decode_json(&payload).unwrap();
    assert!(ack.ok);

    // 3. Scan & MANIFEST
    let scanned = fileset::scan(project_dir, &[]).unwrap();
    let manifest_files: Vec<FileEntry> = scanned
        .values()
        .map(|m| FileEntry {
            path: m.path.clone(),
            hash: m.hash.clone(),
            size: m.size,
            mode: m.mode,
        })
        .collect();
    let manifest = ManifestPayload { files: manifest_files };
    write_json_frame(&mut stream, MsgType::Manifest, &manifest).await.unwrap();

    // 4. NEED
    let (msg_type, payload) = read_frame(&mut stream).await.unwrap();
    assert_eq!(msg_type, MsgType::Need);
    let need: NeedPayload = decode_json(&payload).unwrap();

    // 5. FILES
    if need.want.is_empty() {
        write_frame(&mut stream, MsgType::Files, &[]).await.unwrap();
    } else {
        let tar_gz = fileset::pack_tar(project_dir, &need.want).unwrap();
        write_frame(&mut stream, MsgType::Files, &tar_gz).await.unwrap();
    }

    // 6. RUN
    let run = RunPayload {
        argv: cmd_argv.to_vec(),
        outputs: None,
        cwd: None,
        template: None,
        no_cache: false,
    };
    write_json_frame(&mut stream, MsgType::Run, &run).await.unwrap();

    // 7. Stream LOGs and RESULT
    let mut output = String::new();
    let exit_code;

    loop {
        let (msg_type, payload) = read_frame(&mut stream).await.unwrap();
        match msg_type {
            MsgType::Log => {
                let log: LogPayload = decode_json(&payload).unwrap();
                output.push_str(&log.data);
            }
            MsgType::Result => {
                let res: ResultPayload = decode_json(&payload).unwrap();
                exit_code = res.exit_code;
                break;
            }
            other => panic!("Unexpected frame: {:?}", other),
        }
    }

    (need, output, exit_code)
}

#[tokio::test]
async fn test_e2e_persistent_workspace_and_delta_sync() {
    let token = "test-token-delta".to_string();
    let workdir = tempdir().unwrap();
    let (server_addr, _handle) = spawn_test_server(Some(token.clone()), workdir.path().to_path_buf()).await;

    let project_dir = tempdir().unwrap();
    let file1 = project_dir.path().join("src/lib.rs");
    fs::create_dir_all(file1.parent().unwrap()).unwrap();
    fs::write(&file1, b"pub fn first() {}").unwrap();

    let project_name = "delta-project";

    // --- RUN 1: Initial upload on clean workspace ---
    let (need1, out1, code1) = client_roundtrip(
        &server_addr,
        &token,
        project_name,
        project_dir.path(),
        &["echo".into(), "run1-done".into()],
    )
    .await;

    assert_eq!(code1, 0);
    assert!(out1.contains("run1-done"));
    assert_eq!(need1.want, vec!["src/lib.rs"]);
    assert!(need1.delete_extraneous.is_empty());

    // Verify workspace exists on server
    let resolved_ws = workspace::resolve_workspace_dir(workdir.path(), project_name);
    assert!(resolved_ws.exists());
    assert!(resolved_ws.join("src/lib.rs").exists());

    // Inject remote dependency (e.g. node_modules/react or target/) on server
    let remote_dep = resolved_ws.join("node_modules/fake-lib/index.js");
    fs::create_dir_all(remote_dep.parent().unwrap()).unwrap();
    fs::write(&remote_dep, b"// remote cached dep").unwrap();

    // --- RUN 2: Second run without any local file changes ---
    let (need2, out2, code2) = client_roundtrip(
        &server_addr,
        &token,
        project_name,
        project_dir.path(),
        &["echo".into(), "run2-done".into()],
    )
    .await;

    assert_eq!(code2, 0);
    assert!(out2.contains("run2-done"));
    // CRITICAL: want must be completely empty!
    assert!(need2.want.is_empty(), "Expected 0 files to transfer, got: {:?}", need2.want);
    assert!(need2.delete_extraneous.is_empty());

    // CRITICAL: Section 5.1 deletion safety - remote cached dep must STILL exist!
    assert!(remote_dep.exists(), "Remote dependency in node_modules was deleted!");

    // --- RUN 3: Add new file locally, delete old file ---
    fs::remove_file(&file1).unwrap();
    let file2 = project_dir.path().join("src/new.rs");
    fs::write(&file2, b"pub fn new_code() {}").unwrap();

    let (need3, out3, code3) = client_roundtrip(
        &server_addr,
        &token,
        project_name,
        project_dir.path(),
        &["echo".into(), "run3-done".into()],
    )
    .await;

    assert_eq!(code3, 0);
    assert!(out3.contains("run3-done"));
    // Only new.rs should be requested
    assert_eq!(need3.want, vec!["src/new.rs"]);
    // Deleted file should be flagged and removed
    assert_eq!(need3.delete_extraneous, vec!["src/lib.rs"]);
    assert!(!resolved_ws.join("src/lib.rs").exists(), "Old file was not pruned from workspace");
    assert!(resolved_ws.join("src/new.rs").exists(), "New file was not placed in workspace");

    // Remote dep still intact
    assert!(remote_dep.exists());
}

#[tokio::test]
async fn test_e2e_command_failure_exit_code() {
    let workdir = tempdir().unwrap();
    let (server_addr, _server_handle) = spawn_test_server(None, workdir.path().to_path_buf()).await;

    let project_dir = tempdir().unwrap();
    let (_need, _out, code) = client_roundtrip(
        &server_addr,
        "",
        "fail-test",
        project_dir.path(),
        &["exit".into(), "42".into()],
    )
    .await;

    assert_eq!(code, 42);
}

#[tokio::test]
async fn test_e2e_invalid_token_rejection() {
    let workdir = tempdir().unwrap();
    let (server_addr, _server_handle) =
        spawn_test_server(Some("super-secret".into()), workdir.path().to_path_buf()).await;

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
