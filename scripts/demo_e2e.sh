#!/usr/bin/env bash
set -e

echo "=== Building Farhand Client (fh) and Daemon (fhd) ==="
cargo build -p fhd -p fh

PORT=9876
TOKEN="demo-secret-123"

echo ""
echo "=== Starting 'fhd' Daemon on 127.0.0.1:${PORT} in the background ==="
./target/debug/fhd --listen "127.0.0.1:${PORT}" --token "${TOKEN}" &
DAEMON_PID=$!

# Ensure daemon is stopped when script exits
trap "echo ''; echo '=== Stopping fhd Daemon (PID: ${DAEMON_PID}) ==='; kill -9 ${DAEMON_PID} 2>/dev/null || true" EXIT

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
echo "ALL MANUAL END-TO-END TESTS PASSED SUCCESSFULLY!"
echo "================================================================"
