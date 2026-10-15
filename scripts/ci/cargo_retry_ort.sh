#!/usr/bin/env bash
#
# Run a cargo command, re-running it only when it failed because `ort-sys`'s
# build script could not fetch its prebuilt ONNX Runtime from cdn.pyke.io
# ("Peer disconnected", a truncated archive, a 5xx). ort-sys has no retry of
# its own and a failed fetch leaves nothing behind but a tmp directory, so
# the next cargo invocation simply runs the build script — and the download —
# again. Any other failure (a compile error, a failing test) is final on the
# first attempt: the log is matched for ort-sys's own error lines, so a real
# problem never costs three builds.
#
#   scripts/ci/cargo_retry_ort.sh cargo check -p docling-cli --features cuda
#
# ORT_DOWNLOAD_ATTEMPTS (default 3) sets the number of attempts; the pause
# grows 20s, 40s, …
set -uo pipefail

attempts=${ORT_DOWNLOAD_ATTEMPTS:-3}
status=1
for ((i = 1; i <= attempts; i++)); do
  log=$(mktemp)
  "$@" 2>&1 | tee "$log"
  status=${PIPESTATUS[0]}
  if [ "$status" -eq 0 ]; then
    rm -f "$log"
    exit 0
  fi
  if ! grep -qE 'ort-sys.*(failed to download|extraction of prebuilt binaries)|cdn\.pyke\.io' "$log"; then
    rm -f "$log"
    exit "$status"
  fi
  rm -f "$log"
  if [ "$i" -lt "$attempts" ]; then
    pause=$((20 * i))
    echo "::warning::ort-sys could not download its prebuilt ONNX Runtime (attempt $i/$attempts); retrying in ${pause}s"
    sleep "$pause"
  fi
done
echo "::error::ort-sys could not download its prebuilt ONNX Runtime after $attempts attempts"
exit "$status"
