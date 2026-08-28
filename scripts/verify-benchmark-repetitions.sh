#!/usr/bin/env bash
set -euo pipefail

repetitions_dir=""
cache_state=""
expected_count=""
output=""
while (($#)); do
  case "$1" in
    --dir) repetitions_dir=${2:?}; shift 2 ;;
    --cache-state) cache_state=${2:?}; shift 2 ;;
    --expected-count) expected_count=${2:?}; shift 2 ;;
    --output) output=${2:?}; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

[[ $cache_state == cold || $cache_state == warm ]] || { echo "cache state must be cold or warm" >&2; exit 2; }
[[ $expected_count =~ ^[1-9][0-9]*$ ]] || { echo "expected count must be positive" >&2; exit 2; }
test -d "$repetitions_dir" && test ! -L "$repetitions_dir" && test -n "$output" || exit 2
[[ ! -e $output ]] || { echo "audit output already exists" >&2; exit 2; }
manifest="$repetitions_dir/manifest.json"
test -s "$manifest" && test ! -L "$manifest" || { echo "missing repetition manifest" >&2; exit 1; }
jq -e --arg state "$cache_state" --argjson count "$expected_count" '
  .schema_version == 1 and .cache_state == $state and .expected_count == $count and
  (.summary_artifact | type == "string" and length > 0) and
  (.repetitions | type == "array" and length == $count) and
  (.raw_file_digests | type == "object")
' "$manifest" >/dev/null

summary=$(jq -er '.summary_artifact' "$manifest")
test -s "$summary" && test ! -L "$summary" || { echo "missing benchmark summary" >&2; exit 1; }
jq -e --arg state "$cache_state" --argjson count "$expected_count" '
  .schema_version == 1 and .status == "ok" and .cache_state == $state and
  .repetitions == $count and (.warmup | type == "number") and
  (.median | type == "object") and (.p95 | type == "object")
' "$summary" >/dev/null
warmup_count=$(jq -er '.warmup' "$summary")
warm_up_completed=false
if [[ $cache_state == warm ]] && ((warmup_count > 0)); then
  warm_up_completed=true
fi
jq -e --argjson warmed "$warm_up_completed" '.warm_up_completed == $warmed' "$summary" >/dev/null

mapfile -t repetitions < <(jq -r '.repetitions[]' "$manifest" | tr -d '\r')
canonical_dir=$(realpath -e "$repetitions_dir")
cache_ids=()
for repetition in "${repetitions[@]}"; do
  test -s "$repetition" && test ! -L "$repetition" || { echo "missing repetition: $repetition" >&2; exit 1; }
  [[ $(realpath -e "$(dirname "$repetition")") == "$canonical_dir" ]] || { echo "repetition escapes directory" >&2; exit 1; }
  bytes=$(stat -c %s "$repetition")
  digest=$(sha256sum "$repetition" | cut -d' ' -f1)
  windows_shell=false
  command -v cygpath >/dev/null 2>&1 && windows_shell=true
  jq -e --arg path "$repetition" --argjson windows_shell "$windows_shell" --argjson bytes "$bytes" --arg digest "$digest" '
    def path_key:
      if $windows_shell and test("^/[A-Za-z]/") then
        ((.[1:2] | ascii_upcase) + ":" + .[2:])
      else . end;
    (.raw_file_digests | to_entries |
      map(select((.key | path_key) == ($path | path_key)))) as $matches |
    ($matches | length == 1) and
    $matches[0].value.bytes == $bytes and $matches[0].value.sha256 == $digest
  ' "$manifest" >/dev/null
  jq -e --arg state "$cache_state" --argjson warmed "$warm_up_completed" '
    .schema_version == 1 and .status == "ok" and
    (.repetition | type == "number") and
    (.cache_instance_id | type == "string" and length > 0) and
    (.cache_reset == ($state == "cold")) and
    (.warm_up_completed == $warmed) and
    (($state != "cold") or .first_published_digits == 0) and
    (.scanned_windows | type == "number") and
    (.scanned_windows_per_second | type == "number") and
    (.source_digits_per_second | type == "number") and
    (.logical_window_digits_per_second | type == "number") and
    (.elapsed_seconds | type == "number") and
    (.stage_timings | type == "object") and (.waits | type == "object") and
    (.overlap_wait_ms | type == "number") and (.cache_write_ms | type == "number") and
    (.producer_epochs | type == "number") and (.coalesced_request_count | type == "number") and
    (.generation_batches | type == "number")
  ' "$repetition" >/dev/null
  cache_ids+=("$(jq -er '.cache_instance_id' "$repetition")")
done

unique_cache_count=$(printf '%s\n' "${cache_ids[@]}" | sort -u | wc -l)
if [[ $cache_state == cold ]]; then
  ((unique_cache_count == expected_count)) || { echo "cold repetitions reused a cache instance" >&2; exit 1; }
else
  ((unique_cache_count == 1)) || { echo "warm repetitions did not share one cache instance" >&2; exit 1; }
fi

nearest_rank() {
  local path=$1
  local percentile=$2
  jq -s --arg path "$path" --argjson percentile "$percentile" '
    map(getpath($path | split("."))) | sort |
    .[((((length * $percentile) / 100) | ceil) - 1)]
  ' "${repetitions[@]}"
}

for field in scanned_windows_per_second source_digits_per_second logical_window_digits_per_second elapsed_seconds overlap_wait_ms cache_write_ms; do
  value=$(nearest_rank "$field" 50)
  jq -e --arg field "$field" --argjson value "$value" '.median[$field] == $value' "$summary" >/dev/null
done
generation_wait=$(nearest_rank stage_timings.generation_wait_ms 50)
jq -e --argjson value "$generation_wait" '.median.generation_wait_ms == $value' "$summary" >/dev/null
p95_index=$(( (expected_count * 95 + 99) / 100 - 1 ))
p95_run=$(jq -s --argjson index "$p95_index" 'sort_by(.elapsed_seconds)[$index]' "${repetitions[@]}")
for field in scanned_windows_per_second source_digits_per_second logical_window_digits_per_second elapsed_seconds overlap_wait_ms cache_write_ms; do
  value=$(jq -r --arg field "$field" '.[$field]' <<<"$p95_run")
  jq -e --arg field "$field" --argjson value "$value" '.p95[$field] == $value' "$summary" >/dev/null
done
p95_generation_wait=$(jq -r '.stage_timings.generation_wait_ms' <<<"$p95_run")
jq -e --argjson value "$p95_generation_wait" '.p95.generation_wait_ms == $value' "$summary" >/dev/null
for field in overlap_wait_ms cache_write_ms producer_epochs; do
  value=$(nearest_rank "$field" 50)
  jq -e --arg field "$field" --argjson value "$value" '.[$field] == $value' "$summary" >/dev/null
done

mkdir -p "$(dirname "$output")"
temporary=$(mktemp "$(dirname "$output")/.repetition-audit.XXXXXX")
trap 'rm -f -- "$temporary"' EXIT
jq -n \
  --arg status pass \
  --arg directory "$repetitions_dir" \
  --arg cache_state "$cache_state" \
  --arg summary_artifact "$summary" \
  --argjson verified_count "$expected_count" \
  --argjson unique_cache_instances "$unique_cache_count" \
  '{schema_version:1,status:$status,directory:$directory,cache_state:$cache_state,verified_count:$verified_count,unique_cache_instances:$unique_cache_instances,summary_artifact:$summary_artifact,digests_verified:true,aggregates_verified:true}' > "$temporary"
mv -- "$temporary" "$output"
trap - EXIT
