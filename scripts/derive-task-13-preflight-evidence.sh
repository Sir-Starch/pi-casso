#!/usr/bin/env bash
set -euo pipefail

product_raw=""
commands_json=""
log_file=""
output=""
while (($#)); do
  case "$1" in
    --product-raw) product_raw=${2:?}; shift 2 ;;
    --commands-json) commands_json=${2:?}; shift 2 ;;
    --log) log_file=${2:?}; shift 2 ;;
    --output) output=${2:?}; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

for artifact in "$product_raw" "$commands_json" "$log_file"; do
  test -f "$artifact" && test ! -L "$artifact" || {
    echo "missing or unsafe input: $artifact" >&2
    exit 2
  }
done
test -n "$output" || { echo "output is required" >&2; exit 2; }

jq -e '.status == "unsupported" and .requested_backend == "wgpu" and (.reason | length > 0)' "$product_raw" >/dev/null
jq -e '
  length == 1
  and .[0].expected_exit_code == 2
  and .[0].exit_code == 2
  and .[0].env.PI_CASSO_TEST_FAIL_IF_SOURCE_OPEN == "1"
  and (.[0].argv | index("start")) != null
  and (.[0].argv | index("--no-tui")) != null
' "$commands_json" >/dev/null
if rg -q 'PI_CASSO_TEST_FAIL_IF_SOURCE_OPEN reached' "$log_file"; then
  echo "source-open tripwire fired before preflight returned" >&2
  exit 1
fi

product_bytes=$(stat -c %s "$product_raw")
product_sha256=$(sha256sum "$product_raw" | cut -d' ' -f1)
commands_bytes=$(stat -c %s "$commands_json")
commands_sha256=$(sha256sum "$commands_json" | cut -d' ' -f1)
log_bytes=$(stat -c %s "$log_file")
log_sha256=$(sha256sum "$log_file" | cut -d' ' -f1)

mkdir -p "$(dirname "$output")"
temporary_output=$(mktemp "$(dirname "$output")/.task-13-preflight.XXXXXX")
jq -n \
  --arg product_path "$product_raw" \
  --argjson product_bytes "$product_bytes" \
  --arg product_sha256 "$product_sha256" \
  --arg commands_path "$commands_json" \
  --argjson commands_bytes "$commands_bytes" \
  --arg commands_sha256 "$commands_sha256" \
  --arg log_path "$log_file" \
  --argjson log_bytes "$log_bytes" \
  --arg log_sha256 "$log_sha256" \
  --slurpfile product "$product_raw" \
  --slurpfile commands "$commands_json" '
    {
      schema_version: 1,
      artifact_type: "task13_direct_product_preflight",
      child_exit: $commands[0][0].exit_code,
      status: $product[0].status,
      reason: $product[0].reason,
      requested_backend: $product[0].requested_backend,
      source_open_count: 0,
      worker_start_count: 0,
      worker_present: false,
      event_loop_handoff: false,
      counter_provenance: {
        source_open_count: "PI_CASSO_TEST_FAIL_IF_SOURCE_OPEN=1 armed; direct product log contains no tripwire failure",
        worker_start_count: "direct --no-tui CLI preflight exits before run creation; TUI SearchWorker topology is not entered"
      },
      product_error: $product[0],
      provenance: {
        product_raw: {path: $product_path, bytes: $product_bytes, sha256: $product_sha256},
        commands_json: {path: $commands_path, bytes: $commands_bytes, sha256: $commands_sha256},
        log: {path: $log_path, bytes: $log_bytes, sha256: $log_sha256},
        command: $commands[0][0]
      }
    }
  ' > "$temporary_output"
mv -f -- "$temporary_output" "$output"
