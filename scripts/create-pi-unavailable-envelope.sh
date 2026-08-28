#!/usr/bin/env bash
set -euo pipefail

variant=""
reason=""
output=""
while (($#)); do
  case "$1" in
    --variant) variant=${2:?}; shift 2 ;;
    --reason) reason=${2:?}; shift 2 ;;
    --output) output=${2:?}; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
case "$variant" in
  chudnovsky-rug-binary-split|spigot-persistent|y-cruncher-external) ;;
  *) echo "invalid pi generator variant" >&2; exit 2 ;;
esac
test -n "$reason" && test -n "$output" || { echo "missing unavailable-envelope argument" >&2; exit 2; }
[[ ! -e $output ]] || { echo "output already exists" >&2; exit 2; }
mkdir -p "$(dirname "$output")"

serial_artifact=".omo/evidence/task-12-variants/$variant-serial.json"
concurrent_artifact=".omo/evidence/task-12-variants/$variant-concurrent.json"
overlap_artifact=".omo/evidence/task-12-variants/$variant-search-overlap.json"
summary_artifact=".omo/evidence/task-12-variants/$variant/end-to-end-unavailable.json"
temporary=$(mktemp "$(dirname "$output")/.pi-unavailable.XXXXXX")
trap 'rm -f -- "$temporary"' EXIT
jq -n \
  --arg variant "$variant" \
  --arg reason "$reason" \
  --arg serial_artifact "$serial_artifact" \
  --arg concurrent_artifact "$concurrent_artifact" \
  --arg overlap_artifact "$overlap_artifact" \
  --arg summary_artifact "$summary_artifact" '
  def mode($artifact): {
    status:"unavailable", reason:$reason, artifact:$artifact,
    median_generated_source_digits_per_second:0,
    p95_generated_source_digits_per_second:0,
    median_generation_wait_ms:0, p95_generation_wait_ms:0,
    median_scanned_windows_per_second:0, p95_scanned_windows_per_second:0,
    median_source_digits_per_second:0, p95_source_digits_per_second:0,
    median_overlap_wait_ms:0, coalesced_request_count:0,
    producer_epochs:0, generation_batches:0, search_work_windows:0,
    correctness:false
  };
  {
    schema_version:1, status:"unavailable", reason:$reason,
    selected_variant:$variant, variant_executable_sha256:"",
    eligible:false, rejection_reason:$reason,
    serial:mode($serial_artifact),
    concurrent:mode($concurrent_artifact),
    search_overlap:mode($overlap_artifact),
    end_to_end:(mode("") + {
      summary_artifact:$summary_artifact, repetitions_dir:""
    })
  }' > "$temporary"
mv -- "$temporary" "$output"
trap - EXIT
