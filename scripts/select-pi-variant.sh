#!/usr/bin/env bash
set -euo pipefail

input_dir=""
baseline_dir=""
output=""
comparison_json=""
while (($#)); do
  case "$1" in
    --input-dir) input_dir=${2:?}; shift 2 ;;
    --baseline-dir) baseline_dir=${2:?}; shift 2 ;;
    --output) output=${2:?}; shift 2 ;;
    --comparison-json) comparison_json=${2:?}; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
test -d "$input_dir" && test -d "$baseline_dir" && test -n "$output" || {
  echo "missing selector input" >&2
  exit 1
}
[[ ! -e $output ]] || { echo "selection output already exists" >&2; exit 1; }
if [[ -n $comparison_json ]]; then
  jq -e '.schema_version == 1 and (.accepted | type == "boolean")' "$comparison_json" >/dev/null
fi

variants=(chudnovsky-rug-binary-split spigot-persistent y-cruncher-external)
candidates='[]'
normalized_inputs='[]'
unavailable_count=0
scratch=$(mktemp -d)
trap 'rm -rf -- "$scratch"; rm -f -- "$output"' EXIT

shell_path() {
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -u "$1"
  else
    printf '%s\n' "$1"
  fi
}

verify_raw_input() {
  local normalized=$1
  local raw expected actual
  raw=$(shell_path "$(jq -er '.input_artifact' "$normalized")")
  expected=$(jq -er '.raw_input_sha256 | select(test("^[0-9a-f]{64}$"))' "$normalized")
  test -s "$raw" && test ! -L "$raw"
  actual=$(sha256sum "$raw" | cut -d' ' -f1)
  [[ $actual == "$expected" ]]
}

for variant in "${variants[@]}"; do
  variant_dir="$input_dir/$variant"
  serial="$variant_dir/serial.normalized.json"
  concurrent="$variant_dir/concurrent.normalized.json"
  overlap="$variant_dir/search_overlap.normalized.json"
  end_to_end="$variant_dir/end-to-end.normalized.json"
  for pair in "serial:$serial" "concurrent:$concurrent" "search_overlap:$overlap" "end_to_end:$end_to_end"; do
    mode=${pair%%:*}
    path=${pair#*:}
    test -s "$path" && test ! -L "$path" || { echo "missing normalized input: $path" >&2; exit 1; }
    jq -e --arg variant "$variant" --arg mode "$mode" '
      .schema_version == 1 and .selected_variant == $variant and .mode == $mode and
      (.status == "ok" or .status == "unavailable") and
      (.correctness | type == "boolean") and
      (.variant_executable_sha256 | type == "string")
    ' "$path" >/dev/null
    verify_raw_input "$path" || { echo "raw input digest mismatch" >&2; exit 1; }
    normalized_inputs=$(jq -c --arg path "$path" '. + [$path]' <<<"$normalized_inputs")
  done

  statuses=$(jq -s -c 'map(.status) | unique' "$serial" "$concurrent" "$overlap" "$end_to_end")
  if [[ $statuses == '["unavailable"]' ]]; then
    unavailable_count=$((unavailable_count + 1))
    reason=$(jq -er '.reason | select(length > 0)' "$serial")
    eligible=false
    rejection_reason=$reason
    comparison='null'
  elif [[ $statuses != '["ok"]' ]]; then
    echo "mixed candidate status" >&2
    exit 1
  else
    jq -e 'all(.[]; .correctness == true)' < <(jq -s '.' "$serial" "$concurrent" "$overlap" "$end_to_end") >/dev/null
    hashes=$(jq -s -c 'map(.variant_executable_sha256) | unique' "$serial" "$concurrent" "$overlap" "$end_to_end")
    jq -e '(length == 1) and (.[0] | test("^[0-9a-f]{64}$"))' <<<"$hashes" >/dev/null
    summary=$(shell_path "$(jq -er '.summary_artifact' "$end_to_end")")
    repetitions=$(shell_path "$(jq -er '.repetitions_dir | select(length > 0)' "$end_to_end")")
    test -s "$summary" && test -d "$repetitions" || { echo "missing end-to-end artifacts" >&2; exit 1; }
    if [[ -n $comparison_json ]]; then
      comparison=$(jq -c '.' "$comparison_json")
    else
      comparison_path="$scratch/$variant-comparison.json"
      scripts/compare-benchmark-runs.sh \
        --baseline-dir "$baseline_dir" \
        --candidate-dir "$repetitions" \
        --metrics scanned_windows_per_second,source_digits_per_second,stage_timings.generation_wait_ms,overlap_wait_ms \
        --max-p95-regression 0.10 \
        --comparison-mode generator-selection \
        --allow-config-diff generator_backend,selected_variant,y_cruncher_path_present,y_cruncher_executable_sha256 \
        --output "$comparison_path"
      comparison=$(jq -c '.' "$comparison_path")
    fi
    eligible=$(jq -r '.accepted' <<<"$comparison")
    if [[ $eligible == true ]]; then
      rejection_reason=""
    else
      rejection_reason=$(jq -r '.rejection_reason // "end_to_end_regression"' <<<"$comparison")
    fi
  fi

  candidate=$(jq -n \
    --arg variant "$variant" \
    --argjson serial "$(jq -c '.' "$serial")" \
    --argjson concurrent "$(jq -c '.' "$concurrent")" \
    --argjson search_overlap "$(jq -c '.' "$overlap")" \
    --argjson end_to_end "$(jq -c '.' "$end_to_end")" \
    --argjson eligible "$eligible" \
    --arg rejection_reason "$rejection_reason" \
    --argjson comparison "$comparison" '
      ($serial.status) as $status |
      {
        selected_variant:$variant, status:$status, reason:$serial.reason,
        variant_executable_sha256:$serial.variant_executable_sha256,
        serial:$serial, concurrent:$concurrent, search_overlap:$search_overlap,
        end_to_end:$end_to_end, eligible:$eligible,
        rejection_reason:$rejection_reason, end_to_end_comparison:$comparison,
        scores:{
          search_overlap_scanned_windows_per_second:$search_overlap.median_scanned_windows_per_second,
          generation_geometric_mean:(($serial.median_generated_source_digits_per_second *
            $concurrent.median_generated_source_digits_per_second) | sqrt),
          generation_wait_ms:$search_overlap.median_generation_wait_ms
        }
      }')
  candidates=$(jq -c --argjson candidate "$candidate" '. + [$candidate]' <<<"$candidates")
done

if ((unavailable_count == ${#variants[@]})); then
  exit 2
fi
eligible_count=$(jq '[.[] | select(.eligible)] | length' <<<"$candidates")
((eligible_count > 0)) || { echo "no eligible pi generator variant" >&2; exit 1; }
winner=$(jq -c '
  [.[] | select(.eligible)] |
  sort_by([
    (-.scores.search_overlap_scanned_windows_per_second),
    (-.scores.generation_geometric_mean),
    .scores.generation_wait_ms,
    .selected_variant
  ]) | .[0]
' <<<"$candidates")
selected_variant=$(jq -r '.selected_variant' <<<"$winner")
selected_hash=$(jq -r '.variant_executable_sha256' <<<"$winner")
selected_identity=$(jq -c '.end_to_end.workload_identity' <<<"$winner")
mkdir -p "$(dirname "$output")"
temporary=$(mktemp "$(dirname "$output")/.pi-selection.XXXXXX")
jq -n \
  --argjson workload_identity "$selected_identity" \
  --argjson candidates "$candidates" \
  --arg selected_variant "$selected_variant" \
  --arg selected_hash "$selected_hash" \
  --argjson normalized_inputs "$normalized_inputs" '
  {
    schema_version:1, workload_identity:$workload_identity,
    candidates:$candidates, selected_variant:$selected_variant,
    tie_break:["search_overlap_scanned_windows_per_second","generation_geometric_mean",
      "generation_wait_ms","selected_variant"],
    selected_executable_sha256:$selected_hash,
    normalized_inputs:$normalized_inputs
  }' > "$temporary"
mv -- "$temporary" "$output"
trap - EXIT
rm -rf -- "$scratch"
