#!/usr/bin/env bash
set -euo pipefail

plan=""
base_state=""
final_git=""
evidence_dir=""
task_count=""
phase=""
gates=""
path_has_symlink() {
  local probe=$1
  while [[ $probe != / && $probe != . ]]; do
    [[ -L $probe ]] && return 0
    probe=$(dirname -- "$probe")
  done
  return 1
}
while (($#)); do
  case "$1" in
    --plan) plan=${2:?}; shift 2 ;;
    --base-state) base_state=${2:?}; shift 2 ;;
    --final-git) final_git=${2:?}; shift 2 ;;
    --evidence-dir) evidence_dir=${2:?}; shift 2 ;;
    --task-count) task_count=${2:?}; shift 2 ;;
    --phase) phase=${2:?}; shift 2 ;;
    --gates) gates=${2:?}; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
test -f "$plan" && test ! -L "$plan" && test -n "$final_git" && test -d "$evidence_dir" && test ! -L "$evidence_dir" || exit 2
path_has_symlink "$evidence_dir" && exit 2
evidence_root=$(realpath -e -- "$evidence_dir")
default_evidence_root=$(realpath -e -- .omo/evidence)
safe_evidence_file() {
  local path=$1 canonical
  case "/$path/" in */../*|*/./*) return 1;; esac
  [[ -f $path && ! -L $path ]] || return 1
  path_has_symlink "$path" && return 1
  canonical=$(realpath -e -- "$path")
  case "$canonical" in "$evidence_root"/*|"$default_evidence_root"/*) return 0;; *) return 1;; esac
}
[[ $task_count =~ ^[1-9][0-9]*$ ]] || exit 2
[[ $phase == tasks || $phase == final ]] || exit 2
plan_sha=$(sha256sum "$plan" | cut -d' ' -f1)
base_sha=""
[[ -f $base_state ]] && base_sha=$(jq -er '.base_sha' "$base_state")

verify_manifest() {
  local manifest=$1
  test -s "$manifest" && safe_evidence_file "$manifest"
  jq -e --arg plan "$plan_sha" '.schema_version == 1 and .plan_sha256 == $plan and (.status == "pass" or .status == "skip") and (.commands|type=="array") and all(.commands[]; .expected_exit_code == .exit_code and (.argv_sha256|test("^[0-9a-f]{64}$"))) and (.raw_files|type=="array") and (.raw_file_digests|type=="object") and (.log_sha256|test("^[0-9a-f]{64}$")) and ((.status == "pass" and .skip_reason == "") or (.status == "skip" and (.skip_reason|length)>0))' "$manifest" >/dev/null
  manifest_base=$(jq -er '.base_sha' "$manifest")
  [[ -z $base_sha || $manifest_base == "$base_sha" ]]
  log=$(jq -er '.log_file' "$manifest")
  test -s "$log" && safe_evidence_file "$log"
  [[ $(stat -c %s "$log") == "$(jq -er '.log_bytes' "$manifest")" ]]
  [[ $(sha256sum "$log" | cut -d' ' -f1) == "$(jq -er '.log_sha256' "$manifest")" ]]
  while IFS= read -r file; do
    safe_evidence_file "$file"
    [[ $(stat -c %s "$file") == "$(jq -er --arg file "$file" '.raw_file_digests[$file].bytes' "$manifest")" ]]
    [[ $(sha256sum "$file" | cut -d' ' -f1) == "$(jq -er --arg file "$file" '.raw_file_digests[$file].sha256' "$manifest")" ]]
  done < <(jq -r '.raw_files[]' "$manifest")
}

for ((task = 1; task <= task_count; task++)); do
  manifest="$evidence_dir/task-$task-pi-casso-search-throughput.json"
  verify_manifest "$manifest"
  task_git=$(jq -er '.git_sha' "$manifest")
  git merge-base --is-ancestor "$task_git" "$final_git"
done

if [[ $phase == final ]]; then
  IFS=, read -r -a gate_list <<< "$gates"
  for gate in "${gate_list[@]}"; do
    [[ $gate =~ ^F[1-4]$ ]] || exit 2
    manifest=$(find "$evidence_dir" -maxdepth 1 -type f -name "$gate-*-pi-casso-search-throughput.json" -print -quit)
    test -n "$manifest"
    verify_manifest "$manifest"
    [[ $(jq -er '.final_git_sha' "$manifest") == "$final_git" ]]
  done
fi
