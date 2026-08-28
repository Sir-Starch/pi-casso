#!/usr/bin/env bash
set -euo pipefail

output_dir=""
artifact_prefix=""
scenario=""
source_mode=""
cache_state=""
xdg_root=""
work_windows=""
repetitions=""
warmup=""
profile=""
backend=""
gpu_mode=""
generator_backend=""
y_cruncher_path=""
cpu_workers=""
chunk_size=""
queue_depth=""
memory_limit_mb=""
while (($#)); do
  case "$1" in
    --output-dir) output_dir=${2:?}; shift 2 ;;
    --artifact-prefix) artifact_prefix=${2:?}; shift 2 ;;
    --scenario) scenario=${2:?}; shift 2 ;;
    --source-mode) source_mode=${2:?}; shift 2 ;;
    --cache-state) cache_state=${2:?}; shift 2 ;;
    --xdg-root) xdg_root=${2:?}; shift 2 ;;
    --work-windows) work_windows=${2:?}; shift 2 ;;
    --repetitions) repetitions=${2:?}; shift 2 ;;
    --warmup) warmup=${2:?}; shift 2 ;;
    --profile) profile=${2:?}; shift 2 ;;
    --backend) backend=${2:?}; shift 2 ;;
    --gpu) gpu_mode=${2:?}; shift 2 ;;
    --generator-backend) generator_backend=${2:?}; shift 2 ;;
    --y-cruncher-path) y_cruncher_path=${2:?}; shift 2 ;;
    --cpu-workers) cpu_workers=${2:?}; shift 2 ;;
    --chunk-size) chunk_size=${2:?}; shift 2 ;;
    --queue-depth) queue_depth=${2:?}; shift 2 ;;
    --memory-limit-mb) memory_limit_mb=${2:?}; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
for value in "$output_dir" "$artifact_prefix" "$scenario" "$source_mode" "$cache_state" "$xdg_root" "$work_windows" "$repetitions" "$warmup" "$profile" "$backend" "$gpu_mode" "$generator_backend" "$cpu_workers" "$chunk_size" "$queue_depth" "$memory_limit_mb"; do
  test -n "$value" || { echo "missing required benchmark argument" >&2; exit 2; }
done
[[ $artifact_prefix =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]] || { echo "unsafe artifact prefix" >&2; exit 2; }
if [[ -n ${PI_CASSO_DATA_DIR:-} || -n ${PI_CASSO_CONFIG:-} ]]; then
  echo "PI_CASSO_DATA_DIR and PI_CASSO_CONFIG must be unset" >&2
  exit 2
fi
unset PI_CASSO_DATA_DIR PI_CASSO_CONFIG
generator_path_args=()
if [[ -n $y_cruncher_path ]]; then
  [[ $y_cruncher_path == /* && -f $y_cruncher_path && -x $y_cruncher_path ]] || {
    echo "y-cruncher path must be an absolute executable file" >&2
    exit 2
  }
  generator_path_args=(--y-cruncher-path "$y_cruncher_path")
fi
mkdir -p "$output_dir" "$xdg_root"
commands_json="$output_dir/$artifact_prefix-baseline-commands.json"
log_file="$output_dir/$artifact_prefix-baseline.log"
for path in "$commands_json" "$log_file"; do
  [[ ! -e $path ]] || { echo "benchmark artifact already exists: $path" >&2; exit 2; }
done

case "$scenario" in
  all) scenarios=(finite-cold finite-warm growing-cold) ;;
  finite-cold|finite-warm|growing-cold) scenarios=("$scenario") ;;
  *) echo "unknown scenario" >&2; exit 2 ;;
esac
for scenario_name in "${scenarios[@]}"; do
  case "$scenario_name" in
    finite-cold) resolved_source=finite; resolved_cache=cold ;;
    finite-warm) resolved_source=finite; resolved_cache=warm ;;
    growing-cold) resolved_source=growing; resolved_cache=cold ;;
  esac
  [[ $source_mode == auto || $source_mode == "$resolved_source" ]] || { echo "source mode conflicts with scenario" >&2; exit 2; }
  [[ $cache_state == auto || $cache_state == "$resolved_cache" ]] || { echo "cache state conflicts with scenario" >&2; exit 2; }
  scenario_prefix="$artifact_prefix-$scenario_name"
  scenario_xdg="$xdg_root/$scenario_name"
  export XDG_DATA_HOME="$scenario_xdg/data"
  export XDG_CONFIG_HOME="$scenario_xdg/config"
  export TMPDIR="$scenario_xdg/tmp"
  mkdir -p "$XDG_DATA_HOME" "$XDG_CONFIG_HOME" "$TMPDIR"
  baseline_memory="$output_dir/$scenario_prefix-baseline-memory.json"
  baseline_raw="$output_dir/$scenario_prefix-baseline-raw.json"
  measured_memory="$output_dir/$scenario_prefix-measured-memory.json"
  raw_summary="$output_dir/$scenario_prefix-raw.json"
  repetitions_dir="$output_dir/$scenario_prefix"
  for path in "$baseline_memory" "$baseline_raw" "$measured_memory" "$raw_summary" "$repetitions_dir"; do
    [[ ! -e $path ]] || { echo "benchmark artifact already exists: $path" >&2; exit 2; }
  done
  scripts/run-evidence-command.sh --commands-json "$commands_json" --log "$log_file" --expected-exit 0 -- \
    scripts/measure-process-memory.sh --mode baseline --output "$baseline_memory" -- \
    cargo run --release --locked -- --json benchmark --template arch --seconds 0 --work-windows 0 --repetitions 1 --warmup 0 --profile "$profile" --backend "$backend" --gpu "$gpu_mode" --source-mode "$resolved_source" --cache-state "$resolved_cache" --generator-backend "$generator_backend" "${generator_path_args[@]}" --cpu-workers "$cpu_workers" --chunk-size "$chunk_size" --queue-depth "$queue_depth" --memory-limit-mb "$memory_limit_mb" --show-metrics > "$baseline_raw"
  scripts/run-evidence-command.sh --commands-json "$commands_json" --log "$log_file" --expected-exit 0 -- \
    scripts/measure-process-memory.sh --mode measured --output "$measured_memory" -- \
    cargo run --release --locked -- --json benchmark --template arch --seconds 10 --work-windows "$work_windows" --repetitions "$repetitions" --warmup "$warmup" --profile "$profile" --backend "$backend" --gpu "$gpu_mode" --source-mode "$resolved_source" --cache-state "$resolved_cache" --generator-backend "$generator_backend" "${generator_path_args[@]}" --cpu-workers "$cpu_workers" --chunk-size "$chunk_size" --queue-depth "$queue_depth" --memory-limit-mb "$memory_limit_mb" --show-metrics > "$raw_summary"
  mkdir -p "$repetitions_dir"
  run_count=$(jq '.raw_runs | length' "$raw_summary")
  for ((index = 0; index < run_count; index++)); do
    jq ".raw_runs[$index]" "$raw_summary" > "$repetitions_dir/repetition-$index.json"
  done
  paths_json=$(find "$repetitions_dir" -maxdepth 1 -type f -name 'repetition-*.json' -print0 | sort -z | jq -Rsc 'split("\u0000") | map(select(length > 0))')
  baseline_rss=$(jq '.rss_peak_mb' "$baseline_memory")
  measured_rss=$(jq '.rss_peak_mb' "$measured_memory")
  rss_margin=$(((baseline_rss + 9) / 10))
  ((rss_margin < 64)) && rss_margin=64
  updated=$(mktemp "$output_dir/.benchmark.XXXXXX")
  jq --argjson paths "$paths_json" --argjson baseline "$baseline_rss" --argjson peak "$measured_rss" --argjson margin "$rss_margin" '.raw_run_paths=$paths | .memory.rss_baseline_mb=$baseline | .memory.rss_peak_mb=$peak | .memory.rss_margin_mb=$margin' "$raw_summary" > "$updated"
  mv -f -- "$updated" "$raw_summary"
  raw_digests='{}'
  while IFS= read -r -d '' repetition_path; do
    repetition_bytes=$(stat -c %s "$repetition_path")
    repetition_sha=$(sha256sum "$repetition_path" | cut -d' ' -f1)
    raw_digests=$(jq -c --arg path "$repetition_path" --argjson bytes "$repetition_bytes" --arg sha "$repetition_sha" '. + {($path):{bytes:$bytes,sha256:$sha}}' <<<"$raw_digests")
  done < <(jq -j '.[] + "\u0000"' <<<"$paths_json")
  summary_bytes=$(stat -c %s "$raw_summary")
  summary_sha=$(sha256sum "$raw_summary" | cut -d' ' -f1)
  repetition_manifest=$(mktemp "$repetitions_dir/.manifest.XXXXXX")
  jq -n \
    --arg summary_artifact "$raw_summary" \
    --arg cache_state "$resolved_cache" \
    --arg summary_sha256 "$summary_sha" \
    --argjson summary_bytes "$summary_bytes" \
    --argjson expected_count "$run_count" \
    --argjson repetitions "$paths_json" \
    --argjson raw_file_digests "$raw_digests" \
    '{schema_version:1,summary_artifact:$summary_artifact,cache_state:$cache_state,expected_count:$expected_count,repetitions:$repetitions,raw_file_digests:$raw_file_digests,summary_digest:{bytes:$summary_bytes,sha256:$summary_sha256}}' > "$repetition_manifest"
  mv -- "$repetition_manifest" "$repetitions_dir/manifest.json"
done
