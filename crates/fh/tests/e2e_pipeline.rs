use protocol::{
    decode_json, read_frame, write_frame, write_json_frame, FileEntry, HelloAckPayload,
    HelloPayload, LogPayload, ManifestPayload, MsgType, NeedPayload, ResultPayload, RunPayload,
    CURRENT_PROTOCOL_VERSION,
};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::tempdir;
use tokio::net::{TcpListener, TcpStream};

async fn spawn_test_server(
    token: Option<String>,
    workdir: PathBuf,
) -> (String, tokio::task::JoinHandle<()>) {
    spawn_test_server_with_concurrency(token, workdir, None).await
}

async fn spawn_test_server_with_tags(
    token: Option<String>,
    workdir: PathBuf,
    max_concurrent_runs: Option<usize>,
    tags: Vec<String>,
) -> (String, tokio::task::JoinHandle<()>) {
    spawn_test_server_full(token, workdir, max_concurrent_runs, tags, Some(0)).await
}

async fn spawn_test_server_full(
    token: Option<String>,
    workdir: PathBuf,
    max_concurrent_runs: Option<usize>,
    tags: Vec<String>,
    min_disk_bytes: Option<u64>,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();

    let handle = tokio::spawn(async move {
        let _ = fhd::run_server(
            listener,
            token,
            workdir,
            None,
            max_concurrent_runs,
            tags,
            min_disk_bytes,
            None,
            false,
            None,
            None,
            None,
            None,
        )
        .await;
    });

    (addr, handle)
}

async fn spawn_agent_tls(
    workdir: PathBuf,
    token: Option<String>,
    tls_acceptor: protocol::TlsAcceptor,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();

    let handle = tokio::spawn(async move {
        let _ = fhd::run_server(
            listener,
            token,
            workdir,
            None,
            None,
            Vec::new(),
            None,
            None,
            false,
            Some(tls_acceptor),
            None,
            None,
            None,
        )
        .await;
    });

    (addr, handle)
}

async fn spawn_test_server_with_concurrency(
    token: Option<String>,
    workdir: PathBuf,
    max_concurrent_runs: Option<usize>,
) -> (String, tokio::task::JoinHandle<()>) {
    spawn_test_server_with_tags(token, workdir, max_concurrent_runs, vec![]).await
}

async fn client_roundtrip(
    server_addr: &str,
    token: &str,
    project_name: &str,
    project_dir: &Path,
    cmd_argv: &[String],
) -> (NeedPayload, String, i32) {
    client_roundtrip_with_options(
        server_addr,
        token,
        project_name,
        project_dir,
        cmd_argv,
        None,
        false,
    )
    .await
}

async fn client_roundtrip_with_options(
    server_addr: &str,
    token: &str,
    project_name: &str,
    project_dir: &Path,
    cmd_argv: &[String],
    template: Option<String>,
    no_cache: bool,
) -> (NeedPayload, String, i32) {
    let mut stream = TcpStream::connect(server_addr).await.unwrap();

    // 1. HELLO
    let hello = HelloPayload {
        token: token.to_string(),
        project: project_name.to_string(),
        protocol_version: CURRENT_PROTOCOL_VERSION,
        compressions: None,
    };
    write_json_frame(&mut stream, MsgType::Hello, &hello)
        .await
        .unwrap();

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
    let manifest = ManifestPayload {
        files: manifest_files,
    };
    write_json_frame(&mut stream, MsgType::Manifest, &manifest)
        .await
        .unwrap();

    // 4. NEED (may receive QUEUED first)
    let need: NeedPayload = loop {
        let (msg_type, payload) = read_frame(&mut stream).await.unwrap();
        if msg_type == MsgType::Queued {
            continue;
        }
        assert_eq!(msg_type, MsgType::Need);
        break decode_json(&payload).unwrap();
    };

    // 5. FILES
    if need.want.is_empty() {
        write_frame(&mut stream, MsgType::Files, &[]).await.unwrap();
    } else {
        let tar_gz = fileset::pack_tar(project_dir, &need.want).unwrap();
        write_frame(&mut stream, MsgType::Files, &tar_gz)
            .await
            .unwrap();
    }

    // 6. RUN
    let run = RunPayload {
        argv: cmd_argv.to_vec(),
        outputs: None,
        cwd: None,
        template,
        no_cache,
        env: None,
        toolchain: None,
        tty: false,
        cols: None,
        rows: None,
        raw_stdio: None,
    };
    write_json_frame(&mut stream, MsgType::Run, &run)
        .await
        .unwrap();

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
    let (server_addr, _handle) =
        spawn_test_server(Some(token.clone()), workdir.path().to_path_buf()).await;

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
    assert!(
        need2.want.is_empty(),
        "Expected 0 files to transfer, got: {:?}",
        need2.want
    );
    assert!(need2.delete_extraneous.is_empty());

    // CRITICAL: Section 5.1 deletion safety - remote cached dep must STILL exist!
    assert!(
        remote_dep.exists(),
        "Remote dependency in node_modules was deleted!"
    );

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
    assert!(
        !resolved_ws.join("src/lib.rs").exists(),
        "Old file was not pruned from workspace"
    );
    assert!(
        resolved_ws.join("src/new.rs").exists(),
        "New file was not placed in workspace"
    );

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
        compressions: None,
    };
    write_json_frame(&mut stream, MsgType::Hello, &hello)
        .await
        .unwrap();

    let (msg_type, payload) = read_frame(&mut stream).await.unwrap();
    assert_eq!(msg_type, MsgType::HelloAck);
    let ack: HelloAckPayload = decode_json(&payload).unwrap();
    assert!(!ack.ok);
    assert!(ack.error.unwrap().contains("Unauthorized"));
}

async fn client_roundtrip_with_artifacts(
    server_addr: &str,
    token: &str,
    project_name: &str,
    project_dir: &Path,
    cmd_argv: &[String],
    outputs: Option<Vec<String>>,
    out_dir: &Path,
) -> (i32, bool) {
    let mut stream = TcpStream::connect(server_addr).await.unwrap();

    let hello = HelloPayload {
        token: token.to_string(),
        project: project_name.to_string(),
        protocol_version: CURRENT_PROTOCOL_VERSION,
        compressions: None,
    };
    write_json_frame(&mut stream, MsgType::Hello, &hello)
        .await
        .unwrap();

    let (msg_type, payload) = read_frame(&mut stream).await.unwrap();
    assert_eq!(msg_type, MsgType::HelloAck);
    let ack: HelloAckPayload = decode_json(&payload).unwrap();
    assert!(ack.ok);

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
    let manifest = ManifestPayload {
        files: manifest_files,
    };
    write_json_frame(&mut stream, MsgType::Manifest, &manifest)
        .await
        .unwrap();

    let need: NeedPayload = loop {
        let (msg_type, payload) = read_frame(&mut stream).await.unwrap();
        if msg_type == MsgType::Queued {
            continue;
        }
        assert_eq!(msg_type, MsgType::Need);
        break decode_json(&payload).unwrap();
    };

    if need.want.is_empty() {
        write_frame(&mut stream, MsgType::Files, &[]).await.unwrap();
    } else {
        let tar_gz = fileset::pack_tar(project_dir, &need.want).unwrap();
        write_frame(&mut stream, MsgType::Files, &tar_gz)
            .await
            .unwrap();
    }

    let run = RunPayload {
        argv: cmd_argv.to_vec(),
        outputs,
        cwd: None,
        template: None,
        no_cache: false,
        env: None,
        toolchain: None,
        tty: false,
        cols: None,
        rows: None,
        raw_stdio: None,
    };
    write_json_frame(&mut stream, MsgType::Run, &run)
        .await
        .unwrap();

    let mut exit_code = 1;
    let mut got_artifacts = false;

    loop {
        let (msg_type, payload) = match read_frame(&mut stream).await {
            Ok(f) => f,
            Err(protocol::FrameError::UnexpectedEof) => break,
            Err(e) => panic!("Connection error: {}", e),
        };
        match msg_type {
            MsgType::Log => {}
            MsgType::Result => {
                let res: ResultPayload = decode_json(&payload).unwrap();
                exit_code = res.exit_code;
                if exit_code != 0 {
                    break;
                }
            }
            MsgType::Artifacts => {
                fileset::unpack_tar(out_dir, &payload).unwrap();
                got_artifacts = true;
                break;
            }
            other => panic!("Unexpected frame: {:?}", other),
        }
    }

    (exit_code, got_artifacts)
}

#[tokio::test]
async fn test_e2e_artifact_retrieval_explicit() {
    let workdir = tempdir().unwrap();
    let (server_addr, _server_handle) = spawn_test_server(None, workdir.path().to_path_buf()).await;

    let project_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();

    let build_cmd = if cfg!(windows) {
        vec![
            "cmd.exe".to_string(),
            "/C".to_string(),
            "mkdir dist\\assets 2>nul & echo export const v = 1; > dist\\bundle.js & echo body {} > dist\\assets\\app.css".to_string(),
        ]
    } else {
        vec![
            "sh".to_string(),
            "-c".to_string(),
            "mkdir -p dist/assets && echo 'export const v = 1;' > dist/bundle.js && echo 'body {}' > dist/assets/app.css".to_string(),
        ]
    };

    let (exit_code, got_artifacts) = client_roundtrip_with_artifacts(
        &server_addr,
        "",
        "artifact-explicit",
        project_dir.path(),
        &build_cmd,
        Some(vec!["dist".to_string()]),
        out_dir.path(),
    )
    .await;

    assert_eq!(exit_code, 0);
    assert!(got_artifacts);
    assert!(out_dir.path().join("dist/bundle.js").exists());
    assert!(out_dir.path().join("dist/assets/app.css").exists());
    let content = fs::read_to_string(out_dir.path().join("dist/bundle.js")).unwrap();
    assert_eq!(content.trim(), "export const v = 1;");
}

#[tokio::test]
async fn test_e2e_artifact_retrieval_preset_fallback() {
    let workdir = tempdir().unwrap();
    let (server_addr, _server_handle) = spawn_test_server(None, workdir.path().to_path_buf()).await;

    let project_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();

    // Create Cargo.toml in project to trigger "rust" preset
    fs::write(
        project_dir.path().join("Cargo.toml"),
        "[package]\nname = \"test\"\n",
    )
    .unwrap();

    let build_cmd = if cfg!(windows) {
        vec![
            "cmd.exe".to_string(),
            "/C".to_string(),
            "mkdir target\\release 2>nul & echo binary_payload > target\\release\\my-bin"
                .to_string(),
        ]
    } else {
        vec![
            "sh".to_string(),
            "-c".to_string(),
            "mkdir -p target/release && echo 'binary_payload' > target/release/my-bin".to_string(),
        ]
    };

    // None outputs -> should auto-detect target/release via rust preset
    let (exit_code, got_artifacts) = client_roundtrip_with_artifacts(
        &server_addr,
        "",
        "artifact-preset",
        project_dir.path(),
        &build_cmd,
        None,
        out_dir.path(),
    )
    .await;

    assert_eq!(exit_code, 0);
    assert!(got_artifacts);
    assert!(out_dir.path().join("target/release/my-bin").exists());
    let content = fs::read_to_string(out_dir.path().join("target/release/my-bin")).unwrap();
    assert_eq!(content.trim(), "binary_payload");
}

#[tokio::test]
async fn test_e2e_no_artifacts_on_command_failure() {
    let workdir = tempdir().unwrap();
    let (server_addr, _server_handle) = spawn_test_server(None, workdir.path().to_path_buf()).await;

    let project_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();

    let failing_cmd = if cfg!(windows) {
        vec![
            "cmd.exe".to_string(),
            "/C".to_string(),
            "mkdir dist 2>nul & echo partial > dist\\partial.txt & exit 7".to_string(),
        ]
    } else {
        vec![
            "sh".to_string(),
            "-c".to_string(),
            "mkdir -p dist && echo 'partial' > dist/partial.txt && exit 7".to_string(),
        ]
    };

    let (exit_code, got_artifacts) = client_roundtrip_with_artifacts(
        &server_addr,
        "",
        "artifact-fail",
        project_dir.path(),
        &failing_cmd,
        Some(vec!["dist".to_string()]),
        out_dir.path(),
    )
    .await;

    assert_eq!(exit_code, 7);
    assert!(
        !got_artifacts,
        "Artifacts should never be returned on failure"
    );
    assert!(!out_dir.path().join("dist").exists());
}

#[test]
fn test_cli_unreachable_agent_exit_code_125() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_fh"))
        .args([
            "--host",
            "127.0.0.1:1",
            "--token",
            "dummy",
            "--",
            "echo",
            "test",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(125));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unable to reach agent at 127.0.0.1:1"));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_cli_config_file_resolution_and_telemetry() {
    let token = "config-token-123".to_string();
    let workdir = tempdir().unwrap();
    let (server_addr, _server_handle) =
        spawn_test_server(Some(token.clone()), workdir.path().to_path_buf()).await;

    let project_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();

    // Create .farhand.yaml in project directory
    let yaml_content = format!(
        "host: {}\ntoken: {}\nname: test-config-app\noutDir: {}\noutputs:\n  - out/\n",
        server_addr,
        token,
        out_dir.path().display()
    );
    fs::write(project_dir.path().join(".farhand.yaml"), yaml_content).unwrap();

    let (shell_cmd, shell_arg, build_str) = if cfg!(windows) {
        (
            "cmd.exe",
            "/C",
            "mkdir out 2>nul & echo config-built > out\\artifact.txt",
        )
    } else {
        (
            "sh",
            "-c",
            "mkdir -p out && echo 'config-built' > out/artifact.txt",
        )
    };

    // Execute fh pointing to project_dir without passing --host or --token flags
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_fh"))
        .current_dir(project_dir.path())
        .args(["--verbose", "--", shell_cmd, shell_arg, build_str])
        .output()
        .await
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        output.status.code(),
        Some(0),
        "fh failed: stderr={}",
        stderr
    );
    assert!(stdout.contains("=== Farhand Execution Summary ==="));
    assert!(stdout.contains("[Scan]"));
    assert!(stdout.contains("[Delta Sync]"));
    assert!(stdout.contains("[Remote Build]"));
    assert!(stdout.contains("[Artifacts]"));

    // Verify artifact was unpacked into out_dir specified in .farhand.yaml
    let artifact_file = out_dir.path().join("out/artifact.txt");
    assert!(artifact_file.exists());
    assert_eq!(
        fs::read_to_string(artifact_file).unwrap().trim(),
        "config-built"
    );
}

#[test]
fn test_cli_templates_list_and_show() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_fh"))
        .args(["templates", "list"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("NAME"));
    assert!(stdout.contains("SOURCE"));
    assert!(stdout.contains("rust"));
    assert!(stdout.contains("npm"));
    assert!(stdout.contains("go"));
    assert!(stdout.contains("builtin"));

    let show_out = std::process::Command::new(env!("CARGO_BIN_EXE_fh"))
        .args(["templates", "show", "rust"])
        .output()
        .unwrap();

    assert_eq!(show_out.status.code(), Some(0));
    let show_stdout = String::from_utf8_lossy(&show_out.stdout);
    assert!(show_stdout.contains("name: rust"));
    assert!(show_stdout.contains("Cargo.lock"));
}

#[test]
fn test_cli_templates_init() {
    let project_dir = tempdir().unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_fh"))
        .current_dir(project_dir.path())
        .args(["templates", "init", "rust"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let created_file = project_dir.path().join(".farhand/templates/rust.yaml");
    assert!(created_file.exists());

    let list_out = std::process::Command::new(env!("CARGO_BIN_EXE_fh"))
        .current_dir(project_dir.path())
        .args(["templates", "list"])
        .output()
        .unwrap();

    assert_eq!(list_out.status.code(), Some(0));
    let list_stdout = String::from_utf8_lossy(&list_out.stdout);
    // Project override for rust should now show "project" source
    assert!(list_stdout.contains("rust"));
    assert!(list_stdout.contains("project"));
}

#[test]
fn test_cli_init_command() {
    let project_dir = tempdir().unwrap();

    // 1. Run fh init in empty project
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_fh"))
        .current_dir(project_dir.path())
        .args(["init", "--name", "my-test-app"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let cfg_path = project_dir.path().join(".farhand.yaml");
    assert!(cfg_path.exists());
    let cfg_str = std::fs::read_to_string(&cfg_path).unwrap();
    assert!(cfg_str.contains("name: my-test-app"));
    assert!(cfg_str.contains("compression: zstd"));

    // 2. Second init without --force should fail with exit code 1
    let output2 = std::process::Command::new(env!("CARGO_BIN_EXE_fh"))
        .current_dir(project_dir.path())
        .args(["init"])
        .output()
        .unwrap();
    assert_eq!(output2.status.code(), Some(1));

    // 3. With --force and --with-template on a rust project
    std::fs::write(
        project_dir.path().join("Cargo.toml"),
        "[package]\nname = \"my-test-app\"\n",
    )
    .unwrap();
    let output3 = std::process::Command::new(env!("CARGO_BIN_EXE_fh"))
        .current_dir(project_dir.path())
        .args(["init", "--force", "--with-template"])
        .output()
        .unwrap();
    assert_eq!(output3.status.code(), Some(0));

    let template_path = project_dir.path().join(".farhand/templates/rust.yaml");
    assert!(
        template_path.exists(),
        "Template file should be generated with --with-template"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_template_monorepo_union_and_explicit_template() {
    let token = "template-token-456".to_string();
    let workdir = tempdir().unwrap();
    let (server_addr, _server_handle) =
        spawn_test_server(Some(token.clone()), workdir.path().to_path_buf()).await;

    let project_dir = tempdir().unwrap();
    let out_dir1 = tempdir().unwrap();
    let out_dir2 = tempdir().unwrap();

    // Setup monorepo: both Cargo.toml and package.json exist
    fs::write(
        project_dir.path().join("Cargo.toml"),
        "[package]\nname = \"mono\"\n",
    )
    .unwrap();
    fs::write(
        project_dir.path().join("package.json"),
        "{\"name\": \"mono\"}\n",
    )
    .unwrap();

    // 1. Run build without --template -> monorepo union (both target/release and dist fetched)
    let (shell_cmd, shell_arg, build_cmd) = if cfg!(windows) {
        (
            "cmd.exe",
            "/C",
            "mkdir target\\release 2>nul & mkdir dist 2>nul & echo rust-bin > target\\release\\mono-bin & echo web-bundle > dist\\bundle.js",
        )
    } else {
        (
            "sh",
            "-c",
            "mkdir -p target/release dist && echo 'rust-bin' > target/release/mono-bin && echo 'web-bundle' > dist/bundle.js",
        )
    };

    let output1 = tokio::process::Command::new(env!("CARGO_BIN_EXE_fh"))
        .current_dir(project_dir.path())
        .args([
            "--host",
            &server_addr,
            "--token",
            &token,
            "--out-dir",
            &out_dir1.path().display().to_string(),
            "--",
            shell_cmd,
            shell_arg,
            build_cmd,
        ])
        .output()
        .await
        .unwrap();

    assert_eq!(output1.status.code(), Some(0));
    assert!(out_dir1.path().join("target/release/mono-bin").exists());
    assert!(out_dir1.path().join("dist/bundle.js").exists());

    // 2. Run build WITH explicit --template rust -> ONLY target/release fetched, dist is NOT fetched
    let output2 = tokio::process::Command::new(env!("CARGO_BIN_EXE_fh"))
        .current_dir(project_dir.path())
        .args([
            "--host",
            &server_addr,
            "--token",
            &token,
            "--template",
            "rust",
            "--out-dir",
            &out_dir2.path().display().to_string(),
            "--",
            shell_cmd,
            shell_arg,
            build_cmd,
        ])
        .output()
        .await
        .unwrap();

    assert_eq!(output2.status.code(), Some(0));
    assert!(out_dir2.path().join("target/release/mono-bin").exists());
    assert!(
        !out_dir2.path().join("dist/bundle.js").exists(),
        "Explicit --template rust should NOT retrieve dist artifacts"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_put_template_wire_and_usage() {
    let token = "put-template-tok".to_string();
    let workdir = tempdir().unwrap();
    let (server_addr, _server_handle) =
        spawn_test_server(Some(token.clone()), workdir.path().to_path_buf()).await;

    let project_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();

    // Create a custom zig template locally
    let zig_yaml = r#"
name: zig
description: Zig toolchain
match:
  anyFile:
    - build.zig
outputs:
  - zig-out
"#;
    let template_dir = project_dir.path().join(".farhand/templates");
    fs::create_dir_all(&template_dir).unwrap();
    fs::write(template_dir.join("zig.yaml"), zig_yaml).unwrap();

    // Push template using fh templates push
    let push_out = tokio::process::Command::new(env!("CARGO_BIN_EXE_fh"))
        .current_dir(project_dir.path())
        .args([
            "--host",
            &server_addr,
            "--token",
            &token,
            "templates",
            "push",
            "zig",
        ])
        .output()
        .await
        .unwrap();

    assert_eq!(
        push_out.status.code(),
        Some(0),
        "templates push failed: {}",
        String::from_utf8_lossy(&push_out.stderr)
    );
    assert!(String::from_utf8_lossy(&push_out.stdout).contains("uploaded successfully"));

    // Add build.zig to project and run remote build
    fs::write(project_dir.path().join("build.zig"), "// zig build").unwrap();
    let (shell_cmd, shell_arg, build_cmd) = if cfg!(windows) {
        (
            "cmd.exe",
            "/C",
            "mkdir zig-out 2>nul & echo zig-binary > zig-out\\app",
        )
    } else {
        (
            "sh",
            "-c",
            "mkdir -p zig-out && echo 'zig-binary' > zig-out/app",
        )
    };

    let run_out = tokio::process::Command::new(env!("CARGO_BIN_EXE_fh"))
        .current_dir(project_dir.path())
        .args([
            "--host",
            &server_addr,
            "--token",
            &token,
            "--out-dir",
            &out_dir.path().display().to_string(),
            "--",
            shell_cmd,
            shell_arg,
            build_cmd,
        ])
        .output()
        .await
        .unwrap();

    assert_eq!(run_out.status.code(), Some(0));
    assert!(out_dir.path().join("zig-out/app").exists());
    assert_eq!(
        fs::read_to_string(out_dir.path().join("zig-out/app"))
            .unwrap()
            .trim(),
        "zig-binary"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_concurrency_project_workspace_locking_and_queued() {
    let token = "concurrency-proj-token".to_string();
    let workdir = tempdir().unwrap();
    let (server_addr, _server_handle) =
        spawn_test_server(Some(token.clone()), workdir.path().to_path_buf()).await;

    let project_dir = tempdir().unwrap();
    fs::write(project_dir.path().join("main.txt"), "hello").unwrap();
    let project_name = "serialized-project";

    // Client 1 runs a slow command holding the project workspace
    let server_addr_clone = server_addr.clone();
    let token_clone = token.clone();
    let pdir1 = project_dir.path().to_path_buf();
    let task1 = tokio::spawn(async move {
        client_roundtrip(
            &server_addr_clone,
            &token_clone,
            project_name,
            &pdir1,
            &[
                "sh".into(),
                "-c".into(),
                "sleep 0.6 && echo task1-finished".into(),
            ],
        )
        .await
    });

    // Give task1 time to connect, handshake, and hold the workspace lock
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Client 2 connects for the same project. It MUST receive QUEUED (reason: project_busy)
    let mut stream2 = TcpStream::connect(&server_addr).await.unwrap();
    let hello2 = HelloPayload {
        token: token.clone(),
        project: project_name.to_string(),
        protocol_version: CURRENT_PROTOCOL_VERSION,
        compressions: None,
    };
    write_json_frame(&mut stream2, MsgType::Hello, &hello2)
        .await
        .unwrap();

    let (msg_type, _payload) = read_frame(&mut stream2).await.unwrap();
    assert_eq!(msg_type, MsgType::HelloAck);

    let manifest2 = ManifestPayload { files: Vec::new() };
    write_json_frame(&mut stream2, MsgType::Manifest, &manifest2)
        .await
        .unwrap();

    // Verify task2 gets MsgType::Queued with reason project_busy
    let (msg_type, payload) = read_frame(&mut stream2).await.unwrap();
    assert_eq!(msg_type, MsgType::Queued);
    let queued: protocol::QueuedPayload = decode_json(&payload).unwrap();
    assert_eq!(queued.reason, "project_busy");

    // Wait for task 1 to finish
    let (_need1, out1, code1) = task1.await.unwrap();
    assert_eq!(code1, 0);
    assert!(out1.contains("task1-finished"));

    // Now client 2 should receive NEED as the lock freed up
    let (msg_type, payload) = read_frame(&mut stream2).await.unwrap();
    assert_eq!(msg_type, MsgType::Need);
    let _need2: NeedPayload = decode_json(&payload).unwrap();

    // Complete client 2 run
    write_frame(&mut stream2, MsgType::Files, &[])
        .await
        .unwrap();
    let run2 = RunPayload {
        argv: vec!["echo".into(), "task2-finished".into()],
        outputs: None,
        cwd: None,
        template: None,
        no_cache: false,
        env: None,
        toolchain: None,
        tty: false,
        cols: None,
        rows: None,
        raw_stdio: None,
    };
    write_json_frame(&mut stream2, MsgType::Run, &run2)
        .await
        .unwrap();

    let mut out2 = String::new();
    loop {
        let (msg_type, payload) = read_frame(&mut stream2).await.unwrap();
        match msg_type {
            MsgType::Log => {
                let log: LogPayload = decode_json(&payload).unwrap();
                out2.push_str(&log.data);
            }
            MsgType::Result => {
                let res: ResultPayload = decode_json(&payload).unwrap();
                assert_eq!(res.exit_code, 0);
                break;
            }
            other => panic!("Unexpected frame: {:?}", other),
        }
    }
    assert!(out2.contains("task2-finished"));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_concurrency_global_semaphore_limit() {
    let token = "concurrency-sem-token".to_string();
    let workdir = tempdir().unwrap();
    // Agent limited to max 1 concurrent run across all projects
    let (server_addr, _server_handle) = spawn_test_server_with_concurrency(
        Some(token.clone()),
        workdir.path().to_path_buf(),
        Some(1),
    )
    .await;

    let project_dir = tempdir().unwrap();

    // Client 1 on project-A
    let server_addr_clone = server_addr.clone();
    let token_clone = token.clone();
    let pdir1 = project_dir.path().to_path_buf();
    let task1 = tokio::spawn(async move {
        client_roundtrip(
            &server_addr_clone,
            &token_clone,
            "project-alpha",
            &pdir1,
            &[
                "sh".into(),
                "-c".into(),
                "sleep 0.6 && echo alpha-done".into(),
            ],
        )
        .await
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Client 2 connects for DIFFERENT project-B. Should get QUEUED with concurrency_limit
    let mut stream2 = TcpStream::connect(&server_addr).await.unwrap();
    let hello2 = HelloPayload {
        token: token.clone(),
        project: "project-beta".to_string(),
        protocol_version: CURRENT_PROTOCOL_VERSION,
        compressions: None,
    };
    write_json_frame(&mut stream2, MsgType::Hello, &hello2)
        .await
        .unwrap();

    let (msg_type, _) = read_frame(&mut stream2).await.unwrap();
    assert_eq!(msg_type, MsgType::HelloAck);

    let manifest2 = ManifestPayload { files: Vec::new() };
    write_json_frame(&mut stream2, MsgType::Manifest, &manifest2)
        .await
        .unwrap();

    // Verify task2 gets MsgType::Queued with reason concurrency_limit
    let (msg_type, payload) = read_frame(&mut stream2).await.unwrap();
    assert_eq!(msg_type, MsgType::Queued);
    let queued: protocol::QueuedPayload = decode_json(&payload).unwrap();
    assert_eq!(queued.reason, "concurrency_limit");

    let (_, out1, code1) = task1.await.unwrap();
    assert_eq!(code1, 0);
    assert!(out1.contains("alpha-done"));

    // Complete client 2
    let (msg_type, _) = read_frame(&mut stream2).await.unwrap();
    assert_eq!(msg_type, MsgType::Need);
    write_frame(&mut stream2, MsgType::Files, &[])
        .await
        .unwrap();
    let run2 = RunPayload {
        argv: vec!["echo".into(), "beta-done".into()],
        outputs: None,
        cwd: None,
        template: None,
        no_cache: false,
        env: None,
        toolchain: None,
        tty: false,
        cols: None,
        rows: None,
        raw_stdio: None,
    };
    write_json_frame(&mut stream2, MsgType::Run, &run2)
        .await
        .unwrap();

    let mut out2 = String::new();
    loop {
        let (msg_type, payload) = read_frame(&mut stream2).await.unwrap();
        match msg_type {
            MsgType::Log => {
                let log: LogPayload = decode_json(&payload).unwrap();
                out2.push_str(&log.data);
            }
            MsgType::Result => {
                let res: ResultPayload = decode_json(&payload).unwrap();
                assert_eq!(res.exit_code, 0);
                break;
            }
            other => panic!("Unexpected frame: {:?}", other),
        }
    }
    assert!(out2.contains("beta-done"));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_client_disconnect_terminates_remote_process_group() {
    let token = "disconnect-token".to_string();
    let workdir = tempdir().unwrap();
    let (server_addr, _server_handle) =
        spawn_test_server(Some(token.clone()), workdir.path().to_path_buf()).await;

    let mut stream = TcpStream::connect(&server_addr).await.unwrap();
    let hello = HelloPayload {
        token,
        project: "disconnect-test".to_string(),
        protocol_version: CURRENT_PROTOCOL_VERSION,
        compressions: None,
    };
    write_json_frame(&mut stream, MsgType::Hello, &hello)
        .await
        .unwrap();

    let (msg_type, _) = read_frame(&mut stream).await.unwrap();
    assert_eq!(msg_type, MsgType::HelloAck);

    let manifest = ManifestPayload { files: Vec::new() };
    write_json_frame(&mut stream, MsgType::Manifest, &manifest)
        .await
        .unwrap();

    let (msg_type, _) = read_frame(&mut stream).await.unwrap();
    assert_eq!(msg_type, MsgType::Need);
    write_frame(&mut stream, MsgType::Files, &[]).await.unwrap();

    // Start a command that sleeps in the background
    let run = RunPayload {
        argv: if cfg!(windows) {
            vec![
                "cmd.exe".into(),
                "/C".into(),
                "echo started & ping -n 30 127.0.0.1 > nul".into(),
            ]
        } else {
            vec!["sh".into(), "-c".into(), "echo started && sleep 30".into()]
        },
        outputs: None,
        cwd: None,
        template: None,
        no_cache: false,
        env: None,
        toolchain: None,
        tty: false,
        cols: None,
        rows: None,
        raw_stdio: None,
    };
    write_json_frame(&mut stream, MsgType::Run, &run)
        .await
        .unwrap();

    // Read until we see "started"
    let (msg_type, payload) = read_frame(&mut stream).await.unwrap();
    assert_eq!(msg_type, MsgType::Log);
    let log: LogPayload = decode_json(&payload).unwrap();
    assert!(log.data.contains("started"));

    // Drop the stream abruptly (simulating Ctrl-C on client)
    drop(stream);

    // The agent detects EOF, kills the child process group, and exits the task
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
}

#[tokio::test]
async fn test_e2e_multi_agent_pool_least_busy_dispatch() {
    let token = "pool-secret".to_string();
    let workdir_a = tempdir().unwrap();
    let workdir_b = tempdir().unwrap();

    let (addr_a, _handle_a) =
        spawn_test_server(Some(token.clone()), workdir_a.path().to_path_buf()).await;
    let (addr_b, _handle_b) =
        spawn_test_server(Some(token.clone()), workdir_b.path().to_path_buf()).await;

    // Occupy agent A with a running task
    let mut stream_a = TcpStream::connect(&addr_a).await.unwrap();
    let hello = HelloPayload {
        token: token.clone(),
        project: "busy-proj".into(),
        protocol_version: CURRENT_PROTOCOL_VERSION,
        compressions: None,
    };
    write_json_frame(&mut stream_a, MsgType::Hello, &hello)
        .await
        .unwrap();
    let _ = read_frame(&mut stream_a).await.unwrap();
    let manifest = ManifestPayload { files: Vec::new() };
    write_json_frame(&mut stream_a, MsgType::Manifest, &manifest)
        .await
        .unwrap();
    let _ = read_frame(&mut stream_a).await.unwrap();
    write_frame(&mut stream_a, MsgType::Files, &[])
        .await
        .unwrap();

    let run = RunPayload {
        argv: vec!["sh".into(), "-c".into(), "echo busy && sleep 3".into()],
        outputs: None,
        cwd: None,
        template: None,
        no_cache: false,
        env: None,
        toolchain: None,
        tty: false,
        cols: None,
        rows: None,
        raw_stdio: None,
    };
    write_json_frame(&mut stream_a, MsgType::Run, &run)
        .await
        .unwrap();
    let _ = read_frame(&mut stream_a).await.unwrap(); // log "busy"

    // Both agents are in the pool
    let agents = vec![
        config::AgentConfig {
            host: addr_a.clone(),
            token: Some(token.clone()),
            tags: vec!["generic".into()],
            tls: None,
        },
        config::AgentConfig {
            host: addr_b.clone(),
            token: Some(token.clone()),
            tags: vec!["generic".into()],
            tls: None,
        },
    ];

    // select_best_agent should pick agent B because agent A has active_runs == 1
    let best = fh::select_best_agent(&agents, None, false).await.unwrap();
    assert_eq!(best.host, addr_b);
}

#[tokio::test]
async fn test_e2e_multi_agent_pool_tag_filtering() {
    let token = "pool-secret".to_string();
    let workdir_a = tempdir().unwrap();
    let workdir_b = tempdir().unwrap();

    let (addr_a, _handle_a) = spawn_test_server_with_tags(
        Some(token.clone()),
        workdir_a.path().to_path_buf(),
        None,
        vec!["cpu".into(), "fast".into()],
    )
    .await;
    let (addr_b, _handle_b) = spawn_test_server_with_tags(
        Some(token.clone()),
        workdir_b.path().to_path_buf(),
        None,
        vec!["gpu".into(), "cuda".into()],
    )
    .await;

    let agents = vec![
        config::AgentConfig {
            host: addr_a.clone(),
            token: Some(token.clone()),
            tags: vec!["cpu".into(), "fast".into()],
            tls: None,
        },
        config::AgentConfig {
            host: addr_b.clone(),
            token: Some(token.clone()),
            tags: vec!["gpu".into(), "cuda".into()],
            tls: None,
        },
    ];

    // Filter by "gpu" -> should select addr_b
    let selected_gpu = fh::select_best_agent(&agents, Some("gpu"), false)
        .await
        .unwrap();
    assert_eq!(selected_gpu.host, addr_b);

    // Filter by "fast" -> should select addr_a
    let selected_cpu = fh::select_best_agent(&agents, Some("fast"), false)
        .await
        .unwrap();
    assert_eq!(selected_cpu.host, addr_a);

    // Filter by nonexistent tag -> error
    let err = fh::select_best_agent(&agents, Some("nonexistent"), false).await;
    assert!(err.is_err());
}

#[tokio::test]
async fn test_e2e_multi_agent_pool_offline_failover() {
    let token = "pool-secret".to_string();
    let workdir_live = tempdir().unwrap();

    let (addr_live, _handle) =
        spawn_test_server(Some(token.clone()), workdir_live.path().to_path_buf()).await;
    // Port 1 on localhost is virtually guaranteed closed/unreachable
    let addr_offline = "127.0.0.1:1".to_string();

    let agents = vec![
        config::AgentConfig {
            host: addr_offline,
            token: Some(token.clone()),
            tags: vec![],
            tls: None,
        },
        config::AgentConfig {
            host: addr_live.clone(),
            token: Some(token.clone()),
            tags: vec![],
            tls: None,
        },
    ];

    // Candidate 1 fails, candidate 2 succeeds -> best is live agent
    let best = fh::select_best_agent(&agents, None, false).await.unwrap();
    assert_eq!(best.host, addr_live);
}

#[tokio::test]
async fn test_e2e_dependency_hook_full_caching_lifecycle() {
    let token = "hook-secret".to_string();
    let remote_workdir = tempdir().unwrap();
    let (server_addr, _handle) =
        spawn_test_server(Some(token.clone()), remote_workdir.path().to_path_buf()).await;

    let project_dir = tempdir().unwrap();
    let tmpl_dir = project_dir.path().join(".farhand").join("templates");
    fs::create_dir_all(&tmpl_dir).unwrap();

    // Create a custom template with an installCommand and declared lockfiles
    let hook_cmd = if cfg!(windows) {
        "echo hook-executed >> hook.log"
    } else {
        "sh -c \"echo hook-executed >> hook.log\""
    };
    let tmpl_yaml = format!(
        r#"
name: test-hook-lang
match:
  anyFile:
    - deps.lock
ignoreExtra:
  - hook.log
hints:
  installCommand: {}
  lockfiles:
    - deps.lock
"#,
        hook_cmd
    );
    fs::write(tmpl_dir.join("test-hook-lang.yaml"), tmpl_yaml).unwrap();
    fs::write(project_dir.path().join("deps.lock"), "dep-version-1\n").unwrap();

    let project_name = "hook-test-proj";

    // Run 1: Fresh workspace -> hook must execute
    let (_, out1, code1) = client_roundtrip(
        &server_addr,
        &token,
        project_name,
        project_dir.path(),
        &["echo".into(), "user-cmd-done".into()],
    )
    .await;
    assert_eq!(code1, 0);
    assert!(out1.contains("=== [farhand] Running dependency hook:"));
    assert!(out1.contains("=== [farhand] Dependencies up to date. Proceeding to user command ==="));
    assert!(out1.contains("user-cmd-done"));

    // Check remote workspace: hook.log should exist with 1 execution
    let remote_proj_dir = workspace::resolve_workspace_dir(remote_workdir.path(), project_name);
    let log_content = fs::read_to_string(remote_proj_dir.join("hook.log")).unwrap();
    assert_eq!(log_content.matches("hook-executed").count(), 1);
    assert!(remote_proj_dir.join(".farhand-state.json").exists());

    // Run 2: Same lockfile -> hook must be skipped!
    let (_, out2, code2) = client_roundtrip(
        &server_addr,
        &token,
        project_name,
        project_dir.path(),
        &["echo".into(), "user-cmd-done".into()],
    )
    .await;
    assert_eq!(code2, 0);
    assert!(!out2.contains("=== [farhand] Running dependency hook:"));
    assert!(out2.contains("user-cmd-done"));
    let log_content = fs::read_to_string(remote_proj_dir.join("hook.log")).unwrap();
    assert_eq!(
        log_content.matches("hook-executed").count(),
        1,
        "hook must have been skipped on run 2"
    );

    // Run 3: Modify lockfile -> hook must re-execute!
    fs::write(project_dir.path().join("deps.lock"), "dep-version-2\n").unwrap();
    let (_, out3, code3) = client_roundtrip(
        &server_addr,
        &token,
        project_name,
        project_dir.path(),
        &["echo".into(), "user-cmd-done".into()],
    )
    .await;
    assert_eq!(code3, 0);
    assert!(out3.contains("=== [farhand] Running dependency hook:"));
    let log_content = fs::read_to_string(remote_proj_dir.join("hook.log")).unwrap();
    assert_eq!(
        log_content.matches("hook-executed").count(),
        2,
        "hook must have re-executed after lockfile change"
    );

    // Run 4: Unchanged lockfile but with no_cache = true -> hook must re-execute!
    let (_, out4, code4) = client_roundtrip_with_options(
        &server_addr,
        &token,
        project_name,
        project_dir.path(),
        &["echo".into(), "user-cmd-done".into()],
        None,
        true,
    )
    .await;
    assert_eq!(code4, 0);
    assert!(out4.contains("=== [farhand] Running dependency hook:"));
    let log_content = fs::read_to_string(remote_proj_dir.join("hook.log")).unwrap();
    assert_eq!(
        log_content.matches("hook-executed").count(),
        3,
        "hook must have re-executed with no_cache=true"
    );
}

#[tokio::test]
async fn test_e2e_dependency_hook_failure_aborts_run() {
    let token = "hook-secret".to_string();
    let remote_workdir = tempdir().unwrap();
    let (server_addr, _handle) =
        spawn_test_server(Some(token.clone()), remote_workdir.path().to_path_buf()).await;

    let project_dir = tempdir().unwrap();
    let tmpl_dir = project_dir.path().join(".farhand").join("templates");
    fs::create_dir_all(&tmpl_dir).unwrap();

    let hook_cmd = if cfg!(windows) {
        "echo failing installation step & exit 42"
    } else {
        "sh -c \"echo 'failing installation step' && exit 42\""
    };
    let tmpl_yaml = format!(
        r#"
name: failing-hook-lang
match:
  anyFile:
    - fail.lock
hints:
  installCommand: {}
  lockfiles:
    - fail.lock
"#,
        hook_cmd
    );
    fs::write(tmpl_dir.join("failing-hook-lang.yaml"), tmpl_yaml).unwrap();
    fs::write(project_dir.path().join("fail.lock"), "fail-version-1\n").unwrap();

    let project_name = "failing-hook-proj";

    let (_, out, code) = client_roundtrip(
        &server_addr,
        &token,
        project_name,
        project_dir.path(),
        &["echo".into(), "SHOULD_NOT_EXECUTE".into()],
    )
    .await;

    assert_eq!(
        code, 42,
        "exit code must be mirrored from the failed install hook"
    );
    assert!(out.contains("=== [farhand] Running dependency hook:"));
    assert!(out.contains("failing installation step"));
    assert!(
        !out.contains("SHOULD_NOT_EXECUTE"),
        "user command must not be executed when install hook fails"
    );

    // Remote workspace should NOT have recorded successful state
    let remote_proj_dir = workspace::resolve_workspace_dir(remote_workdir.path(), project_name);
    assert!(!remote_proj_dir.join(".farhand-state.json").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_history_query_and_persistence() {
    let token = "history-tok-1".to_string();
    let remote_workdir = tempdir().unwrap();
    let (server_addr, _handle) =
        spawn_test_server(Some(token.clone()), remote_workdir.path().to_path_buf()).await;

    let project_dir = tempdir().unwrap();
    fs::write(
        project_dir.path().join("main.c"),
        "int main() { return 0; }\n",
    )
    .unwrap();

    let project_name = "history-demo-proj";

    // 1. Run a build via client roundtrip
    let (_, _, exit_code) = client_roundtrip(
        &server_addr,
        &token,
        project_name,
        project_dir.path(),
        &["echo".into(), "hello from run 1".into()],
    )
    .await;
    assert_eq!(exit_code, 0);

    // 2. Query history using fh::query_history function directly
    let mut resp = fh::query_history(&server_addr, &token, project_name, 10, None)
        .await
        .expect("query_history should succeed");
    for _ in 0..10 {
        if !resp.runs.is_empty() {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(15)).await;
        resp = fh::query_history(&server_addr, &token, project_name, 10, None)
            .await
            .expect("query_history should succeed");
    }

    assert_eq!(resp.project, project_name);
    assert_eq!(resp.runs.len(), 1);
    let run = &resp.runs[0];
    assert_eq!(run.project, project_name);
    assert_eq!(run.exit_code, 0);
    assert_eq!(run.argv, vec!["echo", "hello from run 1"]);
    assert!(run.bytes_synced > 0);
    assert!(!run.timestamp_rfc3339.is_empty());
    assert!(!run.id.is_empty());

    // 3. Query history via CLI binary (text table format)
    let cli_out = tokio::process::Command::new(env!("CARGO_BIN_EXE_fh"))
        .args([
            "--host",
            &server_addr,
            "--token",
            &token,
            "history",
            "--name",
            project_name,
        ])
        .output()
        .await
        .unwrap();

    assert_eq!(cli_out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&cli_out.stdout);
    assert!(stdout.contains("DATE / TIME (UTC)"));
    assert!(stdout.contains("echo hello from run 1"));

    // 4. Query history via CLI binary (--log-format json)
    let json_cli_out = tokio::process::Command::new(env!("CARGO_BIN_EXE_fh"))
        .args([
            "--host",
            &server_addr,
            "--token",
            &token,
            "--log-format",
            "json",
            "history",
            "--name",
            project_name,
        ])
        .output()
        .await
        .unwrap();

    assert_eq!(json_cli_out.status.code(), Some(0));
    let json_stdout = String::from_utf8_lossy(&json_cli_out.stdout);
    let parsed: protocol::HistoryResponsePayload =
        serde_json::from_str(&json_stdout).expect("CLI JSON output must be valid JSON");
    assert_eq!(parsed.runs.len(), 1);
    assert_eq!(parsed.runs[0].id, run.id);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_history_limit_and_ordering() {
    let token = "history-tok-2".to_string();
    let remote_workdir = tempdir().unwrap();
    let (server_addr, _handle) =
        spawn_test_server(Some(token.clone()), remote_workdir.path().to_path_buf()).await;

    let project_dir = tempdir().unwrap();
    fs::write(project_dir.path().join("file.txt"), "content\n").unwrap();
    let project_name = "history-ordering-proj";

    // Run 3 commands sequentially
    for i in 1..=3 {
        let cmd = format!("iteration-{}", i);
        let (_, _, code) = client_roundtrip(
            &server_addr,
            &token,
            project_name,
            project_dir.path(),
            &["echo".into(), cmd],
        )
        .await;
        assert_eq!(code, 0);
        tokio::time::sleep(tokio::time::Duration::from_millis(15)).await;
    }

    // Query with limit 2
    let resp = fh::query_history(&server_addr, &token, project_name, 2, None)
        .await
        .expect("query_history should succeed");

    assert_eq!(resp.runs.len(), 2);
    // Newest first: iteration-3 then iteration-2
    assert_eq!(resp.runs[0].argv[1], "iteration-3");
    assert_eq!(resp.runs[1].argv[1], "iteration-2");
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_history_unauthorized() {
    let token = "valid-secret".to_string();
    let remote_workdir = tempdir().unwrap();
    let (server_addr, _handle) =
        spawn_test_server(Some(token), remote_workdir.path().to_path_buf()).await;

    let res = fh::query_history(&server_addr, "wrong-token", "some-proj", 10, None).await;
    assert!(res.is_err(), "Unauthorized history request must fail");

    // Also via CLI binary: must exit with code 125
    let cli_out = tokio::process::Command::new(env!("CARGO_BIN_EXE_fh"))
        .args([
            "--host",
            &server_addr,
            "--token",
            "wrong-token",
            "history",
            "--name",
            "some-proj",
        ])
        .output()
        .await
        .unwrap();

    assert_eq!(
        cli_out.status.code(),
        Some(125),
        "Unauthorized request must exit with 125"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_branch_workspace_cow_cloning() {
    let token = "cow-tok".to_string();
    let remote_workdir = tempdir().unwrap();
    let (server_addr, _handle) =
        spawn_test_server(Some(token.clone()), remote_workdir.path().to_path_buf()).await;

    let project_dir = tempdir().unwrap();
    fs::write(
        project_dir.path().join("common.txt"),
        "base repository content\n",
    )
    .unwrap();

    let base_proj = "cow-repo";

    // 1. Run build for base project (creates seed workspace)
    let (_, _, code1) = client_roundtrip(
        &server_addr,
        &token,
        base_proj,
        project_dir.path(),
        &["echo".into(), "base-build".into()],
    )
    .await;
    assert_eq!(code1, 0);

    let seed_dir = workspace::resolve_workspace_dir(remote_workdir.path(), base_proj);
    assert!(seed_dir.is_dir());
    assert!(seed_dir.join("common.txt").is_file());

    // 2. Run build on a feature branch: cow-repo__feat-alpha
    let branch_proj = "cow-repo__feat-alpha";
    fs::write(
        project_dir.path().join("feature.txt"),
        "new feature content\n",
    )
    .unwrap();

    let (need, _, code2) = client_roundtrip(
        &server_addr,
        &token,
        branch_proj,
        project_dir.path(),
        &["echo".into(), "branch-build".into()],
    )
    .await;
    assert_eq!(code2, 0);

    // Because branch was cloned via CoW from base, common.txt was already present on remote!
    // So the client manifest diff only wanted the new feature.txt file!
    assert!(!need.want.contains(&"common.txt".to_string()));
    assert!(need.want.contains(&"feature.txt".to_string()));

    let branch_dir = workspace::resolve_workspace_dir(remote_workdir.path(), branch_proj);
    assert!(branch_dir.is_dir());
    assert!(branch_dir.join("common.txt").is_file());
    assert!(branch_dir.join("feature.txt").is_file());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_workspace_clean_subcommand() {
    let token = "clean-tok".to_string();
    let remote_workdir = tempdir().unwrap();
    let (server_addr, _handle) =
        spawn_test_server(Some(token.clone()), remote_workdir.path().to_path_buf()).await;

    let project_dir = tempdir().unwrap();
    fs::write(project_dir.path().join("code.txt"), "some code\n").unwrap();

    let proj1 = "clean-repo__branch-1";
    let proj2 = "clean-repo__branch-2";

    // Run builds on both branches
    client_roundtrip(
        &server_addr,
        &token,
        proj1,
        project_dir.path(),
        &["echo".into(), "1".into()],
    )
    .await;
    client_roundtrip(
        &server_addr,
        &token,
        proj2,
        project_dir.path(),
        &["echo".into(), "2".into()],
    )
    .await;

    let dir1 = workspace::resolve_workspace_dir(remote_workdir.path(), proj1);
    let dir2 = workspace::resolve_workspace_dir(remote_workdir.path(), proj2);
    assert!(dir1.is_dir());
    assert!(dir2.is_dir());

    // 1. Clean branch 1 specifically
    let resp1 = fh::clean_workspace(&server_addr, &token, proj1, false, false, None)
        .await
        .expect("clean should succeed");
    assert!(resp1.ok);
    assert!(!dir1.exists());
    assert!(dir2.exists());

    // 2. Clean all branches for clean-repo
    let resp2 = fh::clean_workspace(&server_addr, &token, "clean-repo", true, false, None)
        .await
        .expect("clean all should succeed");
    assert!(resp2.ok);
    assert!(!dir2.exists());

    // 3. Test clean via CLI binary
    let cli_out = tokio::process::Command::new(env!("CARGO_BIN_EXE_fh"))
        .args([
            "--host",
            &server_addr,
            "--token",
            &token,
            "clean",
            "--name",
            "clean-repo",
            "--all-branches",
        ])
        .output()
        .await
        .unwrap();

    assert_eq!(cli_out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&cli_out.stdout);
    assert!(stdout.contains("[clean]"));
}

#[tokio::test]
async fn test_e2e_env_vars_forwarding_default() {
    let token = "env-secret".to_string();
    let workdir = tempdir().unwrap();
    let (server_addr, _handle) =
        spawn_test_server(Some(token.clone()), workdir.path().to_path_buf()).await;

    let project_dir = tempdir().unwrap();
    fs::write(project_dir.path().join("file.txt"), "hello").unwrap();

    let (shell_cmd, shell_arg, echo_cmd) = if cfg!(windows) {
        ("cmd.exe", "/C", "echo VAR=%INFISICAL_DATABASE_URL%")
    } else {
        ("sh", "-c", "echo VAR=$INFISICAL_DATABASE_URL")
    };

    // Execute fh with ambient INFISICAL_DATABASE_URL environment variable set.
    // INFISICAL_* is on the credential denylist: ambient secrets must NOT leak
    // to the agent. An explicit -e override is the documented way to forward.
    let (no_forward, forward) = tokio::join!(
        async {
            tokio::process::Command::new(env!("CARGO_BIN_EXE_fh"))
                .current_dir(project_dir.path())
                .env(
                    "INFISICAL_DATABASE_URL",
                    "postgres://user:pass@remote:5432/app",
                )
                .args([
                    "--host",
                    &server_addr,
                    "--token",
                    &token,
                    "--",
                    shell_cmd,
                    shell_arg,
                    echo_cmd,
                ])
                .output()
                .await
                .unwrap()
        },
        async {
            tokio::process::Command::new(env!("CARGO_BIN_EXE_fh"))
                .current_dir(project_dir.path())
                .env(
                    "INFISICAL_DATABASE_URL",
                    "postgres://user:pass@remote:5432/app",
                )
                .args([
                    "--host",
                    &server_addr,
                    "--token",
                    &token,
                    "--verbose",
                    "-e",
                    "INFISICAL_DATABASE_URL=postgres://user:pass@remote:5432/app",
                    "--",
                    shell_cmd,
                    shell_arg,
                    echo_cmd,
                ])
                .output()
                .await
                .unwrap()
        }
    );

    assert_eq!(no_forward.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&no_forward.stdout);
    assert!(
        stdout.contains("VAR=") && !stdout.contains("postgres://"),
        "Ambient INFISICAL_DATABASE_URL must be denylisted (leak guard): {}",
        stdout
    );

    assert_eq!(forward.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&forward.stdout);
    assert!(
        stdout.contains("VAR=postgres://user:pass@remote:5432/app"),
        "Explicit -e override should forward the variable: {}",
        stdout
    );
}

#[tokio::test]
async fn test_e2e_env_vars_disabled_via_no_env() {
    let token = "env-secret".to_string();
    let workdir = tempdir().unwrap();
    let (server_addr, _handle) =
        spawn_test_server(Some(token.clone()), workdir.path().to_path_buf()).await;

    let project_dir = tempdir().unwrap();
    fs::write(project_dir.path().join("file.txt"), "hello").unwrap();

    let (shell_cmd, shell_arg, echo_cmd) = if cfg!(windows) {
        (
            "cmd.exe",
            "/C",
            "if defined INFISICAL_SECRET (echo FOUND) else (echo MISSING)",
        )
    } else {
        (
            "sh",
            "-c",
            "if [ -n \"$INFISICAL_SECRET\" ]; then echo FOUND; else echo MISSING; fi",
        )
    };

    // Execute fh with --no-env flag
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_fh"))
        .current_dir(project_dir.path())
        .env("INFISICAL_SECRET", "super_secret_val")
        .args([
            "--host",
            &server_addr,
            "--token",
            &token,
            "--no-env",
            "--",
            shell_cmd,
            shell_arg,
            echo_cmd,
        ])
        .output()
        .await
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("MISSING"),
        "Stdout should indicate env variable was not forwarded when --no-env is used: {}",
        stdout
    );
}

#[tokio::test]
async fn test_e2e_env_vars_explicit_flag_and_config() {
    let token = "env-secret".to_string();
    let workdir = tempdir().unwrap();
    let (server_addr, _handle) =
        spawn_test_server(Some(token.clone()), workdir.path().to_path_buf()).await;

    let project_dir = tempdir().unwrap();
    fs::write(project_dir.path().join("file.txt"), "hello").unwrap();

    // Test with .farhand.yaml specifying forwardEnv: false and env: { CONFIG_KEY: config_val_123 }
    let yaml = format!(
        r#"
host: "{}"
token: "{}"
forwardEnv: false
env:
  CONFIG_KEY: config_val_123
"#,
        server_addr, token
    );
    fs::write(project_dir.path().join(".farhand.yaml"), yaml).unwrap();

    let (shell_cmd, shell_arg, echo_cmd) = if cfg!(windows) {
        (
            "cmd.exe",
            "/C",
            "echo C=%CONFIG_KEY% E=%CLI_KEY% A=%AMBIENT_KEY%",
        )
    } else {
        ("sh", "-c", "echo C=$CONFIG_KEY E=$CLI_KEY A=$AMBIENT_KEY")
    };

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_fh"))
        .current_dir(project_dir.path())
        .env("AMBIENT_KEY", "ambient_should_be_skipped")
        .args([
            "-e",
            "CLI_KEY=cli_val_456",
            "--",
            shell_cmd,
            shell_arg,
            echo_cmd,
        ])
        .output()
        .await
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("C=config_val_123"),
        "Config env should be present: {}",
        stdout
    );
    assert!(
        stdout.contains("E=cli_val_456"),
        "CLI -e flag env should be present: {}",
        stdout
    );
    if cfg!(windows) {
        assert!(stdout.contains("A=%AMBIENT_KEY%"));
    } else {
        assert!(stdout.contains("A="));
        assert!(!stdout.contains("ambient_should_be_skipped"));
    }
}

#[tokio::test]
async fn test_cli_tier1_help_flags() {
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_fh"))
        .arg("--help")
        .output()
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--tty"),
        "Should mention --tty in help: {}",
        stdout
    );
    assert!(
        stdout.contains("--forward"),
        "Should mention --forward in help: {}",
        stdout
    );
    assert!(
        stdout.contains("--watch"),
        "Should mention --watch in help: {}",
        stdout
    );
    assert!(
        stdout.contains("watch"),
        "Should mention watch subcommand: {}",
        stdout
    );

    let watch_output = tokio::process::Command::new(env!("CARGO_BIN_EXE_fh"))
        .args(["watch", "--help"])
        .output()
        .await
        .unwrap();
    assert_eq!(watch_output.status.code(), Some(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_pty_interactive_execution() {
    // ConPTY requires an interactive desktop window station; headless Windows CI runners
    // in Session 0 cannot allocate a console screen buffer and will hang.
    if cfg!(windows) {
        eprintln!("Skipping PTY interactive execution test on headless Windows");
        return;
    }

    let token = "pty-secret".to_string();
    let workdir = tempdir().unwrap();
    let (server_addr, _handle) =
        spawn_test_server(Some(token.clone()), workdir.path().to_path_buf()).await;

    let mut stream = TcpStream::connect(&server_addr).await.unwrap();

    let hello = HelloPayload {
        token: token.clone(),
        project: "pty-test-proj".to_string(),
        protocol_version: CURRENT_PROTOCOL_VERSION,
        compressions: None,
    };
    write_json_frame(&mut stream, MsgType::Hello, &hello)
        .await
        .unwrap();
    let (msg_type, payload) = read_frame(&mut stream).await.unwrap();
    assert_eq!(msg_type, MsgType::HelloAck);
    let ack: HelloAckPayload = decode_json(&payload).unwrap();
    assert!(ack.ok);

    let manifest = ManifestPayload { files: Vec::new() };
    write_json_frame(&mut stream, MsgType::Manifest, &manifest)
        .await
        .unwrap();
    let (msg_type, _) = read_frame(&mut stream).await.unwrap();
    assert_eq!(msg_type, MsgType::Need);
    write_frame(&mut stream, MsgType::Files, &[]).await.unwrap();

    let run = RunPayload {
        argv: if cfg!(windows) {
            vec![
                "cmd.exe".into(),
                "/C".into(),
                "echo interactive_pty_output_12345".into(),
            ]
        } else {
            vec!["echo".into(), "interactive_pty_output_12345".into()]
        },
        outputs: None,
        cwd: None,
        template: None,
        no_cache: false,
        env: None,
        toolchain: None,
        tty: true,
        cols: Some(80),
        rows: Some(24),
        raw_stdio: None,
    };
    write_json_frame(&mut stream, MsgType::Run, &run)
        .await
        .unwrap();

    let mut collected = String::new();
    let exit_code = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let (msg_type, payload) = read_frame(&mut stream).await.unwrap();
            match msg_type {
                MsgType::Log => {
                    let log: LogPayload = decode_json(&payload).unwrap();
                    collected.push_str(&log.data);
                }
                MsgType::Result => {
                    let res: ResultPayload = decode_json(&payload).unwrap();
                    return res.exit_code;
                }
                _ => {}
            }
        }
    })
    .await
    .expect("PTY interactive execution timed out waiting for Result frame");

    assert_eq!(exit_code, 0);
    assert!(
        collected.contains("interactive_pty_output_12345"),
        "PTY output did not contain expected text: {}",
        collected
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_pty_terminal_resize_and_stdin() {
    // ConPTY requires an interactive desktop window station; headless Windows CI runners
    // in Session 0 cannot allocate a console screen buffer and will hang.
    if cfg!(windows) {
        eprintln!("Skipping PTY terminal resize/stdin test on headless Windows");
        return;
    }

    let token = "pty-resize-secret".to_string();
    let workdir = tempdir().unwrap();
    let (server_addr, _handle) =
        spawn_test_server(Some(token.clone()), workdir.path().to_path_buf()).await;

    let mut stream = TcpStream::connect(&server_addr).await.unwrap();

    let hello = HelloPayload {
        token: token.clone(),
        project: "pty-resize-proj".to_string(),
        protocol_version: CURRENT_PROTOCOL_VERSION,
        compressions: None,
    };
    write_json_frame(&mut stream, MsgType::Hello, &hello)
        .await
        .unwrap();
    let _ = read_frame(&mut stream).await.unwrap();

    let manifest = ManifestPayload { files: Vec::new() };
    write_json_frame(&mut stream, MsgType::Manifest, &manifest)
        .await
        .unwrap();
    let _ = read_frame(&mut stream).await.unwrap();
    write_frame(&mut stream, MsgType::Files, &[]).await.unwrap();

    let run = RunPayload {
        argv: if cfg!(windows) {
            vec!["cmd.exe".into(), "/C".into(), "echo pty-ok".into()]
        } else {
            vec!["sh".into(), "-c".into(), "echo pty-ok".into()]
        },
        outputs: None,
        cwd: None,
        template: None,
        no_cache: false,
        env: None,
        toolchain: None,
        tty: true,
        cols: Some(80),
        rows: Some(24),
        raw_stdio: None,
    };
    write_json_frame(&mut stream, MsgType::Run, &run)
        .await
        .unwrap();

    // Send Resize frame while running
    let resize = protocol::ResizePayload {
        cols: 120,
        rows: 40,
    };
    let _ = write_json_frame(&mut stream, MsgType::Resize, &resize).await;

    // Send Stdin frame
    let _ = write_frame(&mut stream, MsgType::Stdin, b"hello\n").await;

    let mut collected = String::new();
    let exit_code = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let (msg_type, payload) = read_frame(&mut stream).await.unwrap();
            match msg_type {
                MsgType::Log => {
                    let log: LogPayload = decode_json(&payload).unwrap();
                    collected.push_str(&log.data);
                }
                MsgType::Result => {
                    let res: ResultPayload = decode_json(&payload).unwrap();
                    return res.exit_code;
                }
                _ => {}
            }
        }
    })
    .await
    .expect("PTY terminal resize/stdin test timed out waiting for Result frame");

    assert_eq!(exit_code, 0);
    assert!(collected.contains("pty-ok"));
}

#[tokio::test]
async fn test_e2e_reverse_port_forwarding_tunnel() {
    // 1. Start a local mock server simulating a service listening on agent host
    let mock_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_port = mock_listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        if let Ok((mut socket, _)) = mock_listener.accept().await {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 128];
            if let Ok(n) = socket.read(&mut buf).await {
                if n > 0 {
                    let mut response = b"echo-back: ".to_vec();
                    response.extend_from_slice(&buf[..n]);
                    let _ = socket.write_all(&response).await;
                }
            }
        }
    });

    let token = "port-fwd-secret".to_string();
    let workdir = tempdir().unwrap();
    let (server_addr, _handle) =
        spawn_test_server(Some(token.clone()), workdir.path().to_path_buf()).await;

    let mut stream = TcpStream::connect(&server_addr).await.unwrap();

    let hello = HelloPayload {
        token: token.clone(),
        project: "port-fwd-proj".to_string(),
        protocol_version: CURRENT_PROTOCOL_VERSION,
        compressions: None,
    };
    write_json_frame(&mut stream, MsgType::Hello, &hello)
        .await
        .unwrap();
    let _ = read_frame(&mut stream).await.unwrap();

    let manifest = ManifestPayload { files: Vec::new() };
    write_json_frame(&mut stream, MsgType::Manifest, &manifest)
        .await
        .unwrap();
    let _ = read_frame(&mut stream).await.unwrap();
    write_frame(&mut stream, MsgType::Files, &[]).await.unwrap();

    // Start a command that stays alive long enough to forward packets
    let run = RunPayload {
        argv: if cfg!(windows) {
            vec![
                "cmd.exe".into(),
                "/C".into(),
                "ping 127.0.0.1 -n 3 > nul".into(),
            ]
        } else {
            vec!["sleep".into(), "2".into()]
        },
        outputs: None,
        cwd: None,
        template: None,
        no_cache: false,
        env: None,
        toolchain: None,
        tty: false,
        cols: None,
        rows: None,
        raw_stdio: None,
    };
    write_json_frame(&mut stream, MsgType::Run, &run)
        .await
        .unwrap();

    // 2. Open tunnel channel on mock_port
    let open_payload = protocol::PortOpenPayload {
        channel_id: 101,
        target_port: mock_port,
    };
    write_json_frame(&mut stream, MsgType::PortOpen, &open_payload)
        .await
        .unwrap();

    // 3. Send PortData over tunnel
    let data_payload = protocol::PortDataPayload {
        channel_id: 101,
        data: b"hello-farhand-port".to_vec(),
    };
    write_json_frame(&mut stream, MsgType::PortData, &data_payload)
        .await
        .unwrap();

    // 4. Expect PortData back from daemon containing mock response
    let mut received_echo = false;
    loop {
        let (msg_type, payload) = read_frame(&mut stream).await.unwrap();
        match msg_type {
            MsgType::PortData => {
                let pd: protocol::PortDataPayload = decode_json(&payload).unwrap();
                if pd.channel_id == 101 {
                    let s = String::from_utf8_lossy(&pd.data);
                    if s.contains("echo-back: hello-farhand-port") {
                        received_echo = true;
                        break;
                    }
                }
            }
            MsgType::Result => {
                break;
            }
            _ => {}
        }
    }

    assert!(
        received_echo,
        "Tunnel did not receive echo response from mock service"
    );

    // 5. Close channel
    let close = protocol::PortClosePayload { channel_id: 101 };
    let _ = write_json_frame(&mut stream, MsgType::PortClose, &close).await;
}

#[tokio::test]
async fn test_e2e_preflight_disk_guard_rejection_and_status() {
    let workdir = tempdir().unwrap();
    let token = "disk-guard-secret".to_string();

    // 1. Check STATUS returns valid disk metrics
    let (server_addr, _handle) = spawn_test_server_full(
        Some(token.clone()),
        workdir.path().to_path_buf(),
        None,
        vec![],
        Some(0),
    )
    .await;

    let mut stream = TcpStream::connect(&server_addr).await.unwrap();
    let status_req = protocol::StatusRequestPayload {
        token: token.clone(),
    };
    write_json_frame(&mut stream, MsgType::Status, &status_req)
        .await
        .unwrap();

    let (msg_type, payload) = read_frame(&mut stream).await.unwrap();
    assert_eq!(msg_type, MsgType::StatusResp);
    let resp: protocol::StatusResponsePayload = decode_json(&payload).unwrap();
    assert!(
        resp.disk_free_bytes.is_some(),
        "STATUS response should include disk_free_bytes"
    );
    assert!(
        resp.disk_total_bytes.is_some(),
        "STATUS response should include disk_total_bytes"
    );

    // 2. Start a server demanding an impossible amount of free disk space (u64::MAX)
    let workdir2 = tempdir().unwrap();
    let (server_addr2, _handle2) = spawn_test_server_full(
        Some(token.clone()),
        workdir2.path().to_path_buf(),
        None,
        vec![],
        Some(u64::MAX),
    )
    .await;

    let mut stream2 = TcpStream::connect(&server_addr2).await.unwrap();
    let hello = HelloPayload {
        token: token.clone(),
        project: "low-disk-proj".to_string(),
        protocol_version: CURRENT_PROTOCOL_VERSION,
        compressions: None,
    };
    write_json_frame(&mut stream2, MsgType::Hello, &hello)
        .await
        .unwrap();

    let (msg_type2, payload2) = read_frame(&mut stream2).await.unwrap();
    assert_eq!(msg_type2, MsgType::HelloAck);
    let ack: HelloAckPayload = decode_json(&payload2).unwrap();
    assert!(
        !ack.ok,
        "Daemon must reject HELLO when free disk is below threshold"
    );
    let err = ack.error.unwrap_or_default();
    assert!(
        err.contains("Remote agent disk low"),
        "Error message should warn about low disk: {}",
        err
    );
}

#[tokio::test]
async fn test_e2e_cli_output_overrides_and_no_output() {
    let token = "output-override-secret".to_string();
    let workdir = tempdir().unwrap();
    let (server_addr, _handle) =
        spawn_test_server(Some(token.clone()), workdir.path().to_path_buf()).await;

    let project_dir = tempdir().unwrap();
    fs::write(project_dir.path().join("src.txt"), "build-me").unwrap();

    // Command that generates an artifact
    let cmd = vec![
        "sh".into(),
        "-c".into(),
        "mkdir -p dist && echo 'bundle-content' > dist/bundle.js".into(),
    ];

    // Case 1: Run with explicit outputs
    let mut stream = TcpStream::connect(&server_addr).await.unwrap();
    let hello = HelloPayload {
        token: token.clone(),
        project: "out-test-1".to_string(),
        protocol_version: CURRENT_PROTOCOL_VERSION,
        compressions: None,
    };
    write_json_frame(&mut stream, MsgType::Hello, &hello)
        .await
        .unwrap();
    let _ = read_frame(&mut stream).await.unwrap();

    let scanned = fileset::scan(project_dir.path(), &[]).unwrap();
    let manifest_files: Vec<FileEntry> = scanned
        .values()
        .map(|m| FileEntry {
            path: m.path.clone(),
            hash: m.hash.clone(),
            size: m.size,
            mode: m.mode,
        })
        .collect();
    write_json_frame(
        &mut stream,
        MsgType::Manifest,
        &ManifestPayload {
            files: manifest_files,
        },
    )
    .await
    .unwrap();
    let _ = read_frame(&mut stream).await.unwrap();
    write_frame(&mut stream, MsgType::Files, &[]).await.unwrap();

    let run_with_output = RunPayload {
        argv: cmd.clone(),
        outputs: Some(vec!["dist/bundle.js".into()]),
        cwd: None,
        template: None,
        no_cache: false,
        env: None,
        toolchain: None,
        tty: false,
        cols: None,
        rows: None,
        raw_stdio: None,
    };
    write_json_frame(&mut stream, MsgType::Run, &run_with_output)
        .await
        .unwrap();

    let mut received_artifacts = false;
    loop {
        let (msg, _) = read_frame(&mut stream).await.unwrap();
        if msg == MsgType::Artifacts {
            received_artifacts = true;
            break;
        } else if msg == MsgType::Result {
            // Check if artifacts follow or stop
            if let Ok((next_msg, _)) = read_frame(&mut stream).await {
                if next_msg == MsgType::Artifacts {
                    received_artifacts = true;
                }
            }
            break;
        }
    }
    assert!(
        received_artifacts,
        "Expected artifacts frame when outputs requested"
    );

    // Case 2: Run with outputs = None (simulating --no-output or fh exec)
    let mut stream2 = TcpStream::connect(&server_addr).await.unwrap();
    let hello2 = HelloPayload {
        token: token.clone(),
        project: "out-test-2".to_string(),
        protocol_version: CURRENT_PROTOCOL_VERSION,
        compressions: None,
    };
    write_json_frame(&mut stream2, MsgType::Hello, &hello2)
        .await
        .unwrap();
    let _ = read_frame(&mut stream2).await.unwrap();
    write_json_frame(
        &mut stream2,
        MsgType::Manifest,
        &ManifestPayload { files: vec![] },
    )
    .await
    .unwrap();
    let _ = read_frame(&mut stream2).await.unwrap();
    write_frame(&mut stream2, MsgType::Files, &[])
        .await
        .unwrap();

    let run_no_output = RunPayload {
        argv: cmd.clone(),
        outputs: None,
        cwd: None,
        template: None,
        no_cache: false,
        env: None,
        toolchain: None,
        tty: false,
        cols: None,
        rows: None,
        raw_stdio: None,
    };
    write_json_frame(&mut stream2, MsgType::Run, &run_no_output)
        .await
        .unwrap();

    let mut received_artifacts2 = false;
    loop {
        let (msg, _) = read_frame(&mut stream2).await.unwrap();
        if msg == MsgType::Artifacts {
            received_artifacts2 = true;
            break;
        } else if msg == MsgType::Result {
            break;
        }
    }
    assert!(
        !received_artifacts2,
        "Must NOT receive artifacts frame when outputs is None"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_pty_shell_fallback_and_spawn_error_exit_code() {
    let token = "pty-fallback-tok".to_string();
    let remote_workdir = tempdir().unwrap();
    let (server_addr, _handle) =
        spawn_test_server(Some(token.clone()), remote_workdir.path().to_path_buf()).await;

    // Test 1: Nonexistent command under PTY must return exit code 127, NOT disconnect abruptly
    let mut stream = TcpStream::connect(&server_addr).await.unwrap();
    let hello = HelloPayload {
        token: token.clone(),
        project: "pty-fallback-proj".to_string(),
        protocol_version: CURRENT_PROTOCOL_VERSION,
        compressions: None,
    };
    write_json_frame(&mut stream, MsgType::Hello, &hello)
        .await
        .unwrap();
    let _ = read_frame(&mut stream).await.unwrap();
    write_json_frame(
        &mut stream,
        MsgType::Manifest,
        &ManifestPayload { files: vec![] },
    )
    .await
    .unwrap();
    let _ = read_frame(&mut stream).await.unwrap();
    write_frame(&mut stream, MsgType::Files, &[]).await.unwrap();

    let run_bad = RunPayload {
        argv: vec!["/nonexistent/custom/shell/binary".to_string()],
        outputs: None,
        cwd: None,
        template: None,
        no_cache: false,
        env: None,
        toolchain: None,
        tty: true,
        cols: Some(80),
        rows: Some(24),
        raw_stdio: None,
    };
    write_json_frame(&mut stream, MsgType::Run, &run_bad)
        .await
        .unwrap();

    let exit_code;
    let mut had_stderr = false;
    loop {
        let (msg, payload) = read_frame(&mut stream).await.unwrap();
        if msg == MsgType::Log {
            let log: protocol::LogPayload = serde_json::from_slice(&payload).unwrap();
            if log.stream == "stderr" && log.data.contains("failed to spawn") {
                had_stderr = true;
            }
        } else if msg == MsgType::Result {
            let res: protocol::ResultPayload = serde_json::from_slice(&payload).unwrap();
            exit_code = Some(res.exit_code);
            break;
        }
    }

    assert_eq!(
        exit_code,
        Some(127),
        "Failed spawn must return exit code 127"
    );
    assert!(had_stderr, "Must log spawn failure to stderr");
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_compression_negotiation_zstd_and_none() {
    let token = "comp-tok".to_string();
    let remote_workdir = tempdir().unwrap();
    let (server_addr, _handle) =
        spawn_test_server(Some(token.clone()), remote_workdir.path().to_path_buf()).await;

    // Case 1: Negotiate zstd
    let mut stream = TcpStream::connect(&server_addr).await.unwrap();
    let hello_zstd = HelloPayload {
        token: token.clone(),
        project: "zstd-proj".to_string(),
        protocol_version: CURRENT_PROTOCOL_VERSION,
        compressions: Some(vec!["zstd".to_string(), "gzip".to_string()]),
    };
    write_json_frame(&mut stream, MsgType::Hello, &hello_zstd)
        .await
        .unwrap();
    let (msg, payload) = read_frame(&mut stream).await.unwrap();
    assert_eq!(msg, MsgType::HelloAck);
    let ack: HelloAckPayload = serde_json::from_slice(&payload).unwrap();
    assert!(ack.ok);
    assert_eq!(ack.compression.as_deref(), Some("zstd"));

    // Case 2: Negotiate none
    let mut stream2 = TcpStream::connect(&server_addr).await.unwrap();
    let hello_none = HelloPayload {
        token: token.clone(),
        project: "none-proj".to_string(),
        protocol_version: CURRENT_PROTOCOL_VERSION,
        compressions: Some(vec!["none".to_string()]),
    };
    write_json_frame(&mut stream2, MsgType::Hello, &hello_none)
        .await
        .unwrap();
    let (msg2, payload2) = read_frame(&mut stream2).await.unwrap();
    assert_eq!(msg2, MsgType::HelloAck);
    let ack2: HelloAckPayload = serde_json::from_slice(&payload2).unwrap();
    assert!(ack2.ok);
    assert_eq!(ack2.compression.as_deref(), Some("none"));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_cas_cross_project_zero_upload() {
    let token = "cas-e2e-tok".to_string();
    let remote_workdir = tempdir().unwrap();
    let (server_addr, _handle) =
        spawn_test_server(Some(token.clone()), remote_workdir.path().to_path_buf()).await;

    // 1. First project uploads "shared_library.txt"
    let local1 = tempdir().unwrap();
    let content = b"shared big binary content across projects";
    fs::write(local1.path().join("shared.txt"), content).unwrap();

    let meta = fileset::scan(local1.path(), &[]).unwrap();
    let entry = meta.get("shared.txt").unwrap();
    let file_hash = entry.hash.clone();

    let mut stream1 = TcpStream::connect(&server_addr).await.unwrap();
    let hello1 = HelloPayload {
        token: token.clone(),
        project: "cas-project-alpha".to_string(),
        protocol_version: CURRENT_PROTOCOL_VERSION,
        compressions: Some(vec!["zstd".to_string()]),
    };
    write_json_frame(&mut stream1, MsgType::Hello, &hello1)
        .await
        .unwrap();
    let _ = read_frame(&mut stream1).await.unwrap();

    let manifest1 = ManifestPayload {
        files: vec![FileEntry {
            path: "shared.txt".to_string(),
            hash: file_hash.clone(),
            size: content.len() as u64,
            mode: 0o644,
        }],
    };
    write_json_frame(&mut stream1, MsgType::Manifest, &manifest1)
        .await
        .unwrap();
    let (msg, need_bytes) = read_frame(&mut stream1).await.unwrap();
    assert_eq!(msg, MsgType::Need);
    let need1: NeedPayload = serde_json::from_slice(&need_bytes).unwrap();
    assert_eq!(need1.want, vec!["shared.txt"]);

    // Upload delta
    let delta1 = fileset::pack_tar(local1.path(), &need1.want).unwrap();
    write_frame(&mut stream1, MsgType::Files, &delta1)
        .await
        .unwrap();

    // Run simple command to complete alpha run
    let cmd = if cfg!(windows) {
        vec![
            "cmd.exe".to_string(),
            "/C".to_string(),
            "exit 0".to_string(),
        ]
    } else {
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "exit 0".to_string(),
        ]
    };
    let run = RunPayload {
        argv: cmd.clone(),
        outputs: None,
        cwd: None,
        template: None,
        no_cache: false,
        env: None,
        toolchain: None,
        tty: false,
        cols: None,
        rows: None,
        raw_stdio: None,
    };
    write_json_frame(&mut stream1, MsgType::Run, &run)
        .await
        .unwrap();
    loop {
        let (m, _) = read_frame(&mut stream1).await.unwrap();
        if m == MsgType::Result {
            break;
        }
    }

    // 2. Second project (brand new project!) contains the same file hash
    let mut stream2 = TcpStream::connect(&server_addr).await.unwrap();
    let hello2 = HelloPayload {
        token: token.clone(),
        project: "cas-project-beta".to_string(),
        protocol_version: CURRENT_PROTOCOL_VERSION,
        compressions: Some(vec!["zstd".to_string()]),
    };
    write_json_frame(&mut stream2, MsgType::Hello, &hello2)
        .await
        .unwrap();
    let _ = read_frame(&mut stream2).await.unwrap();

    let manifest2 = ManifestPayload {
        files: vec![FileEntry {
            path: "shared.txt".to_string(),
            hash: file_hash.clone(),
            size: content.len() as u64,
            mode: 0o644,
        }],
    };
    write_json_frame(&mut stream2, MsgType::Manifest, &manifest2)
        .await
        .unwrap();

    let (msg2, need_bytes2) = read_frame(&mut stream2).await.unwrap();
    assert_eq!(msg2, MsgType::Need);
    let need2: NeedPayload = serde_json::from_slice(&need_bytes2).unwrap();

    // Verify: Agent hydrated from CAS! need2.want must be EMPTY!
    assert!(
        need2.want.is_empty(),
        "Agent must hydrate shared.txt from CAS, want list should be empty!"
    );

    // Client sends 0 delta bytes
    write_frame(&mut stream2, MsgType::Files, &[])
        .await
        .unwrap();

    write_json_frame(&mut stream2, MsgType::Run, &run)
        .await
        .unwrap();
    loop {
        let (m, _) = read_frame(&mut stream2).await.unwrap();
        if m == MsgType::Result {
            break;
        }
    }

    // Verify that beta's remote workspace actually contains the file with matching content!
    let beta_ws = workspace::resolve_workspace_dir(remote_workdir.path(), "cas-project-beta");
    assert_eq!(
        fs::read(beta_ws.join("shared.txt")).unwrap(),
        content,
        "File materialized from CAS must have identical content"
    );
}

#[tokio::test]
async fn test_e2e_tls_self_signed_with_fingerprint_verification() {
    let remote_workdir = tempdir().unwrap();
    let cert =
        protocol::generate_self_signed_cert(vec!["localhost".into(), "127.0.0.1".into()]).unwrap();
    let server_cfg = protocol::create_server_config(&cert.cert_pem, &cert.key_pem, None).unwrap();
    let acceptor = protocol::TlsAcceptor::from(server_cfg);

    let (server_addr, _server_handle) = spawn_agent_tls(
        remote_workdir.path().to_path_buf(),
        Some("tls-token".into()),
        acceptor,
    )
    .await;

    let tls_config = config::TlsConfig {
        enabled: true,
        fingerprint: Some(cert.fingerprint.clone()),
        ..Default::default()
    };

    let mut stream = fh::connect_to_agent(&server_addr, Some(&tls_config))
        .await
        .unwrap();

    let hello = HelloPayload {
        token: "tls-token".into(),
        project: "tls-project".into(),
        protocol_version: CURRENT_PROTOCOL_VERSION,
        compressions: None,
    };
    write_json_frame(&mut stream, MsgType::Hello, &hello)
        .await
        .unwrap();

    let (msg, ack_bytes) = read_frame(&mut stream).await.unwrap();
    assert_eq!(msg, MsgType::HelloAck);
    let ack: HelloAckPayload = serde_json::from_slice(&ack_bytes).unwrap();
    assert!(ack.ok);

    let manifest = ManifestPayload { files: Vec::new() };
    write_json_frame(&mut stream, MsgType::Manifest, &manifest)
        .await
        .unwrap();

    let (msg, _) = read_frame(&mut stream).await.unwrap();
    assert_eq!(msg, MsgType::Need);

    write_frame(&mut stream, MsgType::Files, &[]).await.unwrap();

    let run = RunPayload {
        argv: vec!["echo".into(), "tls connection works!".into()],
        outputs: None,
        cwd: None,
        template: None,
        no_cache: false,
        env: None,
        toolchain: None,
        tty: false,
        cols: None,
        rows: None,
        raw_stdio: None,
    };
    write_json_frame(&mut stream, MsgType::Run, &run)
        .await
        .unwrap();

    let mut output_logs = Vec::new();
    loop {
        let (msg, payload) = read_frame(&mut stream).await.unwrap();
        if msg == MsgType::Log {
            let log: LogPayload = serde_json::from_slice(&payload).unwrap();
            output_logs.push(log.data);
        } else if msg == MsgType::Result {
            let res: ResultPayload = serde_json::from_slice(&payload).unwrap();
            assert_eq!(res.exit_code, 0);
            break;
        }
    }

    let joined = output_logs.join("");
    assert!(joined.contains("tls connection works!"));
}

#[tokio::test]
async fn test_e2e_tls_insecure_flag() {
    let remote_workdir = tempdir().unwrap();
    let cert =
        protocol::generate_self_signed_cert(vec!["localhost".into(), "127.0.0.1".into()]).unwrap();
    let server_cfg = protocol::create_server_config(&cert.cert_pem, &cert.key_pem, None).unwrap();
    let acceptor = protocol::TlsAcceptor::from(server_cfg);

    let (server_addr, _server_handle) = spawn_agent_tls(
        remote_workdir.path().to_path_buf(),
        Some("tls-token".into()),
        acceptor,
    )
    .await;

    let tls_config = config::TlsConfig {
        enabled: true,
        insecure: true,
        ..Default::default()
    };

    let mut stream = fh::connect_to_agent(&server_addr, Some(&tls_config))
        .await
        .unwrap();

    let hello = HelloPayload {
        token: "tls-token".into(),
        project: "tls-insecure-project".into(),
        protocol_version: CURRENT_PROTOCOL_VERSION,
        compressions: None,
    };
    write_json_frame(&mut stream, MsgType::Hello, &hello)
        .await
        .unwrap();

    let (msg, ack_bytes) = read_frame(&mut stream).await.unwrap();
    assert_eq!(msg, MsgType::HelloAck);
    let ack: HelloAckPayload = serde_json::from_slice(&ack_bytes).unwrap();
    assert!(ack.ok);
}

#[tokio::test]
async fn test_e2e_tls_mismatched_fingerprint_rejected() {
    let remote_workdir = tempdir().unwrap();
    let cert =
        protocol::generate_self_signed_cert(vec!["localhost".into(), "127.0.0.1".into()]).unwrap();
    let server_cfg = protocol::create_server_config(&cert.cert_pem, &cert.key_pem, None).unwrap();
    let acceptor = protocol::TlsAcceptor::from(server_cfg);

    let (server_addr, _server_handle) = spawn_agent_tls(
        remote_workdir.path().to_path_buf(),
        Some("tls-token".into()),
        acceptor,
    )
    .await;

    let bogus_fingerprint =
        "0000000000000000000000000000000000000000000000000000000000000000".to_string();
    let tls_config = config::TlsConfig {
        enabled: true,
        fingerprint: Some(bogus_fingerprint),
        ..Default::default()
    };

    let conn_res = fh::connect_to_agent(&server_addr, Some(&tls_config)).await;
    assert!(
        conn_res.is_err(),
        "Connection with mismatched TLS fingerprint must be rejected!"
    );
}

#[tokio::test]
async fn test_e2e_toolchain_rustup_and_python_env_injection() {
    let remote_workdir = tempdir().unwrap();
    let (server_addr, _server_handle) = spawn_test_server(
        Some("toolchain-token".into()),
        remote_workdir.path().to_path_buf(),
    )
    .await;

    let mut stream = TcpStream::connect(&server_addr).await.unwrap();

    let hello = HelloPayload {
        token: "toolchain-token".into(),
        project: "toolchain-project".into(),
        protocol_version: CURRENT_PROTOCOL_VERSION,
        compressions: None,
    };
    write_json_frame(&mut stream, MsgType::Hello, &hello)
        .await
        .unwrap();

    let (msg, ack_bytes) = read_frame(&mut stream).await.unwrap();
    assert_eq!(msg, MsgType::HelloAck);
    let ack: HelloAckPayload = serde_json::from_slice(&ack_bytes).unwrap();
    assert!(ack.ok);

    let manifest = ManifestPayload { files: Vec::new() };
    write_json_frame(&mut stream, MsgType::Manifest, &manifest)
        .await
        .unwrap();

    let (msg, _) = read_frame(&mut stream).await.unwrap();
    assert_eq!(msg, MsgType::Need);

    write_frame(&mut stream, MsgType::Files, &[]).await.unwrap();

    let mut toolchain_map = std::collections::HashMap::new();
    toolchain_map.insert("rust".to_string(), "nightly-2026".to_string());
    toolchain_map.insert("python".to_string(), "3.12.1".to_string());

    let run = RunPayload {
        argv: vec![
            "sh".into(),
            "-c".into(),
            "echo RUSTUP=$RUSTUP_TOOLCHAIN PYENV=$PYENV_VERSION TC_RUST=$FARHAND_TOOLCHAIN_RUST"
                .into(),
        ],
        outputs: None,
        cwd: None,
        template: None,
        no_cache: false,
        env: None,
        toolchain: Some(toolchain_map),
        tty: false,
        cols: None,
        rows: None,
        raw_stdio: None,
    };
    write_json_frame(&mut stream, MsgType::Run, &run)
        .await
        .unwrap();

    let mut output_logs = Vec::new();
    loop {
        let (msg, payload) = read_frame(&mut stream).await.unwrap();
        if msg == MsgType::Log {
            let log: LogPayload = serde_json::from_slice(&payload).unwrap();
            output_logs.push(log.data);
        } else if msg == MsgType::Result {
            let res: ResultPayload = serde_json::from_slice(&payload).unwrap();
            assert_eq!(res.exit_code, 0);
            break;
        }
    }

    let joined = output_logs.join("");
    assert!(joined.contains("RUSTUP=nightly-2026"));
    assert!(joined.contains("PYENV=3.12.1"));
    assert!(joined.contains("TC_RUST=nightly-2026"));
}

#[tokio::test]
async fn test_enriched_status_and_top_snapshot() {
    let workdir = tempfile::tempdir().unwrap();
    let (addr, _server_handle) = spawn_test_server(
        Some("test-token-secret".into()),
        workdir.path().to_path_buf(),
    )
    .await;

    let status = fh::probe_agent_status(&addr, "test-token-secret", None)
        .await
        .expect("probe should succeed");

    assert!(!status.hostname.is_empty());
    assert!(status.cpu_count.unwrap_or(0) >= 1);
    assert!(status.uptime_secs.is_some());
    assert_eq!(status.active_runs, 0);
    assert_eq!(status.queue_depth, 0);
    assert!(status.active_builds.is_some());
    assert!(status.active_builds.as_ref().unwrap().is_empty());

    // Verify snapshot renderer executes without panicking
    fh::top::render_snapshot(&status, &addr);
}

#[tokio::test]
async fn test_active_build_registry_tracking() {
    let workdir = tempfile::tempdir().unwrap();
    let (addr, _server_handle) = spawn_test_server(
        Some("test-token-secret".into()),
        workdir.path().to_path_buf(),
    )
    .await;
    let mut stream = TcpStream::connect(&addr).await.unwrap();

    let hello = HelloPayload {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        token: "test-token-secret".into(),
        project: "active-build-test".into(),
        compressions: None,
    };
    write_json_frame(&mut stream, MsgType::Hello, &hello)
        .await
        .unwrap();
    let (msg, payload) = read_frame(&mut stream).await.unwrap();
    assert_eq!(msg, MsgType::HelloAck);
    let ack: HelloAckPayload = serde_json::from_slice(&payload).unwrap();
    assert!(ack.ok);
    assert!(ack.remote_workdir.is_some());

    // Manifest
    let manifest = ManifestPayload { files: vec![] };
    write_json_frame(&mut stream, MsgType::Manifest, &manifest)
        .await
        .unwrap();
    let (msg, _) = read_frame(&mut stream).await.unwrap();
    assert_eq!(msg, MsgType::Need);
    write_frame(&mut stream, MsgType::Files, &[]).await.unwrap();

    // Spawn a long-running command (sleep 1s)
    let run = RunPayload {
        argv: vec![
            env!("CARGO_BIN_EXE_fh").into(),
            "__test_sleep".into(),
            "1000".into(),
        ],
        outputs: None,
        cwd: None,
        template: None,
        no_cache: false,
        env: None,
        toolchain: None,
        tty: false,
        cols: None,
        rows: None,
        raw_stdio: None,
    };
    write_json_frame(&mut stream, MsgType::Run, &run)
        .await
        .unwrap();

    // Small delay to let child process spawn
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    // Probe status while run is active
    let status_mid = fh::probe_agent_status(&addr, "test-token-secret", None)
        .await
        .expect("probe should succeed while run is active");

    let builds = status_mid
        .active_builds
        .as_ref()
        .expect("active builds present");
    assert_eq!(builds.len(), 1, "expected exactly 1 active build");
    assert_eq!(builds[0].project, "active-build-test");

    // Finish reading from stream
    loop {
        let (msg, payload) = read_frame(&mut stream).await.unwrap();
        if msg == MsgType::Result {
            let res: ResultPayload = serde_json::from_slice(&payload).unwrap();
            assert_eq!(res.exit_code, 0);
            break;
        }
    }

    // Small delay for cleanup guard
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Probe status again: active builds should now be empty
    let status_post = fh::probe_agent_status(&addr, "test-token-secret", None)
        .await
        .expect("probe should succeed");
    assert!(status_post.active_builds.as_ref().unwrap().is_empty());
}

#[tokio::test]
async fn test_lsp_raw_stdio_echo() {
    let workdir = tempfile::tempdir().unwrap();
    let (addr, _server_handle) = spawn_test_server(
        Some("test-token-secret".into()),
        workdir.path().to_path_buf(),
    )
    .await;
    let mut stream = TcpStream::connect(&addr).await.unwrap();

    let hello = HelloPayload {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        token: "test-token-secret".into(),
        project: "lsp-echo-test".into(),
        compressions: None,
    };
    write_json_frame(&mut stream, MsgType::Hello, &hello)
        .await
        .unwrap();
    let (msg, payload) = read_frame(&mut stream).await.unwrap();
    assert_eq!(msg, MsgType::HelloAck);
    let ack: HelloAckPayload = serde_json::from_slice(&payload).unwrap();
    assert!(ack.ok);
    assert!(ack.remote_workdir.is_some());

    let manifest = ManifestPayload { files: vec![] };
    write_json_frame(&mut stream, MsgType::Manifest, &manifest)
        .await
        .unwrap();
    let (msg, _) = read_frame(&mut stream).await.unwrap();
    assert_eq!(msg, MsgType::Need);
    write_frame(&mut stream, MsgType::Files, &[]).await.unwrap();

    // Start echo process with raw_stdio: true
    let run = RunPayload {
        argv: vec![env!("CARGO_BIN_EXE_fh").into(), "__test_echo".into()],
        outputs: None,
        cwd: None,
        template: None,
        no_cache: true,
        env: None,
        toolchain: None,
        tty: false,
        cols: None,
        rows: None,
        raw_stdio: Some(true),
    };
    write_json_frame(&mut stream, MsgType::Run, &run)
        .await
        .unwrap();

    let sample_msg = "Content-Length: 26\r\n\r\n{\"jsonrpc\":\"2.0\",\"id\":1}";
    write_frame(&mut stream, MsgType::Stdin, sample_msg.as_bytes())
        .await
        .unwrap();

    // Read echoed log frame
    let (msg, payload) =
        tokio::time::timeout(std::time::Duration::from_secs(3), read_frame(&mut stream))
            .await
            .expect("timeout waiting for echoed raw stdio")
            .unwrap();

    assert_eq!(msg, MsgType::Log);
    let log: LogPayload = serde_json::from_slice(&payload).unwrap();
    assert!(log.data.contains("Content-Length: 26"));
}

#[tokio::test]
async fn test_connection_limit_closes_excess_connections() {
    let workdir = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();

    // Server capped at exactly one concurrent connection.
    let workdir_path = workdir.path().to_path_buf();
    let _handle = tokio::spawn(async move {
        let _ = fhd::run_server(
            listener,
            None,
            workdir_path,
            None,
            None,
            Vec::new(),
            None,
            None,
            false,
            None,
            Some(1),
            None,
            None,
        )
        .await;
    });

    // First connection occupies the only slot; leave it idle.
    let mut first = TcpStream::connect(&addr).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Second connection must be closed promptly by the server (EOF, no data).
    let mut second = TcpStream::connect(&addr).await.unwrap();
    let mut sink = [0u8; 16];
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        tokio::io::AsyncReadExt::read(&mut second, &mut sink),
    )
    .await
    .expect("server did not close the excess connection in time");
    assert_eq!(result.unwrap(), 0, "excess connection should see EOF");

    // First connection still has its slot — the socket stays open (a zero
    // read would also surface here, so write-check instead: try to read with
    // a short timeout and expect a timeout, i.e. the server did not close it).
    let mut sink = [0u8; 16];
    let kept_open = tokio::time::timeout(
        std::time::Duration::from_millis(300),
        tokio::io::AsyncReadExt::read(&mut first, &mut sink),
    )
    .await;
    assert!(kept_open.is_err(), "first connection should still be open");
    drop(first);
}

#[tokio::test]
async fn test_e2e_mtls_client_cert_required_and_verified() {
    let remote_workdir = tempdir().unwrap();
    let server_cert =
        protocol::generate_self_signed_cert(vec!["localhost".into(), "127.0.0.1".into()]).unwrap();
    let client_identity = protocol::generate_client_identity().unwrap();

    // Server requires and verifies client certificates signed by the client CA.
    let server_cfg = protocol::create_server_config(
        &server_cert.cert_pem,
        &server_cert.key_pem,
        Some(&client_identity.ca_pem),
    )
    .unwrap();
    let acceptor = protocol::TlsAcceptor::from(server_cfg);

    let (server_addr, _server_handle) = spawn_agent_tls(
        remote_workdir.path().to_path_buf(),
        Some("mtls-token".into()),
        acceptor,
    )
    .await;

    // Write the client certificate/key to disk for the client TLS config.
    let cert_dir = tempdir().unwrap();
    let client_cert_path = cert_dir.path().join("client.pem");
    let client_key_path = cert_dir.path().join("client.key");
    fs::write(&client_cert_path, &client_identity.cert_pem).unwrap();
    fs::write(&client_key_path, &client_identity.key_pem).unwrap();

    let mtls_config = config::TlsConfig {
        enabled: true,
        fingerprint: Some(server_cert.fingerprint.clone()),
        cert: Some(client_cert_path.to_string_lossy().to_string()),
        key: Some(client_key_path.to_string_lossy().to_string()),
        ..Default::default()
    };

    // 1. Client WITH a valid client certificate: full roundtrip succeeds.
    {
        let mut stream = fh::connect_to_agent(&server_addr, Some(&mtls_config))
            .await
            .expect("mTLS handshake with a valid client certificate must succeed");

        let hello = HelloPayload {
            token: "mtls-token".into(),
            project: "mtls-project".into(),
            protocol_version: CURRENT_PROTOCOL_VERSION,
            compressions: None,
        };
        write_json_frame(&mut stream, MsgType::Hello, &hello)
            .await
            .unwrap();

        let (msg, ack_bytes) = read_frame(&mut stream).await.unwrap();
        assert_eq!(msg, MsgType::HelloAck);
        let ack: HelloAckPayload = serde_json::from_slice(&ack_bytes).unwrap();
        assert!(
            ack.ok,
            "mutual-TLS handshake with a valid client cert should pass"
        );

        write_json_frame(
            &mut stream,
            MsgType::Manifest,
            &ManifestPayload { files: Vec::new() },
        )
        .await
        .unwrap();
        let (msg, _) = read_frame(&mut stream).await.unwrap();
        assert_eq!(msg, MsgType::Need);
        write_frame(&mut stream, MsgType::Files, &[]).await.unwrap();

        let run = RunPayload {
            argv: vec!["echo".into(), "mtls works!".into()],
            outputs: None,
            cwd: None,
            template: None,
            no_cache: false,
            env: None,
            toolchain: None,
            tty: false,
            cols: None,
            rows: None,
            raw_stdio: None,
        };
        write_json_frame(&mut stream, MsgType::Run, &run)
            .await
            .unwrap();

        loop {
            let (msg, payload) = read_frame(&mut stream).await.unwrap();
            if msg == MsgType::Result {
                let res: ResultPayload = serde_json::from_slice(&payload).unwrap();
                assert_eq!(res.exit_code, 0);
                break;
            }
        }
    }

    // 2. Client WITHOUT a client certificate: the server aborts the handshake
    // (NoCertificatesPresented alert). The client may observe it either at
    // connect time or on the first frame exchange — both count as rejection.
    let no_client_cert = config::TlsConfig {
        enabled: true,
        fingerprint: Some(server_cert.fingerprint.clone()),
        ..Default::default()
    };
    let rejected = match fh::connect_to_agent(&server_addr, Some(&no_client_cert)).await {
        Err(_) => true,
        Ok(mut stream) => {
            let hello = HelloPayload {
                token: "mtls-token".into(),
                project: "mtls-project".into(),
                protocol_version: CURRENT_PROTOCOL_VERSION,
                compressions: None,
            };
            match write_json_frame(&mut stream, MsgType::Hello, &hello).await {
                Err(_) => true,
                Ok(()) => read_frame(&mut stream).await.is_err(),
            }
        }
    };
    assert!(
        rejected,
        "server must reject clients without a client certificate in mTLS mode"
    );
}

#[tokio::test]
async fn test_e2e_queued_client_disconnect_frees_slot() {
    let workdir = tempdir().unwrap();
    let (server_addr, _server_handle) =
        spawn_test_server(Some("q-token".into()), workdir.path().to_path_buf()).await;

    let run_payload = |argv: Vec<String>| RunPayload {
        argv,
        outputs: None,
        cwd: None,
        template: None,
        no_cache: false,
        env: None,
        toolchain: None,
        tty: false,
        cols: None,
        rows: None,
        raw_stdio: None,
    };
    let manifest = ManifestPayload { files: Vec::new() };

    // Run 1 holds the project lock with a long sleep.
    let mut run1 = TcpStream::connect(&server_addr).await.unwrap();
    let hello = HelloPayload {
        token: "q-token".into(),
        project: "queued-project".into(),
        protocol_version: CURRENT_PROTOCOL_VERSION,
        compressions: None,
    };
    write_json_frame(&mut run1, MsgType::Hello, &hello)
        .await
        .unwrap();
    let (msg, _) = read_frame(&mut run1).await.unwrap();
    assert_eq!(msg, MsgType::HelloAck);
    write_json_frame(&mut run1, MsgType::Manifest, &manifest)
        .await
        .unwrap();
    let (msg, _) = read_frame(&mut run1).await.unwrap();
    assert_eq!(msg, MsgType::Need);
    write_frame(&mut run1, MsgType::Files, &[]).await.unwrap();
    let sleep_argv = if cfg!(windows) {
        vec![
            "cmd.exe".into(),
            "/C".into(),
            "ping -n 3 127.0.0.1 > nul".into(),
        ]
    } else {
        vec!["sleep".into(), "2".into()]
    };
    write_json_frame(&mut run1, MsgType::Run, &run_payload(sleep_argv))
        .await
        .unwrap();

    // Give run 1 time to start (lock now held).
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    // Run 2 connects for the same project, sends its manifest, and gets
    // queued on the project lock.
    let mut run2 = TcpStream::connect(&server_addr).await.unwrap();
    write_json_frame(&mut run2, MsgType::Hello, &hello)
        .await
        .unwrap();
    let (msg, _) = read_frame(&mut run2).await.unwrap();
    assert_eq!(msg, MsgType::HelloAck);
    write_json_frame(&mut run2, MsgType::Manifest, &manifest)
        .await
        .unwrap();

    // While queued, run 2's client dies. The daemon's watchdog must free
    // the slot instead of holding it forever.
    let (msg2, _) = read_frame(&mut run2).await.unwrap();
    assert_eq!(msg2, MsgType::Queued);
    tokio::io::AsyncWriteExt::shutdown(&mut run2).await.unwrap();

    // The server must notice the disconnect and close its side (EOF), and
    // the queue depth must return to zero — the key regression assertion.
    let mut buf = [0u8; 8];
    let n = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        tokio::io::AsyncReadExt::read(&mut run2, &mut buf),
    )
    .await
    .expect("server did not close the disconnected client's connection in time")
    .unwrap();
    assert_eq!(n, 0, "expected EOF from the server after client shutdown");

    let mut probe = TcpStream::connect(&server_addr).await.unwrap();
    let status_req = protocol::StatusRequestPayload {
        token: "q-token".into(),
    };
    write_json_frame(&mut probe, MsgType::Status, &status_req)
        .await
        .unwrap();
    let (msg, payload) = read_frame(&mut probe).await.unwrap();
    assert_eq!(msg, MsgType::StatusResp);
    let st: protocol::StatusResponsePayload = serde_json::from_slice(&payload).unwrap();
    assert_eq!(
        st.queue_depth, 0,
        "disconnected client must free its queue slot"
    );

    // Run 3 must still be servable: it queues behind run 1 (which sleeps 2s)
    // and proceeds once the lock frees.
    let mut run3 = TcpStream::connect(&server_addr).await.unwrap();
    write_json_frame(&mut run3, MsgType::Hello, &hello)
        .await
        .unwrap();
    let (msg, payload) = read_frame(&mut run3).await.unwrap();
    assert_eq!(msg, MsgType::HelloAck);
    let ack: HelloAckPayload = serde_json::from_slice(&payload).unwrap();
    assert!(ack.ok, "third client must be accepted (slot was freed)");

    write_json_frame(&mut run3, MsgType::Manifest, &manifest)
        .await
        .unwrap();
    // The project lock may still be held by run 1 — accept QUEUED frames
    // until the NEED arrives.
    loop {
        let (m, _) = read_frame(&mut run3).await.unwrap();
        if m == MsgType::Need {
            break;
        }
        assert_eq!(m, MsgType::Queued, "unexpected frame while queued");
    }
    write_frame(&mut run3, MsgType::Files, &[]).await.unwrap();

    let run3_cmd = vec![
        if cfg!(windows) {
            "cmd".into()
        } else {
            "echo".into()
        },
        if cfg!(windows) {
            "/C".into()
        } else {
            "done".into()
        },
    ];
    write_json_frame(&mut run3, MsgType::Run, &run_payload(run3_cmd))
        .await
        .unwrap();
    loop {
        let (msg, payload) = read_frame(&mut run3).await.unwrap();
        if msg == MsgType::Result {
            let res: ResultPayload = serde_json::from_slice(&payload).unwrap();
            assert_eq!(res.exit_code, 0);
            break;
        }
    }
}

#[tokio::test]
async fn test_e2e_queue_full_rejection() {
    let workdir = tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let wd = workdir.path().to_path_buf();
    let _handle = tokio::spawn(async move {
        let _ = fhd::run_server(
            listener,
            Some("qf-token".into()),
            wd,
            None,
            None,
            Vec::new(),
            None,
            None,
            false,
            None,
            None,
            Some(1), // max_queued_runs = 1
            None,
        )
        .await;
    });

    let run_payload = || RunPayload {
        argv: vec!["sleep".into(), "2".into()],
        outputs: None,
        cwd: None,
        template: None,
        no_cache: false,
        env: None,
        toolchain: None,
        tty: false,
        cols: None,
        rows: None,
        raw_stdio: None,
    };
    let manifest = ManifestPayload { files: Vec::new() };
    let hello = HelloPayload {
        token: "qf-token".into(),
        project: "full-queue-project".into(),
        protocol_version: CURRENT_PROTOCOL_VERSION,
        compressions: None,
    };

    // Run 1 holds the project lock.
    let mut run1 = TcpStream::connect(&addr).await.unwrap();
    write_json_frame(&mut run1, MsgType::Hello, &hello)
        .await
        .unwrap();
    let (msg, payload) = read_frame(&mut run1).await.unwrap();
    assert_eq!(msg, MsgType::HelloAck);
    let ack: HelloAckPayload = serde_json::from_slice(&payload).unwrap();
    assert!(ack.ok);
    write_json_frame(&mut run1, MsgType::Manifest, &manifest)
        .await
        .unwrap();
    let (msg, _) = read_frame(&mut run1).await.unwrap();
    assert_eq!(msg, MsgType::Need);
    write_frame(&mut run1, MsgType::Files, &[]).await.unwrap();
    write_json_frame(&mut run1, MsgType::Run, &run_payload())
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    // Run 2 fills the single queue slot.
    let mut run2 = TcpStream::connect(&addr).await.unwrap();
    write_json_frame(&mut run2, MsgType::Hello, &hello)
        .await
        .unwrap();
    let (msg, _) = read_frame(&mut run2).await.unwrap();
    assert_eq!(msg, MsgType::HelloAck);
    write_json_frame(&mut run2, MsgType::Manifest, &manifest)
        .await
        .unwrap();
    let (msg2, _) = read_frame(&mut run2).await.unwrap();
    assert_eq!(msg2, MsgType::Queued, "run 2 should be queued");

    // Run 3 is rejected: the queue is full. The rejection HelloAck (ok:false)
    // arrives AFTER the manifest — the first ack is the plain handshake.
    let mut run3 = TcpStream::connect(&addr).await.unwrap();
    write_json_frame(&mut run3, MsgType::Hello, &hello)
        .await
        .unwrap();
    let (msg, _) = read_frame(&mut run3).await.unwrap();
    assert_eq!(msg, MsgType::HelloAck);

    write_json_frame(&mut run3, MsgType::Manifest, &manifest)
        .await
        .unwrap();
    let (msg, payload) = read_frame(&mut run3).await.unwrap();
    assert_eq!(msg, MsgType::HelloAck);
    let ack: HelloAckPayload = serde_json::from_slice(&payload).unwrap();
    assert!(!ack.ok, "run 3 must be rejected when the queue is full");
    assert!(
        ack.error.unwrap().contains("full"),
        "rejection must explain the queue is full"
    );
}
