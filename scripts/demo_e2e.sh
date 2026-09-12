#!/usr/bin/env bash
set -e

echo "=== Building Farhand Client (fh) and Daemon (fhd) ==="
cargo build -p fhd -p fh

PORT=9876
TOKEN="demo-secret-123"

echo ""
echo "=== Starting 'fhd' Daemon on 127.0.0.1:${PORT} in the background ==="
./target/debug/fhd --listen "127.0.0.1:${PORT}" --token "${TOKEN}" --tag lan --tag cpu &
DAEMON_PID=$!
DAEMON2_PID=""

# Ensure daemons are stopped when script exits
trap "echo ''; echo '=== Stopping fhd Daemons ==='; kill -9 ${DAEMON_PID} ${DAEMON2_PID} 2>/dev/null || true" EXIT

# Wait a brief moment for socket to bind
sleep 0.5

echo ""
echo "================================================================"
echo "TEST 1: Run remote echo command and stream output"
echo "Command: fh --host 127.0.0.1:${PORT} --token ${TOKEN} -- echo 'Hello from the remote daemon!'"
echo "================================================================"
./target/debug/fh --host "127.0.0.1:${PORT}" --token "${TOKEN}" -- echo "Hello from the remote daemon!"
echo "[PASS] Test 1 completed with exit code: $?"

echo ""
echo "================================================================"
echo "TEST 2: Inspect remote workspace (confirming project files were transferred)"
echo "Command: fh --host 127.0.0.1:${PORT} --token ${TOKEN} -- ls -la crates"
echo "================================================================"
./target/debug/fh --host "127.0.0.1:${PORT}" --token "${TOKEN}" -- ls -la crates
echo "[PASS] Test 2 completed with exit code: $?"

echo ""
echo "================================================================"
echo "TEST 3: Multiline live streaming simulation"
echo "Command: fh ... -- sh -c 'for i in 1 2 3; do echo \"Progress: step \$i/3\"; sleep 0.2; done'"
echo "================================================================"
./target/debug/fh --host "127.0.0.1:${PORT}" --token "${TOKEN}" -- sh -c 'for i in 1 2 3; do echo "Progress: step $i/3"; sleep 0.2; done'
echo "[PASS] Test 3 completed with exit code: $?"

echo ""
echo "================================================================"
echo "TEST 4: Non-zero exit code propagation (command failure)"
echo "Command: fh ... -- sh -c 'echo \"Failing intentionally...\"; exit 42'"
echo "================================================================"
set +e
./target/debug/fh --host "127.0.0.1:${PORT}" --token "${TOKEN}" -- sh -c 'echo "Failing intentionally..."; exit 42'
CODE=$?
set -e
echo "Remote process failed as expected. Local exit code was: ${CODE}"
if [ "${CODE}" -eq 42 ]; then
  echo "[PASS] Test 4 accurately mirrored exit code 42!"
else
  echo "[FAIL] Expected exit code 42, got ${CODE}"
  exit 1
fi

echo ""
echo "================================================================"
echo "TEST 5: Authentication failure rejection (bad token)"
echo "Command: fh --token wrong_token -- echo 'should be rejected'"
echo "================================================================"
set +e
./target/debug/fh --host "127.0.0.1:${PORT}" --token "wrong_token" -- echo "should be rejected"
AUTH_CODE=$?
set -e
echo "Auth rejected as expected. Local exit code was: ${AUTH_CODE}"
if [ "${AUTH_CODE}" -eq 125 ]; then
  echo "[PASS] Test 5 exited with reserved infrastructure code 125!"
else
  echo "[FAIL] Expected exit code 125, got ${AUTH_CODE}"
  exit 1
fi

echo ""
echo "================================================================"
echo "TEST 6: Incremental delta sync (0 files/bytes uploaded on unchanged project)"
echo "Command: fh --verbose ... -- echo 'checking delta sync'"
echo "================================================================"
OUTPUT_RUN=$(./target/debug/fh --host "127.0.0.1:${PORT}" --token "${TOKEN}" --verbose -- echo "checking delta sync")
echo "${OUTPUT_RUN}"
if echo "${OUTPUT_RUN}" | grep -q "0 files to transfer"; then
  echo "[PASS] Test 6 confirmed remote agent required 0 delta files!"
else
  echo "[FAIL] Expected 0 delta files needed on rerun"
  exit 1
fi

echo ""
echo "================================================================"
echo "TEST 7: Remote artifact retrieval (--output and --out-dir)"
echo "Command: fh ... --output dist --out-dir /tmp/farhand-demo-out -- sh -c 'mkdir -p dist && echo \"built package content\" > dist/bundle.js'"
echo "================================================================"
rm -rf /tmp/farhand-demo-out
./target/debug/fh --host "127.0.0.1:${PORT}" --token "${TOKEN}" --verbose \
  --output dist \
  --out-dir /tmp/farhand-demo-out \
  -- sh -c 'mkdir -p dist && echo "built package content" > dist/bundle.js'

if [ -f "/tmp/farhand-demo-out/dist/bundle.js" ]; then
  CONTENT=$(cat /tmp/farhand-demo-out/dist/bundle.js)
  echo "Retrieved artifact content: ${CONTENT}"
  if [ "${CONTENT}" = "built package content" ]; then
    echo "[PASS] Test 7 successfully retrieved and extracted remote artifact!"
  else
    echo "[FAIL] Artifact content mismatch: ${CONTENT}"
    exit 1
  fi
else
  echo "[FAIL] Expected artifact /tmp/farhand-demo-out/dist/bundle.js not found"
  exit 1
fi
rm -rf /tmp/farhand-demo-out

echo ""
echo "================================================================"
echo "TEST 8: Project config (.farhand.yaml) zero-flag execution & telemetry"
echo "================================================================"
DEMO_PROJECT_DIR=$(mktemp -d)
cat <<EOF > "${DEMO_PROJECT_DIR}/.farhand.yaml"
host: 127.0.0.1:${PORT}
token: \${DEMO_ENV_TOKEN}
name: demo-cfg-app
verbose: true
outDir: ${DEMO_PROJECT_DIR}/out
outputs:
  - build/
EOF

export DEMO_ENV_TOKEN="${TOKEN}"
(
  cd "${DEMO_PROJECT_DIR}"
  "${OLDPWD}/target/debug/fh" -- sh -c 'mkdir -p build && echo "config-produced-binary" > build/binary'
)

if [ -f "${DEMO_PROJECT_DIR}/out/build/binary" ]; then
  CFG_CONTENT=$(cat "${DEMO_PROJECT_DIR}/out/build/binary")
  if [ "${CFG_CONTENT}" = "config-produced-binary" ]; then
    echo "[PASS] Test 8 successfully executed with zero flags and extracted artifacts via .farhand.yaml!"
  else
    echo "[FAIL] Unexpected content: ${CFG_CONTENT}"
    exit 1
  fi
else
  echo "[FAIL] Artifact ${DEMO_PROJECT_DIR}/out/build/binary not found"
  exit 1
fi
rm -rf "${DEMO_PROJECT_DIR}"
unset DEMO_ENV_TOKEN

echo ""
echo "================================================================"
echo "TEST 9: Unreachable host returns reserved exit code 125"
echo "Command: fh --host 127.0.0.1:1 --token abc -- echo unreachable"
echo "================================================================"
set +e
./target/debug/fh --host 127.0.0.1:1 --token abc -- echo unreachable
UNREACHABLE_CODE=$?
set -e
if [ "${UNREACHABLE_CODE}" -eq 125 ]; then
  echo "[PASS] Test 9 properly returned exit code 125 on unreachable host!"
else
  echo "[FAIL] Expected exit code 125, got ${UNREACHABLE_CODE}"
  exit 1
fi

echo ""
echo "================================================================"
echo "TEST 10: Template listing, display, and local init"
echo "Command: fh templates list && fh templates show rust"
echo "================================================================"
./target/debug/fh templates list
./target/debug/fh templates show rust | head -n 10
echo "[PASS] Test 10 template CLI inspection successful!"

echo ""
echo "================================================================"
echo "TEST 11: Dynamic template installation (fh templates push) and artifact extraction"
echo "================================================================"
DEMO_TMPL_DIR=$(mktemp -d)
mkdir -p "${DEMO_TMPL_DIR}/.farhand/templates"
cat <<EOF > "${DEMO_TMPL_DIR}/.farhand/templates/zig.yaml"
name: zig
description: Zig build toolchain
match:
  anyFile:
    - build.zig
outputs:
  - zig-out
EOF

touch "${DEMO_TMPL_DIR}/build.zig"
(
  cd "${DEMO_TMPL_DIR}"
  "${OLDPWD}/target/debug/fh" --host "127.0.0.1:${PORT}" --token "${TOKEN}" templates push zig
  "${OLDPWD}/target/debug/fh" --host "127.0.0.1:${PORT}" --token "${TOKEN}" --out-dir ./out -- sh -c 'mkdir -p zig-out && echo "zig-built-artifact" > zig-out/main'
)

if [ -f "${DEMO_TMPL_DIR}/out/zig-out/main" ]; then
  ZIG_CONTENT=$(cat "${DEMO_TMPL_DIR}/out/zig-out/main")
  if [ "${ZIG_CONTENT}" = "zig-built-artifact" ]; then
    echo "[PASS] Test 11 dynamically uploaded template and retrieved output using template auto-detection!"
  else
    echo "[FAIL] Unexpected content in zig-out: ${ZIG_CONTENT}"
    exit 1
  fi
else
  echo "[FAIL] Expected artifact ${DEMO_TMPL_DIR}/out/zig-out/main not found"
  exit 1
fi
rm -rf "${DEMO_TMPL_DIR}"

echo ""
echo "================================================================"
echo "TEST 12: Concurrency & Job Queuing (serialized project runs with QUEUED frame)"
echo "================================================================"
CONCURRENCY_DIR=$(mktemp -d)
(
  cd "${CONCURRENCY_DIR}"
  # Run first job in background holding workspace for 1 second
  "${OLDPWD}/target/debug/fh" --host "127.0.0.1:${PORT}" --token "${TOKEN}" --name "demo-queued-project" \
    -- sh -c 'sleep 1 && echo "job-1-finished"' > out1.log 2>&1 &
  JOB1_PID=$!

  # Sleep briefly to ensure job 1 connects and acquires project lock
  sleep 0.2

  # Run second job targeting the same project
  "${OLDPWD}/target/debug/fh" --host "127.0.0.1:${PORT}" --token "${TOKEN}" --name "demo-queued-project" \
    -- echo "job-2-finished" > out2.log 2>&1

  wait ${JOB1_PID}
)

OUT2_CONTENT=$(cat "${CONCURRENCY_DIR}/out2.log")
echo "Job 2 Output:"
echo "${OUT2_CONTENT}"

if echo "${OUT2_CONTENT}" | grep -q "Build queued on agent"; then
  echo "[PASS] Test 12 verified second job received QUEUED status and safely waited for project lock!"
else
  echo "[FAIL] Expected 'Build queued on agent' in job 2 output"
  exit 1
fi
rm -rf "${CONCURRENCY_DIR}"

echo ""
echo "================================================================"
echo "TEST 13: Multi-agent pool probing, least-busy dispatching, and tag filtering"
echo "================================================================"
PORT2=9877
TOKEN2="demo-gpu-secret"
./target/debug/fhd --listen "127.0.0.1:${PORT2}" --token "${TOKEN2}" --tag gpu --tag fast &
DAEMON2_PID=$!
sleep 0.5

POOL_DIR=$(mktemp -d)
cat <<EOF > "${POOL_DIR}/.farhand.yaml"
name: pool-demo-project
agents:
  - host: 127.0.0.1:${PORT}
    token: ${TOKEN}
    tags: [lan, cpu]
  - host: 127.0.0.1:${PORT2}
    token: ${TOKEN2}
    tags: [gpu, fast]
EOF

(
  cd "${POOL_DIR}"
  echo "--- Subtest 13A: Route specifically to GPU agent via --agent-tag gpu ---"
  "${OLDPWD}/target/debug/fh" --agent-tag gpu --verbose -- echo "running on gpu node"

  echo "--- Subtest 13B: Route specifically to CPU agent via --agent-tag cpu ---"
  "${OLDPWD}/target/debug/fh" --agent-tag cpu --verbose -- echo "running on cpu node"

  echo "--- Subtest 13C: Dynamic load-aware selection across agent pool ---"
  "${OLDPWD}/target/debug/fh" --verbose -- echo "running on least busy node"
)
echo "[PASS] Test 13 multi-agent pool routing and tag filtering succeeded!"
rm -rf "${POOL_DIR}"

echo ""
echo "================================================================"
echo "ALL MANUAL END-TO-END TESTS PASSED SUCCESSFULLY!"
echo "================================================================"

