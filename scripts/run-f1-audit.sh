#!/usr/bin/env bash
set -euo pipefail

plan=""
base_state=""
final_git=""
evidence_dir=""
commands_json=""
log_file=""
report=""
while (($#)); do
  case "$1" in
    --plan) plan=${2:?}; shift 2 ;;
    --base-state) base_state=${2:?}; shift 2 ;;
    --final-git) final_git=${2:?}; shift 2 ;;
    --evidence-dir) evidence_dir=${2:?}; shift 2 ;;
    --commands-json) commands_json=${2:?}; shift 2 ;;
    --log) log_file=${2:?}; shift 2 ;;
    --report) report=${2:?}; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
for value in "$plan" "$base_state" "$final_git" "$evidence_dir" "$commands_json" "$log_file" "$report"; do test -n "$value" || exit 2; done
task_count=$(find "$evidence_dir" -maxdepth 1 -type f -name 'task-[0-9]*-pi-casso-search-throughput.json' | wc -l)
((task_count > 0)) || { echo "no numeric task manifests" >&2; exit 1; }
scripts/run-evidence-command.sh --commands-json "$commands_json" --log "$log_file" --expected-exit 0 -- \
  scripts/verify-evidence.sh --plan "$plan" --base-state "$base_state" --final-git "$final_git" --evidence-dir "$evidence_dir" --task-count "$task_count" --phase tasks
jq -n --arg final_git "$final_git" --argjson task_count "$task_count" '{schema_version:1,status:"pass",final_git_sha:$final_git,task_count:$task_count,checks:{numeric_manifests:true,raw_hashes:true,ancestor_chain:true}}' > "$report"
