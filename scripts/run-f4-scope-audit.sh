#!/usr/bin/env bash
set -euo pipefail

base_state=""
base_sha=""
final_git=""
evidence_dir=""
commands_json=""
log_file=""
report=""
while (($#)); do
  case "$1" in
    --base-state) base_state=${2:?}; shift 2 ;;
    --base-sha) base_sha=${2:?}; shift 2 ;;
    --final-git) final_git=${2:?}; shift 2 ;;
    --evidence-dir) evidence_dir=${2:?}; shift 2 ;;
    --commands-json) commands_json=${2:?}; shift 2 ;;
    --log) log_file=${2:?}; shift 2 ;;
    --report) report=${2:?}; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
for value in "$base_state" "$base_sha" "$final_git" "$evidence_dir" "$commands_json" "$log_file" "$report"; do test -n "$value" || exit 2; done
test -f "$base_state" || { echo "missing base state" >&2; exit 1; }
[[ $(jq -er '.base_sha' "$base_state") == "$base_sha" ]] || { echo "base SHA mismatch" >&2; exit 1; }
scope_tmp_parent=${TMPDIR:-/tmp}
if [[ $scope_tmp_parent != /* ]]; then
  scope_tmp_parent=/tmp
fi
scope_tmp_parent=${scope_tmp_parent%/}
scope_tmp_parent=${scope_tmp_parent:-/}
mkdir -p -- "$scope_tmp_parent"
scope_tmpdir=$(mktemp -d "$scope_tmp_parent/pi-casso-f4.XXXXXX")
export TMPDIR="$scope_tmpdir"
export XDG_DATA_HOME="$scope_tmpdir/xdg-data"
export XDG_CONFIG_HOME="$scope_tmpdir/xdg-config"
mkdir -p -- "$XDG_DATA_HOME" "$XDG_CONFIG_HOME"
paths_file=$(mktemp "$scope_tmpdir/scope-paths.XXXXXX")
changed_paths_file=$(mktemp "$scope_tmpdir/scope-changed.XXXXXX")
untracked_paths_file=$(mktemp "$scope_tmpdir/scope-untracked.XXXXXX")
cleanup() {
  rm -f -- "$paths_file" "$changed_paths_file" "$untracked_paths_file"
  rm -rf -- "$scope_tmpdir"
}
trap cleanup EXIT INT TERM
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
evidence_runner=(bash "$script_dir/run-evidence-command.sh")

{
  "${evidence_runner[@]}" --commands-json "$commands_json" --log "$log_file" --expected-exit 0 -- \
    git diff --name-only --no-ext-diff -z "$base_sha" "$final_git"
  "${evidence_runner[@]}" --commands-json "$commands_json" --log "$log_file" --expected-exit 0 -- \
    git diff --cached --name-only --no-ext-diff -z "$base_sha"
  "${evidence_runner[@]}" --commands-json "$commands_json" --log "$log_file" --expected-exit 0 -- \
    git diff --name-only --no-ext-diff -z
} >> "$changed_paths_file"
"${evidence_runner[@]}" --commands-json "$commands_json" --log "$log_file" --expected-exit 0 -- \
  git ls-files --others --exclude-standard -z > "$untracked_paths_file"
jq -jr '.preexisting_untracked[]? | .path, "\u0000"' "$base_state" >> "$untracked_paths_file"

sort -zu "$changed_paths_file" -o "$changed_paths_file"
sort -zu "$untracked_paths_file" -o "$untracked_paths_file"
cat "$changed_paths_file" "$untracked_paths_file" > "$paths_file"
sort -zu "$paths_file" -o "$paths_file"

is_allowed_path() {
  case "$1" in
    src/*|tests/*|scripts/*|kernels/cuda/*|.omo/evidence/*|.omo/plans/*|.omo/drafts/*|Cargo.toml|Cargo.lock|README.md) return 0 ;;
    *) return 1 ;;
  esac
}

is_operational_preexisting_path() {
  case "$1" in
    .codebase-memory/artifact.json|.omo/start-work/ledger.jsonl) return 0 ;;
    *) return 1 ;;
  esac
}

operational_artifact_is_valid() {
  local path=$1 entry expected_type
  entry=$(jq -c --arg path "$path" '.preexisting_untracked[]? | select(.path == $path)' "$base_state") || return 1
  [[ -n $entry ]] || return 1
  expected_type=$(jq -er '.type' <<<"$entry") || return 1
  [[ $expected_type == regular && -f $path && ! -L $path ]] || return 1
  jq -e --arg base_sha "$base_sha" '
    type == "object"
    and .schema_version == 2
    and .commit == $base_sha
    and (.project | type == "string")
    and (.indexed_at | type == "string")
    and (.nodes | type == "number")
    and (.edges | type == "number")
    and (.original_size | type == "number")
    and (.compressed_size | type == "number")
    and (.compression_level | type == "number")
  ' "$path" >/dev/null
}

operational_ledger_is_valid() {
  local path=$1 entry expected_type expected_bytes expected_sha current_bytes prefix_sha suffix_file line
  entry=$(jq -c --arg path "$path" '.preexisting_untracked[]? | select(.path == $path)' "$base_state") || return 1
  [[ -n $entry ]] || return 1
  expected_type=$(jq -er '.type' <<<"$entry") || return 1
  expected_bytes=$(jq -er '.bytes' <<<"$entry") || return 1
  expected_sha=$(jq -er '.sha256' <<<"$entry") || return 1
  [[ $expected_type == regular && $expected_bytes =~ ^[0-9]+$ && $expected_sha =~ ^[[:xdigit:]]{64}$ ]] || return 1
  [[ -f $path && ! -L $path ]] || return 1
  current_bytes=$(stat -c %s -- "$path") || return 1
  (( current_bytes >= expected_bytes )) || return 1
  prefix_sha=$(head -c "$expected_bytes" -- "$path" | sha256sum | cut -d' ' -f1) || return 1
  [[ $prefix_sha == "$expected_sha" ]] || return 1

  suffix_file=$(mktemp "$scope_tmpdir/ledger-suffix.XXXXXX") || return 1
  if ! tail -c +$((expected_bytes + 1)) -- "$path" > "$suffix_file"; then
    rm -f -- "$suffix_file"
    return 1
  fi
  suffix_valid=1
  if [[ -s $suffix_file ]]; then
    while IFS= read -r line || [[ -n $line ]]; do
      if [[ -z $line ]] || ! jq -e 'type == "object"' <<<"$line" >/dev/null; then
        suffix_valid=0
        break
      fi
    done < "$suffix_file"
  fi
  rm -f -- "$suffix_file"
  ((suffix_valid == 1))
}

preexisting_object_is_unchanged() {
  local path=$1 entry expected_type expected_bytes expected_sha current_type current_bytes current_sha
  entry=$(jq -c --arg path "$path" '.preexisting_untracked[]? | select(.path == $path)' "$base_state") || return 1
  [[ -n $entry ]] || return 1
  expected_type=$(jq -er '.type' <<<"$entry") || return 1
  expected_bytes=$(jq -er '.bytes' <<<"$entry") || return 1
  expected_sha=$(jq -er '.sha256' <<<"$entry") || return 1
  if [[ -L $path ]]; then
    current_type=symlink
  elif [[ -f $path ]]; then
    current_type=regular
  elif [[ -d $path ]]; then
    current_type=directory
  elif [[ -e $path ]]; then
    current_type=other
  else
    return 1
  fi
  [[ $current_type == "$expected_type" ]] || return 1
  if [[ $current_type == regular ]]; then
    current_bytes=$(stat -c %s -- "$path") || return 1
    current_sha=$(sha256sum -- "$path" | cut -d' ' -f1) || return 1
    [[ $current_bytes == "$expected_bytes" && $current_sha == "$expected_sha" ]]
  else
    [[ $expected_bytes == 0 && -z $expected_sha ]]
  fi
}

path_count=0
operational_preexisting_paths=()
while IFS= read -r -d '' path; do
  if is_operational_preexisting_path "$path"; then
    preexisting_object_is_unchanged "$path" && continue
    case "$path" in
      .codebase-memory/artifact.json) operational_artifact_is_valid "$path" || { echo "invalid operational pre-existing path: $path" >&2; exit 1; } ;;
      .omo/start-work/ledger.jsonl) operational_ledger_is_valid "$path" || { echo "invalid operational pre-existing path: $path" >&2; exit 1; } ;;
    esac
    operational_preexisting_paths+=("$path")
    path_count=$((path_count + 1))
    continue
  fi
  is_allowed_path "$path" || { echo "scope violation: $path" >&2; exit 1; }
  path_count=$((path_count + 1))
done < "$changed_paths_file"
while IFS= read -r -d '' path; do
  if is_operational_preexisting_path "$path"; then
    preexisting_object_is_unchanged "$path" && continue
    case "$path" in
      .codebase-memory/artifact.json) operational_artifact_is_valid "$path" || { echo "invalid operational pre-existing path: $path" >&2; exit 1; } ;;
      .omo/start-work/ledger.jsonl) operational_ledger_is_valid "$path" || { echo "invalid operational pre-existing path: $path" >&2; exit 1; } ;;
    esac
    operational_preexisting_paths+=("$path")
    path_count=$((path_count + 1))
    continue
  fi
  preexisting_object_is_unchanged "$path" && continue
  is_allowed_path "$path" || { echo "scope violation: $path" >&2; exit 1; }
  path_count=$((path_count + 1))
done < "$untracked_paths_file"
if ((${#operational_preexisting_paths[@]})); then
  operational_preexisting_paths_json=$(printf '%s\n' "${operational_preexisting_paths[@]}" | jq -Rsc 'split("\n") | map(select(length > 0))')
else
  operational_preexisting_paths_json='[]'
fi
jq -n \
  --arg base_sha "$base_sha" \
  --arg final_git "$final_git" \
  --argjson path_count "$path_count" \
  --argjson operational_preexisting_paths "$operational_preexisting_paths_json" \
  '{schema_version:1,status:"pass",base_sha:$base_sha,final_git_sha:$final_git,path_count:$path_count,allowlist_exact:true,operational_preexisting_paths:$operational_preexisting_paths}' > "$report"
cleanup
trap - EXIT INT TERM
