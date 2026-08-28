#!/usr/bin/env bash
set -euo pipefail

scenario=""
raw_file=""
commands_json=""
command_index=""
output=""
while (($#)); do
  case "$1" in
    --scenario) scenario=${2:?}; shift 2 ;;
    --raw) raw_file=${2:?}; shift 2 ;;
    --commands-json) commands_json=${2:?}; shift 2 ;;
    --command-index) command_index=${2:?}; shift 2 ;;
    --output) output=${2:?}; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

case "$scenario" in
  start|resume|growing) ;;
  *) echo "scenario must be start, resume, or growing" >&2; exit 2 ;;
esac
[[ $command_index =~ ^[0-9]+$ ]] || { echo "command index must be nonnegative" >&2; exit 2; }
for artifact in "$raw_file" "$commands_json"; do
  test -f "$artifact" && test ! -L "$artifact" || {
    echo "missing or unsafe input: $artifact" >&2
    exit 2
  }
done
test -n "$output" && test ! -L "$output" || { echo "output is required or unsafe" >&2; exit 2; }

jq -s -e '
  length == 1
  and (.[0] | type == "object")
  and (.[0].params_json | type == "string")
  and ((.[0].params_json | fromjson | type) == "object")
' "$raw_file" >/dev/null || {
  echo "raw input must be exactly one valid run-record document" >&2
  exit 2
}
jq -s -e '
  length == 1
  and (.[0] | type == "array")
  and all(.[0][]; type == "object")
' "$commands_json" >/dev/null || {
  echo "commands input must be exactly one valid JSON array document" >&2
  exit 2
}
commands_length=$(jq -s -er '.[0] | length' "$commands_json")
(( command_index < commands_length )) || {
  echo "command index is outside the commands array" >&2
  exit 2
}

raw_bytes=$(stat -c %s "$raw_file")
raw_sha256=$(sha256sum "$raw_file" | cut -d' ' -f1)
commands_bytes=$(stat -c %s "$commands_json")
commands_sha256=$(sha256sum "$commands_json" | cut -d' ' -f1)
params_sha256=$(jq -j -er '.params_json' "$raw_file" | sha256sum | cut -d' ' -f1)

mkdir -p "$(dirname "$output")"
temporary_output=$(mktemp "$(dirname "$output")/.task-13-derived.XXXXXX")
cleanup() {
  [[ -z ${temporary_output:-} ]] || rm -f -- "$temporary_output"
}
trap cleanup EXIT INT TERM
jq -e \
  --arg scenario "$scenario" \
  --arg raw_path "$raw_file" \
  --argjson raw_bytes "$raw_bytes" \
  --arg raw_sha256 "$raw_sha256" \
  --arg commands_path "$commands_json" \
  --argjson commands_bytes "$commands_bytes" \
  --arg commands_sha256 "$commands_sha256" \
  --argjson command_index "$command_index" \
  --arg params_sha256 "$params_sha256" \
  --slurpfile commands "$commands_json" '
    . as $run
    | (.params_json | fromjson) as $params
    | $params.performance_snapshot as $snapshot
    | $commands[0][$command_index] as $command
    | if ($run | type) != "object"
        or ($params | type) != "object"
        or ($snapshot.schema_version != 1)
        or ($command.expected_exit_code != 0)
        or ($command.exit_code != 0)
      then error("inputs do not satisfy the Task 13 derivation contract")
      else .
      end
    | (if $scenario == "resume" then ($params.checkpoint.scanned_windows // 0) else 0 end) as $prior_scanned
    | (if $scenario == "growing"
       then ($snapshot.work_windows // $params.work_windows)
       else ($snapshot.limit // $params.limit)
       end) as $requested_windows
    | ($run.scanned_windows - $prior_scanned) as $invocation_scanned
    | ($requested_windows != null and $invocation_scanned == $requested_windows) as $bound_completed
    | if $bound_completed | not
      then error("observed progress does not exactly match the persisted invocation bound")
      else .
      end
    | {
        schema_version: 1,
        artifact_type: "task13_run_record_derivation",
        scenario: $scenario,
        status: $run.status,
        run_id: $run.id,
        current_offset: $run.current_offset,
        scanned_windows: $run.scanned_windows,
        requested_backend: $snapshot.current_host_resolution.requested,
        resolved_backend: $snapshot.current_host_resolution.resolved,
        stop_reason: (if $scenario == "growing" then "work_windows" else "limit" end),
        stop_reason_provenance: {
          status: "derived_from_exact_persisted_bound",
          prior_scanned_windows: $prior_scanned,
          requested_windows: $requested_windows,
          observed_invocation_scanned_windows: $invocation_scanned,
          exact_bound_completed: $bound_completed
        },
        config: {
          profile: $snapshot.settings.profile,
          cpu_workers: $snapshot.settings.limits.cpu_workers,
          cpu_utilization: $snapshot.settings.limits.cpu_utilization,
          gpu_utilization: $snapshot.settings.limits.gpu_utilization,
          chunk_size: $snapshot.settings.limits.chunk_size,
          queue_depth: $snapshot.settings.limits.queue_depth,
          memory_limit_mb: $snapshot.settings.limits.memory_limit_mb
        },
        memory: {
          status: "unavailable_not_persisted_in_run_record",
          logical_peak_mb: null
        },
        waits: {
          status: "unavailable_not_persisted_in_run_record",
          generator_ms: null,
          queue_ms: null,
          throttle_ms: null,
          persistence_ms: null
        },
        provenance: {
          raw_run_record: {path: $raw_path, bytes: $raw_bytes, sha256: $raw_sha256},
          params_json_sha256: $params_sha256,
          commands_json: {path: $commands_path, bytes: $commands_bytes, sha256: $commands_sha256},
          command_index: $command_index,
          command: $command
        },
        raw_run_record: $run
      }
  ' "$raw_file" > "$temporary_output"
mv -f -- "$temporary_output" "$output"
temporary_output=""
