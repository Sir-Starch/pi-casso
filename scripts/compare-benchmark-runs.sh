#!/usr/bin/env bash
set -euo pipefail

baseline_dir=""
candidate_dir=""
metric_list=""
max_p95_regression=""
output=""
comparison_mode="exact"
allow_config_diff=""
while (($#)); do
  case "$1" in
    --baseline-dir) baseline_dir=${2:?}; shift 2 ;;
    --candidate-dir) candidate_dir=${2:?}; shift 2 ;;
    --metrics) metric_list=${2:?}; shift 2 ;;
    --max-p95-regression) max_p95_regression=${2:?}; shift 2 ;;
    --output) output=${2:?}; shift 2 ;;
    --comparison-mode) comparison_mode=${2:?}; shift 2 ;;
    --allow-config-diff) allow_config_diff=${2:?}; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

for value in "$baseline_dir" "$candidate_dir" "$metric_list" "$max_p95_regression" "$output"; do
  test -n "$value" || { echo "missing comparison argument" >&2; exit 2; }
done
test -d "$baseline_dir" && test -d "$candidate_dir" || exit 2
[[ ! -e $output ]] || { echo "comparison output already exists" >&2; exit 2; }
jq -n -e --arg value "$max_p95_regression" '($value | tonumber) >= 0 and ($value | tonumber) <= 1' >/dev/null || {
  echo "max p95 regression must be in [0,1]" >&2
  exit 2
}

case "$comparison_mode" in
  exact)
    [[ -z $allow_config_diff ]] || { echo "exact comparison does not allow identity differences" >&2; exit 2; }
    ;;
  tuning)
    [[ $allow_config_diff == cpu_workers ]] || { echo "tuning permits only cpu_workers" >&2; exit 2; }
    ;;
  generator-selection)
    [[ -n $allow_config_diff ]] || { echo "generator selection requires an allowlist" >&2; exit 2; }
    IFS=, read -r -a allowed_fields <<< "$allow_config_diff"
    for field in "${allowed_fields[@]}"; do
      case "$field" in generator_backend|selected_variant|y_cruncher_path_present|y_cruncher_executable_sha256) ;;
        *) echo "invalid generator-selection identity field: $field" >&2; exit 2 ;;
      esac
    done
    ;;
  *) echo "invalid comparison mode" >&2; exit 2 ;;
esac

mkdir -p "$(dirname "$output")"
write_failure() {
  local reason=$1
  local temporary
  temporary=$(mktemp "$(dirname "$output")/.comparison.XXXXXX")
  jq -n --arg mode "$comparison_mode" --arg reason "$reason" --arg baseline "$baseline_dir" --arg candidate "$candidate_dir" '{schema_version:1,status:"fail",accepted:false,rejection_reason:$reason,comparison_mode:$mode,baseline_dir:$baseline,candidate_dir:$candidate,identity_differences:[],metrics:[]}' > "$temporary"
  mv -- "$temporary" "$output"
  echo "$reason" >&2
  exit 1
}

baseline_manifest="$baseline_dir/manifest.json"
candidate_manifest="$candidate_dir/manifest.json"
test -s "$baseline_manifest" && test -s "$candidate_manifest" || write_failure "missing repetition manifest"
baseline_state=$(jq -er '.cache_state' "$baseline_manifest") || write_failure "malformed baseline manifest"
candidate_state=$(jq -er '.cache_state' "$candidate_manifest") || write_failure "malformed candidate manifest"
[[ $baseline_state == "$candidate_state" ]] || write_failure "cache-state mismatch"
baseline_count=$(jq -er '.expected_count' "$baseline_manifest") || write_failure "missing baseline count"
candidate_count=$(jq -er '.expected_count' "$candidate_manifest") || write_failure "missing candidate count"

audit_root=$(mktemp -d)
trap 'rm -rf -- "$audit_root"' EXIT
scripts/verify-benchmark-repetitions.sh --dir "$baseline_dir" --cache-state "$baseline_state" --expected-count "$baseline_count" --output "$audit_root/baseline.json" || write_failure "baseline repetition audit failed"
scripts/verify-benchmark-repetitions.sh --dir "$candidate_dir" --cache-state "$candidate_state" --expected-count "$candidate_count" --output "$audit_root/candidate.json" || write_failure "candidate repetition audit failed"

baseline_summary=$(jq -er '.summary_artifact' "$baseline_manifest")
candidate_summary=$(jq -er '.summary_artifact' "$candidate_manifest")
baseline_machine=$(jq -cS '.machine' "$baseline_summary") || write_failure "missing baseline machine identity"
candidate_machine=$(jq -cS '.machine' "$candidate_summary") || write_failure "missing candidate machine identity"
[[ $baseline_machine == "$candidate_machine" ]] || write_failure "machine or power-policy identity mismatch"

baseline_identity=$(jq -cS '.workload_identity' "$baseline_summary") || write_failure "missing baseline workload identity"
candidate_identity=$(jq -cS '.workload_identity' "$candidate_summary") || write_failure "missing candidate workload identity"
identity_differences='[]'
identity_paths='[]'
if [[ -n $allow_config_diff ]]; then
  IFS=, read -r -a allowed_fields <<< "$allow_config_diff"
  for field in "${allowed_fields[@]}"; do
    identity_paths=$(jq -c --arg field "$field" '. + [[$field]]' <<<"$identity_paths")
    baseline_value=$(jq -c --arg field "$field" 'try getpath([$field]) catch null' <<<"$baseline_identity")
    candidate_value=$(jq -c --arg field "$field" 'try getpath([$field]) catch null' <<<"$candidate_identity")
    if [[ $baseline_value != "$candidate_value" ]]; then
      identity_differences=$(jq -c --arg field "$field" --argjson baseline "$baseline_value" --argjson candidate "$candidate_value" '. + [{field:$field,baseline:$baseline,candidate:$candidate}]' <<<"$identity_differences")
    fi
  done
fi
comparable_baseline=$(jq -cS --argjson paths "$identity_paths" 'delpaths($paths)' <<<"$baseline_identity")
comparable_candidate=$(jq -cS --argjson paths "$identity_paths" 'delpaths($paths)' <<<"$candidate_identity")
[[ $comparable_baseline == "$comparable_candidate" ]] || write_failure "workload identity mismatch outside allowlist"
if [[ $comparison_mode == exact ]]; then
  baseline_workload=$(jq -er '.workload_id' "$baseline_summary")
  candidate_workload=$(jq -er '.workload_id' "$candidate_summary")
  [[ $baseline_workload == "$candidate_workload" ]] || write_failure "workload_id mismatch"
fi

mapfile -t baseline_runs < <(jq -r '.repetitions[]' "$baseline_manifest" | tr -d '\r')
mapfile -t candidate_runs < <(jq -r '.repetitions[]' "$candidate_manifest" | tr -d '\r')
IFS=, read -r -a metrics <<< "$metric_list"
metric_results='[]'
accepted=true
rejection_reason=""

metric_direction() {
  case "$1" in
    scanned_windows_per_second|source_digits_per_second|logical_window_digits_per_second|generated_source_digits_per_second|gpu.resource_reuses) echo higher ;;
    elapsed_seconds|overlap_wait_ms|cache_write_ms|generation_wait_ms|stage_timings.read_ms|stage_timings.parse_ms|stage_timings.queue_wait_ms|stage_timings.backend_compute_ms|stage_timings.gpu_allocation_ms|stage_timings.gpu_upload_ms|stage_timings.gpu_dispatch_ms|stage_timings.gpu_readback_map_ms|stage_timings.reduction_ms|stage_timings.persistence_ms|stage_timings.generation_wait_ms|stage_timings.throttle_wait_ms|waits.source_ms|waits.queue_ms|waits.generator_ms|waits.throttle_ms|memory.logical_peak_mb|memory.rss_peak_mb|memory.gpu_vram_peak_mb|gpu.fallback_count|error_count) echo lower ;;
    *) return 1 ;;
  esac
}

metric_stats() {
  local metric=$1
  shift
  jq -s --arg metric "$metric" '
    map(getpath($metric | split("."))) as $values |
    if ($values | all(type == "number") | not) or ($values | length) == 0 then error("missing numeric metric") else
      ($values | sort) as $sorted |
      ($values | add / length) as $mean |
      (if ($values | length) < 2 or $mean == 0 then 0 else
        (([$values[] | (. - $mean) as $difference | $difference * $difference] | add / (($values | length) - 1) | sqrt) / $mean)
      end) as $cv |
      {median:$sorted[((((($sorted | length) * 50) / 100) | ceil) - 1)],cv:$cv,
       direct_p95:$sorted[((((($sorted | length) * 95) / 100) | ceil) - 1)]}
    end
  ' "$@"
}

for metric in "${metrics[@]}"; do
  test -n "$metric" || write_failure "empty metric name"
  direction=$(metric_direction "$metric") || write_failure "unknown metric: $metric"
  baseline_stats=$(metric_stats "$metric" "${baseline_runs[@]}") || write_failure "baseline metric is missing: $metric"
  candidate_stats=$(metric_stats "$metric" "${candidate_runs[@]}") || write_failure "candidate metric is missing: $metric"
  baseline_median=$(jq -r '.median' <<<"$baseline_stats")
  candidate_median=$(jq -r '.median' <<<"$candidate_stats")
  baseline_cv=$(jq -r '.cv' <<<"$baseline_stats")
  candidate_cv=$(jq -r '.cv' <<<"$candidate_stats")
  if [[ $metric == *_per_second ]]; then
    baseline_p95=$(jq -s --arg metric "$metric" 'sort_by(.elapsed_seconds) | .[(((length * 95 / 100) | ceil) - 1)] | getpath($metric | split("."))' "${baseline_runs[@]}")
    candidate_p95=$(jq -s --arg metric "$metric" 'sort_by(.elapsed_seconds) | .[(((length * 95 / 100) | ceil) - 1)] | getpath($metric | split("."))' "${candidate_runs[@]}")
  else
    baseline_p95=$(jq -r '.direct_p95' <<<"$baseline_stats")
    candidate_p95=$(jq -r '.direct_p95' <<<"$candidate_stats")
  fi
  calculation=$(jq -n \
    --arg direction "$direction" \
    --argjson baseline_median "$baseline_median" \
    --argjson candidate_median "$candidate_median" \
    --argjson baseline_cv "$baseline_cv" \
    --argjson candidate_cv "$candidate_cv" \
    --argjson baseline_p95 "$baseline_p95" \
    --argjson candidate_p95 "$candidate_p95" \
    --argjson max_regression "$max_p95_regression" '
      ([0.05, (2 * ([$baseline_cv,$candidate_cv] | max))] | max) as $noise |
      (if $direction == "higher" then
         (if $baseline_median == 0 then (if $candidate_median > 0 then 1 else 0 end) else ($candidate_median / $baseline_median - 1) end)
       else
         (if $candidate_median == 0 then (if $baseline_median > 0 then 1 else 0 end) else ($baseline_median / $candidate_median - 1) end)
       end) as $uplift |
      ($direction == "lower" and $baseline_median == 0 and $candidate_median == 0) as $equal_optimal |
      (if $direction == "higher" then $candidate_p95 >= ($baseline_p95 * (1 - $max_regression)) else $candidate_p95 <= ($baseline_p95 * (1 + $max_regression)) end) as $p95_ok |
      {baseline_median:$baseline_median,candidate_median:$candidate_median,baseline_cv:$baseline_cv,candidate_cv:$candidate_cv,baseline_p95:$baseline_p95,candidate_p95:$candidate_p95,noise_floor:$noise,uplift:$uplift,equal_optimal:$equal_optimal,p95_ok:$p95_ok,accepted:(($equal_optimal or $uplift > $noise) and $p95_ok)}
  ')
  metric_accepted=$(jq -r '.accepted' <<<"$calculation")
  if [[ $metric_accepted != true ]]; then
    accepted=false
    [[ -n $rejection_reason ]] || rejection_reason="metric $metric did not exceed the noise floor with an acceptable p95"
  fi
  metric_results=$(jq -c --arg metric "$metric" --arg direction "$direction" --argjson calculation "$calculation" '. + [({metric:$metric,direction:$direction} + $calculation)]' <<<"$metric_results")
done

temporary=$(mktemp "$(dirname "$output")/.comparison.XXXXXX")
jq -n \
  --arg mode "$comparison_mode" \
  --arg baseline "$baseline_dir" \
  --arg candidate "$candidate_dir" \
  --arg rejection_reason "$rejection_reason" \
  --argjson accepted "$accepted" \
  --argjson identity_differences "$identity_differences" \
  --argjson metrics "$metric_results" \
  '{schema_version:1,status:"pass",accepted:$accepted,rejection_reason:$rejection_reason,comparison_mode:$mode,baseline_dir:$baseline,candidate_dir:$candidate,identity_differences:$identity_differences,metrics:$metrics}' > "$temporary"
mv -- "$temporary" "$output"
if [[ $accepted == false && $comparison_mode == exact ]]; then
  exit 1
fi
