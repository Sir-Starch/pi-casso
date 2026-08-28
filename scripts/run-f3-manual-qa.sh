#!/usr/bin/env bash
set -euo pipefail

commands_json=""
log_file=""
xdg_root=""
work_windows=""
repetitions=""
warmup=""
profile=""
backend=""
gpu_mode=""
cpu_workers=""
queue_depth=""
memory_limit_mb=""
while (($#)); do
  case "$1" in
    --commands-json) commands_json=${2:?}; shift 2 ;;
    --log) log_file=${2:?}; shift 2 ;;
    --xdg-root) xdg_root=${2:?}; shift 2 ;;
    --work-windows) work_windows=${2:?}; shift 2 ;;
    --repetitions) repetitions=${2:?}; shift 2 ;;
    --warmup) warmup=${2:?}; shift 2 ;;
    --profile) profile=${2:?}; shift 2 ;;
    --backend) backend=${2:?}; shift 2 ;;
    --gpu) gpu_mode=${2:?}; shift 2 ;;
    --cpu-workers) cpu_workers=${2:?}; shift 2 ;;
    --queue-depth) queue_depth=${2:?}; shift 2 ;;
    --memory-limit-mb) memory_limit_mb=${2:?}; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

for value in "$commands_json" "$log_file" "$xdg_root" "$work_windows" "$repetitions" "$warmup" "$profile" "$backend" "$gpu_mode" "$cpu_workers" "$queue_depth" "$memory_limit_mb"; do
  [[ -n $value && $value != *$'\n'* && $value != *$'\r'* ]] || exit 2
done
[[ $work_windows =~ ^[1-9][0-9]*$ && $repetitions =~ ^[1-9][0-9]*$ && $warmup =~ ^[0-9]+$ ]] || exit 2
[[ $cpu_workers =~ ^[1-9][0-9]*$ && $queue_depth =~ ^[1-9][0-9]*$ && $memory_limit_mb =~ ^[1-9][0-9]*$ ]] || exit 2
if [[ -n ${PI_CASSO_DATA_DIR:-} || -n ${PI_CASSO_CONFIG:-} ]]; then
  echo "PI_CASSO_DATA_DIR and PI_CASSO_CONFIG must be unset" >&2
  exit 2
fi
unset PI_CASSO_DATA_DIR PI_CASSO_CONFIG

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd -- "$script_dir/.." && pwd)
runner="$script_dir/run-evidence-command.sh"
skip_helper="$script_dir/create-benchmark-skip-envelope.sh"
derive_helper="$script_dir/derive-task-13-run-evidence.sh"
cd "$repo_root"

absolute_path() {
  case "$1" in
    /*) printf '%s\n' "$1" ;;
    *) printf '%s/%s\n' "$PWD" "$1" ;;
  esac
}
commands_json=$(absolute_path "$commands_json")
log_file=$(absolute_path "$log_file")
xdg_root=$(absolute_path "$xdg_root")
evidence_dir="$repo_root/.omo/evidence"
mkdir -p "$evidence_dir"

[[ ! -L "$xdg_root" && (! -e "$xdg_root" || -d "$xdg_root") ]] || {
  echo "XDG root must be a non-symlink directory" >&2
  exit 2
}
mkdir -p "$xdg_root/data" "$xdg_root/config" "$xdg_root/tmp" "$(dirname "$commands_json")" "$(dirname "$log_file")"
reset_file() {
  local path=$1 content=$2 temporary
  temporary=$(mktemp "$(dirname "$path")/.f3-reset.XXXXXX")
  printf '%s' "$content" > "$temporary"
  mv -f -- "$temporary" "$path"
}
reset_file "$commands_json" $'[]\n'
reset_file "$log_file" ""
export XDG_DATA_HOME="$xdg_root/data"
export XDG_CONFIG_HOME="$xdg_root/config"
export TMPDIR="$xdg_root/tmp"

[[ -x "$runner" && -x "$skip_helper" && -x "$derive_helper" ]] || {
  echo "F3 helper is missing or not executable" >&2
  exit 2
}

gpu_info_raw="$evidence_dir/F3-gpu-info-raw.json"
cpu_raw="$evidence_dir/F3-cpu-benchmark-raw.json"
pi_generate_log="$evidence_dir/F3-pi-generate.log"
cache_info_raw="$evidence_dir/F3-cache-info-raw.json"
finite_raw="$evidence_dir/F3-finite-raw.json"
resume_raw="$evidence_dir/F3-resume-raw.json"
growing_raw="$evidence_dir/F3-growing-raw.json"
growing_derived="$evidence_dir/F3-growing-derived.json"
tui_transcript="$evidence_dir/F3-tui-transcript.log"
tui_raw="$evidence_dir/F3-tui-raw.json"
wgpu_raw="$evidence_dir/F3-wgpu-raw.json"
cuda_info_raw="$evidence_dir/F3-cuda-info-raw.json"
cuda_raw="$evidence_dir/F3-cuda-raw.json"

temporary_files=()
cleanup() {
  local path
  for path in "${temporary_files[@]}"; do
    [[ -z $path ]] || rm -f -- "$path"
  done
}
trap cleanup EXIT INT TERM

new_stream() {
  local path
  path=$(mktemp "$TMPDIR/f3-stream.XXXXXX")
  temporary_files+=("$path")
  printf '%s\n' "$path"
}
run_wrapped() {
  local expected_exit=$1 stdout_file=$2
  shift 2
  "$runner" --commands-json "$commands_json" --log "$log_file" --expected-exit "$expected_exit" -- "$@" > "$stdout_file"
}
extract_run_record() {
  local stream=$1 output=$2 temporary
  temporary=$(mktemp "$TMPDIR/f3-record.XXXXXX")
  temporary_files+=("$temporary")
  jq -s -e '
    if length == 0 or any(.[]; type != "object") then
      error("run output is empty or contains a non-object document")
    else
      map(select((type == "object") and ((.params_json? | type) == "string"))) as $records
      | if ($records | length) != 1 then
          error("run output must contain exactly one persisted run record")
        else $records[0]
        end
    end
  ' "$stream" > "$temporary"
  mv -f -- "$temporary" "$output"
}
validate_capability() {
  local path=$1 expected_backend=$2
  jq -s -e --arg expected_backend "$expected_backend" '
    if length != 1 or (.[0] | type) != "object" then error("gpu info must be one JSON object")
    elif .[0].schema_version != 1
      or (.[0].capability_state | type) != "string"
      or (.[0].available | type) != "boolean"
      or (.[0].backend | type) != "string"
      or .[0].backend != $expected_backend
      or (.[0].device | type) != "string"
      or (.[0].driver | type) != "string"
      or (.[0].feature | type) != "string"
      or (.[0].reason | type) != "string"
      or (.[0].cuda_feature_enabled | type) != "boolean"
      or (.[0].cuda_driver_loaded | type) != "boolean"
      or (.[0].cuda_device_count | type) != "number"
      or (.[0].cuda_available | type) != "boolean"
      or (.[0].cuda_driver_version | type) != "string"
      or (.[0].cuda_device_compute_capability | type) != "string"
      or (.[0].cuda_ptx_compatible | type) != "boolean"
      or (.[0].kernel_arch | type) != "string"
      or (.[0].kernel_sha256 | type) != "string"
      or (.[0].kernel_source_sha256 | type) != "string"
      or (.[0].kernel_load_status | type) != "string"
      or (. [0].capability_state | IN(["unavailable", "preflight_ok"][]) | not) then
        error("gpu info schema or capability state is invalid")
    else true
    end
  ' "$path" >/dev/null
}
validate_run_record() {
  local path=$1 expected_scanned=$2
  jq -s -e --argjson expected "$expected_scanned" '
    if length != 1 or (.[0] | type) != "object" then error("run record must be one JSON object")
    elif .[0].status != "paused"
      or (.[0].id | type) != "string"
      or (.[0].id | length) == 0
      or (.[0].current_offset | type) != "number"
      or (.[0].scanned_windows | type) != "number"
      or .[0].scanned_windows != $expected
      or (.[0].params_json | type) != "string"
      or ((.[0].params_json | fromjson | type) != "object")
      or ((.[0].params_json | fromjson | .performance_snapshot.schema_version) != 1) then
        error("run record does not satisfy the bounded persistence contract")
    else true
    end
  ' "$path" >/dev/null
}
validate_benchmark() {
  local path=$1 expected_backend=$2 expected_scanned=$3
  jq -s -e --arg expected_backend "$expected_backend" --argjson expected "$expected_scanned" '
    if length != 1 or (.[0] | type) != "object" then error("benchmark must be one JSON object")
    elif .[0].status != "ok"
      or .[0].requested_backend != $expected_backend
      or .[0].resolved_backend != $expected_backend
      or .[0].scanned_windows != $expected
      or .[0].backend_fault_status != "none"
      or .[0].fallback != false
      or .[0].fallback_count != 0
      or (.[0].fallback_reason | type) != "string"
      or .[0].fallback_reason != "" then
        error("benchmark backend or fallback contract failed")
    else true
    end
  ' "$path" >/dev/null
}
validate_skip_probe() {
  local path=$1 requested_backend=$2 expected_reason=$3
  jq -s -e --arg requested_backend "$requested_backend" --arg expected_reason "$expected_reason" '
    length == 1
    and (.[0] | type == "object")
    and .[0].status == "unsupported"
    and .[0].requested_backend == $requested_backend
    and .[0].resolved_backend == null
    and .[0].scanned_windows == 0
    and .[0].backend_fault_status == "none"
    and .[0].fallback == false
    and (.[0].reason | type == "string" and length > 0)
    and .[0].reason == $expected_reason
  ' "$path" >/dev/null
}
validate_cache_info() {
  jq -s -e '
    length == 1
    and (.[0] | type == "object")
    and .[0].schema_version == 1
    and (.[0].path | type == "string" and length > 0)
    and (.[0].digits | type == "number" and . >= 20000)
    and (.[0].published_digits | type == "number" and . >= 20000)
    and .[0].valid_ascii == true
  ' "$cache_info_raw" >/dev/null
}

run_wrapped 0 "$gpu_info_raw" cargo run --release --locked -- --json gpu info
validate_capability "$gpu_info_raw" wgpu
wgpu_capability=$(jq -er '.capability_state' "$gpu_info_raw")
wgpu_reason=$(jq -er '.reason' "$gpu_info_raw")
case "$wgpu_capability" in
  preflight_ok)
    run_wrapped 0 "$wgpu_raw" cargo run --release --locked -- --json benchmark --template arch --source-mode finite --cache-state cold --work-windows "$work_windows" --repetitions "$repetitions" --warmup "$warmup" --profile "$profile" --backend gpu --gpu on --generator-backend cpu --cpu-workers "$cpu_workers" --chunk-size 65536 --queue-depth "$queue_depth" --memory-limit-mb "$memory_limit_mb" --show-metrics
    validate_benchmark "$wgpu_raw" wgpu "$work_windows"
    jq -e '.gpu.test_only_mock == false and .gpu.submissions > 0 and .gpu.completions == .gpu.submissions' "$wgpu_raw" >/dev/null
    ;;
  unavailable)
    case "$wgpu_reason" in
      adapter_unavailable|device_unavailable|pipeline_preflight_unavailable) ;;
      *) echo "invalid WGPU unavailable reason: $wgpu_reason" >&2; exit 1 ;;
    esac
    unsupported_stream=$(new_stream)
    set +e
    run_wrapped 2 "$unsupported_stream" cargo run --release --locked -- --json benchmark --template arch --source-mode finite --cache-state cold --work-windows "$work_windows" --repetitions 1 --warmup 0 --profile "$profile" --backend gpu --gpu on --generator-backend cpu --cpu-workers "$cpu_workers" --chunk-size 65536 --queue-depth "$queue_depth" --memory-limit-mb "$memory_limit_mb" --show-metrics
    unsupported_exit=$?
    set -e
    [[ $unsupported_exit -eq 2 ]] || { echo "WGPU unavailable probe must exit 2" >&2; exit 1; }
    validate_skip_probe "$unsupported_stream" wgpu "$wgpu_reason"
    "$skip_helper" --requested-backend wgpu --reason "$wgpu_reason" --output "$wgpu_raw"
    jq -s -e 'length == 1 and .[0].schema_version == 1 and .[0].status == "skip" and .[0].requested_backend == "wgpu" and .[0].resolved_backend == null and .[0].skip_reason == $wgpu_reason' --arg wgpu_reason "$wgpu_reason" "$wgpu_raw" >/dev/null
    ;;
  *) echo "invalid WGPU capability state: $wgpu_capability" >&2; exit 1 ;;
esac

run_wrapped 0 "$cpu_raw" cargo run --release --locked -- --json benchmark --template arch --source-mode finite --cache-state cold --work-windows "$work_windows" --repetitions "$repetitions" --warmup "$warmup" --profile "$profile" --backend "$backend" --gpu "$gpu_mode" --generator-backend cpu --cpu-workers "$cpu_workers" --chunk-size 65536 --queue-depth "$queue_depth" --memory-limit-mb "$memory_limit_mb" --show-metrics
validate_benchmark "$cpu_raw" "$backend" "$work_windows"

run_wrapped 0 "$pi_generate_log" cargo run --release --locked -- pi generate --digits 20000 --generator-backend cpu --workers 4
run_wrapped 0 "$cache_info_raw" cargo run --release --locked -- --json pi cache-info
validate_cache_info
pi_file=$(jq -er '.path' "$cache_info_raw")
[[ -f "$pi_file" && ! -L "$pi_file" ]] || { echo "π cache path is missing or unsafe" >&2; exit 1; }

finite_stream=$(new_stream)
run_wrapped 0 "$finite_stream" cargo run --release --locked -- --json start --template arch --name f3-finite --mode 8x8 --max-offset 128 --limit 128 --work-windows 128 --no-tui --pi-file "$pi_file" --backend cpu --gpu off --profile performance --max-fps 60 --ui-refresh-ms 1000 --checkpoint-every 1 --yes --show-metrics
extract_run_record "$finite_stream" "$finite_raw"
validate_run_record "$finite_raw" 128
finite_run_id=$(jq -er '.id' "$finite_raw")

resume_stream=$(new_stream)
run_wrapped 0 "$resume_stream" cargo run --release --locked -- --json resume "$finite_run_id" --no-tui --backend cpu --gpu off --profile performance --max-fps 60 --ui-refresh-ms 1000 --limit 128 --show-metrics
extract_run_record "$resume_stream" "$resume_raw"
validate_run_record "$resume_raw" 128
jq -e '.params_json | fromjson | .performance_snapshot.schema_version == 1 and .performance_snapshot.settings.backend == "cpu" and .performance_snapshot.settings.gpu == "off"' "$resume_raw" >/dev/null

run_wrapped 0 "$tui_raw" "$script_dir/run-tui-qa.sh" --run-id "$finite_run_id" --cli-resume --commands-json "$commands_json" --log "$log_file" --transcript "$tui_transcript" --xdg-root "$xdg_root" --raw "$tui_raw" --work-windows 128 --profile performance --backend cpu --gpu off --max-fps 60 --ui-refresh-ms 1000
jq -e '
  .status == "ok"
  and .terminal.pty == true
  and .terminal.columns == 120
  and .terminal.lines == 40
  and .snapshot_ui.max_fps == 60
  and .snapshot_ui.ui_refresh_ms == 1000
  and .resume_state_restored == true
  and .event_loop_handoff == true
  and .resume_run_id == $run_id
  and .transcript_contains.backend
  and .transcript_contains.queue
  and .transcript_contains.wait
  and .transcript_contains.stop_reason
  and .transcript_contains.resume
' --arg run_id "$finite_run_id" "$tui_raw" >/dev/null

growing_stream=$(new_stream)
run_wrapped 0 "$growing_stream" cargo run --release --locked -- --json start --template arch --name f3-growing --no-tui --infinite --work-windows "$work_windows" --backend cpu --gpu off --profile performance --generator-backend cpu --cpu-workers 1 --chunk-size 65536 --queue-depth 1 --memory-limit-mb 512 --yes --show-metrics
extract_run_record "$growing_stream" "$growing_raw"
validate_run_record "$growing_raw" "$work_windows"
jq -e '.params_json | fromjson | .infinite == true and .work_windows == $work_windows and .performance_snapshot.work_windows == $work_windows' --argjson work_windows "$work_windows" "$growing_raw" >/dev/null

growing_command_index=$(jq -er 'length - 1' "$commands_json")
run_wrapped 0 "$growing_derived" "$derive_helper" --scenario growing --raw "$growing_raw" --commands-json "$commands_json" --command-index "$growing_command_index" --output "$growing_derived"
jq -e --argjson work_windows "$work_windows" '
  .schema_version == 1
  and .scenario == "growing"
  and .status == "paused"
  and .stop_reason == "work_windows"
  and .scanned_windows == $work_windows
  and .stop_reason_provenance.exact_bound_completed == true
  and .stop_reason_provenance.requested_windows == $work_windows
' "$growing_derived" >/dev/null

cuda_check_log=$(new_stream)
run_wrapped 0 "$cuda_check_log" cargo check --release --locked --no-default-features --features cuda-native
run_wrapped 0 "$cuda_info_raw" cargo run --release --locked --no-default-features --features cuda-native -- --json gpu info
validate_capability "$cuda_info_raw" cuda
cuda_capability=$(jq -er '.capability_state' "$cuda_info_raw")
cuda_reason=$(jq -er '.reason | select(type == "string" and length > 0)' "$cuda_info_raw" 2>/dev/null || true)
if [[ $cuda_capability == preflight_ok ]]; then
  cuda_handoff_log=$(new_stream)
  run_wrapped 0 "$cuda_handoff_log" "$script_dir/verify-cuda-handoff.sh" --manifest kernels/cuda/handoff.json --readme kernels/cuda/README.md --source kernels/cuda/emergence.cu --artifact kernels/cuda/emergence.ptx
  cuda_readme_source=$(sed -n 's/^source_sha256=//p' kernels/cuda/README.md)
  cuda_readme_artifact=$(sed -n 's/^artifact_sha256=//p' kernels/cuda/README.md)
  [[ $cuda_readme_source =~ ^[0-9a-f]{64} && $cuda_readme_artifact =~ ^[0-9a-f]{64} ]]
  [[ $cuda_readme_source == $(sha256sum kernels/cuda/emergence.cu | cut -d' ' -f1) ]]
  [[ $cuda_readme_artifact == $(sha256sum kernels/cuda/emergence.ptx | cut -d' ' -f1) ]]
  jq -e '.available == true and .cuda_feature_enabled == true and .cuda_driver_loaded == true and .cuda_available == true and .cuda_ptx_compatible == true and .cuda_device_compute_capability == "8.9" and .kernel_arch == "compute_89" and .kernel_load_status == "loaded" and (.kernel_sha256 | length) == 64 and (.kernel_source_sha256 | length) == 64' "$cuda_info_raw" >/dev/null
  run_wrapped 0 "$cuda_raw" cargo run --release --locked --no-default-features --features cuda-native -- --json benchmark --template arch --source-mode finite --cache-state cold --work-windows "$work_windows" --repetitions "$repetitions" --warmup "$warmup" --profile "$profile" --backend cuda --gpu on --generator-backend cpu --cpu-workers "$cpu_workers" --chunk-size 65536 --queue-depth "$queue_depth" --memory-limit-mb "$memory_limit_mb" --show-metrics
  validate_benchmark "$cuda_raw" cuda "$work_windows"
  jq -e --arg source "$cuda_readme_source" --arg artifact "$cuda_readme_artifact" '.kernel_arch == "compute_89" and .kernel_sha256 == $artifact and .kernel_source_sha256 == $source and .kernel_load_status == "loaded"' "$cuda_raw" >/dev/null
else
  case "$cuda_reason" in
    artifact_handoff_missing|driver_unavailable|device_unavailable|unsupported_compute_capability) ;;
    *) echo "invalid CUDA unavailable reason: $cuda_reason" >&2; exit 1 ;;
  esac
  cuda_stream=$(new_stream)
  set +e
  run_wrapped 2 "$cuda_stream" cargo run --release --locked --no-default-features --features cuda-native -- --json benchmark --template arch --source-mode finite --cache-state cold --work-windows "$work_windows" --repetitions 1 --warmup 0 --profile "$profile" --backend cuda --gpu on --generator-backend cpu --cpu-workers "$cpu_workers" --chunk-size 65536 --queue-depth "$queue_depth" --memory-limit-mb "$memory_limit_mb" --show-metrics
  cuda_exit=$?
  set -e
  [[ $cuda_exit -eq 2 ]] || { echo "CUDA unavailable probe must exit 2" >&2; exit 1; }
  validate_skip_probe "$cuda_stream" cuda "$cuda_reason"
  "$skip_helper" --requested-backend cuda --reason "$cuda_reason" --output "$cuda_raw"
  jq -s -e --arg reason "$cuda_reason" 'length == 1 and .[0].schema_version == 1 and .[0].status == "skip" and .[0].requested_backend == "cuda" and .[0].resolved_backend == null and .[0].skip_reason == $reason' "$cuda_raw" >/dev/null
fi

redact_file() {
  local path=$1 temporary
  [[ -f $path ]] || return 0
  temporary=$(mktemp "$TMPDIR/f3-redact.XXXXXX")
  REDACT_ROOT="$xdg_root" perl -0pe 's/\Q$ENV{REDACT_ROOT}\E/<xdg-root>/g' "$path" > "$temporary"
  mv -f -- "$temporary" "$path"
}
command -v perl >/dev/null 2>&1 || { echo "perl is required to redact private paths" >&2; exit 2; }
for artifact in "$gpu_info_raw" "$cpu_raw" "$pi_generate_log" "$cache_info_raw" "$finite_raw" "$resume_raw" "$growing_raw" "$growing_derived" "$tui_transcript" "$tui_raw" "$wgpu_raw" "$cuda_info_raw" "$cuda_raw" "$commands_json" "$log_file"; do
  redact_file "$artifact"
done
