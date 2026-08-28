#!/usr/bin/env bash
set -euo pipefail

input=""
variant=""
mode=""
artifact=""
summary=""
repetitions_dir=""
output=""
while (($#)); do
  case "$1" in
    --input) input=${2:?}; shift 2 ;;
    --variant) variant=${2:?}; shift 2 ;;
    --mode) mode=${2:?}; shift 2 ;;
    --artifact) artifact=${2:?}; shift 2 ;;
    --summary) summary=${2:?}; shift 2 ;;
    --repetitions-dir) repetitions_dir=${2-}; shift 2 ;;
    --output) output=${2:?}; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
case "$variant" in
  chudnovsky-rug-binary-split|spigot-persistent|y-cruncher-external) ;;
  *) echo "invalid pi generator variant" >&2; exit 2 ;;
esac
case "$mode" in
  serial|concurrent) normalized_mode=$mode ;;
  search-overlap) normalized_mode=search_overlap ;;
  end-to-end) normalized_mode=end_to_end ;;
  *) echo "invalid pi benchmark mode" >&2; exit 2 ;;
esac
test -s "$input" && test -n "$output" || { echo "missing normalizer input or output" >&2; exit 2; }
if [[ $normalized_mode == end_to_end ]]; then
  test -n "$summary" || { echo "end-to-end summary is required" >&2; exit 2; }
  artifact=""
else
  test -n "$artifact" || { echo "mode artifact is required" >&2; exit 2; }
  summary=""
  repetitions_dir=""
fi

input_sha256=$(sha256sum "$input" | cut -d' ' -f1)
mkdir -p "$(dirname "$output")"
temporary=$(mktemp "$(dirname "$output")/.pi-normalized.XXXXXX")
trap 'rm -f -- "$temporary"' EXIT
jq -e \
  --arg variant "$variant" \
  --arg mode "$normalized_mode" \
  --arg artifact "$artifact" \
  --arg summary "$summary" \
  --arg repetitions_dir "$repetitions_dir" \
  --arg input "$input" \
  --arg input_sha256 "$input_sha256" '
  def identity_complete:
    .workload_identity as $id |
    ["template","match_mode","canvas_width","canvas_height","target_width","target_height",
     "target_bitmap_sha256","start_offset","max_offset","work_windows","source_mode",
     "cache_state","profile","requested_backend","gpu_mode","gpu_device","generator_backend",
     "selected_variant","y_cruncher_path_present","y_cruncher_executable_sha256","cpu_workers",
     "cpu_utilization","chunk_size","queue_depth","memory_limit_mb"] as $fields |
    [$fields[] | . as $field | $id | has($field)] | all;
  def empty_mode($status; $reason; $hash; $identity): {
    schema_version:1, mode:$mode, selected_variant:$variant,
    status:$status, reason:$reason, artifact:$artifact,
    summary_artifact:$summary, repetitions_dir:$repetitions_dir,
    input_artifact:$input, raw_input_sha256:$input_sha256,
    workload_identity:$identity, variant_executable_sha256:$hash,
    median_generated_source_digits_per_second:0,
    p95_generated_source_digits_per_second:0,
    median_generation_wait_ms:0, p95_generation_wait_ms:0,
    median_scanned_windows_per_second:0, p95_scanned_windows_per_second:0,
    median_source_digits_per_second:0, p95_source_digits_per_second:0,
    median_overlap_wait_ms:0, coalesced_request_count:0,
    producer_epochs:0, generation_batches:0, search_work_windows:0,
    correctness:false
  };
  . as $root |
  if .schema_version != 1 then error("schema_version")
  elif .status == "unavailable" then
    (if has($mode) then .[$mode] else . end) as $raw_mode |
    empty_mode("unavailable"; ($raw_mode.reason // .reason); ""; {}) |
    .artifact = ($raw_mode.artifact // $artifact) |
    .summary_artifact = ($raw_mode.summary_artifact // $summary) |
    .repetitions_dir = ($raw_mode.repetitions_dir // $repetitions_dir)
  elif .status != "ok" then error("status")
  elif (.selected_variant != $variant) then error("selected_variant")
  elif (identity_complete | not) then error("workload_identity")
  elif (.workload_identity.selected_variant != $variant) then error("identity_variant")
  elif ((.generator_executable_sha256 | test("^[0-9a-f]{64}$")) | not) then error("executable_hash")
  elif $mode == "end_to_end" then
    empty_mode("ok"; ""; .generator_executable_sha256; .workload_identity) |
    .median_scanned_windows_per_second = $root.median.scanned_windows_per_second |
    .p95_scanned_windows_per_second = $root.p95.scanned_windows_per_second |
    .median_source_digits_per_second = $root.median.source_digits_per_second |
    .p95_source_digits_per_second = $root.p95.source_digits_per_second |
    .median_generation_wait_ms = $root.median.generation_wait_ms |
    .p95_generation_wait_ms = $root.p95.generation_wait_ms |
    .median_overlap_wait_ms = $root.median.overlap_wait_ms |
    .correctness = (($root.raw_runs | length) > 0 and all($root.raw_runs[]; .status == "ok"))
  else
    empty_mode("ok"; ""; .generator_executable_sha256; .workload_identity) |
    .median_generated_source_digits_per_second = $root.median.generated_source_digits_per_second |
    .p95_generated_source_digits_per_second = $root.p95.generated_source_digits_per_second |
    .median_generation_wait_ms = $root.median.generation_wait_ms |
    .p95_generation_wait_ms = $root.p95.generation_wait_ms |
    .median_scanned_windows_per_second = ($root.median.scanned_windows_per_second // 0) |
    .p95_scanned_windows_per_second = ($root.p95.scanned_windows_per_second // 0) |
    .median_overlap_wait_ms = $root.overlap_wait_ms |
    .coalesced_request_count = $root.coalesced_request_count |
    .producer_epochs = $root.producer_epochs |
    .generation_batches = $root.generation_batches |
    .search_work_windows = $root.search_work_windows |
    .correctness = $root.correctness
  end |
  if ([.median_generated_source_digits_per_second,.p95_generated_source_digits_per_second,
       .median_generation_wait_ms,.p95_generation_wait_ms,.median_scanned_windows_per_second,
       .p95_scanned_windows_per_second,.median_source_digits_per_second,
       .p95_source_digits_per_second,.median_overlap_wait_ms,.coalesced_request_count,
       .producer_epochs,.generation_batches,.search_work_windows] | all(type == "number"))
  then . else error("metric_type") end
' "$input" > "$temporary"
mv -- "$temporary" "$output"
trap - EXIT
