#!/usr/bin/env bash
set -euo pipefail

task=""
gate=""
status=""
skip_reason=""
output=""
log_file=""
commands_json=""
plan=".omo/plans/pi-casso-search-throughput.md"
path_manifest=""
raw_inputs=()
path_has_symlink() {
  local probe=$1
  while [[ $probe != / && $probe != . ]]; do
    [[ -L $probe ]] && return 0
    probe=$(dirname -- "$probe")
  done
  return 1
}
safe_evidence_path() {
  local path=$1 canonical
  case "/$path/" in */../*|*/./*) return 1;; esac
  path_has_symlink "$path" && return 1
  canonical=$(realpath -m -- "$path")
  case "$canonical" in "$evidence_root"|"$evidence_root"/*) return 0;; *) return 1;; esac
}
while (($#)); do
  case "$1" in
    --task) task=${2:?}; shift 2 ;;
    --gate) gate=${2:?}; shift 2 ;;
    --status) status=${2:?}; shift 2 ;;
    --skip-reason) skip_reason=${2-}; shift 2 ;;
    --output) output=${2:?}; shift 2 ;;
    --log) log_file=${2:?}; shift 2 ;;
    --commands-json) commands_json=${2:?}; shift 2 ;;
    --plan) plan=${2:?}; shift 2 ;;
    --path-manifest) path_manifest=${2:?}; shift 2 ;;
    --raw-files)
      shift
      while (($#)) && [[ $1 != --* ]]; do raw_inputs+=("$1"); shift; done
      ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[[ $status == pass || $status == skip ]] || { echo "status must be pass or skip" >&2; exit 2; }
if [[ $status == pass && -n $skip_reason ]] || [[ $status == skip && -z $skip_reason ]]; then
  echo "skip_reason must be empty for pass and nonempty for skip" >&2
  exit 2
fi
if [[ -n $task && -n $gate ]] || [[ -z $task && -z $gate ]]; then
  echo "exactly one of task or gate is required" >&2
  exit 2
fi
if [[ -n $task ]]; then
  [[ $task =~ ^[1-9][0-9]*$ ]] || exit 2
  output=${output:-".omo/evidence/task-$task-pi-casso-search-throughput.json"}
else
  [[ $gate =~ ^F[1-4]$ ]] || exit 2
  test -n "$output" || { echo "gate output is required" >&2; exit 2; }
fi
test -f "$plan" && test ! -L "$plan" && test -f "$log_file" && test ! -L "$log_file" && test -s "$log_file" && test -f "$commands_json" && test ! -L "$commands_json" || {
  echo "plan, nonempty log and commands JSON are required" >&2
  exit 2
}
mkdir -p .omo/evidence
evidence_root=$(realpath -e -- .omo/evidence)
safe_evidence_path "$output" || { echo "evidence output escapes .omo/evidence" >&2; exit 2; }
[[ ! -e $output && ! -L $output ]] || { echo "evidence manifest already exists" >&2; exit 2; }
jq -e 'type == "array" and all(.[]; (.argv|type=="array") and (.argv_sha256|test("^[0-9a-f]{64}$")) and (.env|type=="object") and (.expected_exit_code|type=="number") and (.exit_code|type=="number") and .expected_exit_code == .exit_code)' "$commands_json" >/dev/null

path_manifests_json='[]'
if [[ -n $path_manifest ]]; then
  test -f "$path_manifest" && test ! -L "$path_manifest" && safe_evidence_path "$path_manifest" || { echo "missing or unsafe path manifest" >&2; exit 2; }
  prefix=$(jq -er '.artifact_prefix | select(. != "standalone")' "$path_manifest")
  for candidate in .omo/evidence/task-*-pi-casso-search-throughput.json .omo/evidence/F*-pi-casso-search-throughput.json; do
    [[ -f $candidate ]] || continue
    if jq -e --arg prefix "$prefix" 'any(.path_manifests[]?; .artifact_prefix == $prefix)' "$candidate" >/dev/null; then
      echo "artifact prefix was already recorded" >&2
      exit 2
    fi
  done
  for key in raw commands_json log paths; do
    raw_inputs+=("$(jq -er --arg key "$key" '.[$key]' "$path_manifest")")
  done
  path_manifests_json=$(jq -c '[.]' "$path_manifest")
fi

expanded=()
directory_digests='{}'
for input in "${raw_inputs[@]}"; do
  safe_evidence_path "$input" || { echo "raw input escapes .omo/evidence" >&2; exit 2; }
  if [[ -d $input && ! -L $input ]]; then
    [[ -z $(find "$input" \( -type l -o \! -type f -a \! -type d \) -print -quit) ]] || { echo "unsafe entry in raw directory" >&2; exit 2; }
    while IFS= read -r file; do
      expanded+=("$file")
    done < <(find "$input" -type f ! -xtype l -print | sort)
    directory_sha=$(
      while IFS= read -r file; do
        relative=${file#"$input"/}
        bytes=$(stat -c %s "$file")
        digest=$(sha256sum "$file" | cut -d' ' -f1)
        printf '%s\0%s\0%s\n' "$relative" "$bytes" "$digest"
      done < <(find "$input" -type f ! -xtype l -print | sort) | sha256sum | cut -d' ' -f1
    )
    directory_digests=$(jq -c --arg path "$input" --arg sha "$directory_sha" '. + {($path):$sha}' <<<"$directory_digests")
  else
    expanded+=("$input")
  fi
done

raw_files='[]'
raw_digests='{}'
declare -A seen=()
for file in "${expanded[@]}"; do
  [[ -z ${seen[$file]:-} ]] || continue
  seen[$file]=1
  test -f "$file" && test ! -L "$file" && safe_evidence_path "$file" || { echo "missing or unsafe raw file: $file" >&2; exit 2; }
  bytes=$(stat -c %s "$file")
  digest=$(sha256sum "$file" | cut -d' ' -f1)
  raw_files=$(jq -c --arg path "$file" '. + [$path]' <<<"$raw_files")
  raw_digests=$(jq -c --arg path "$file" --argjson bytes "$bytes" --arg sha "$digest" '. + {($path):{bytes:$bytes,sha256:$sha}}' <<<"$raw_digests")
done

plan_sha=$(sha256sum "$plan" | cut -d' ' -f1)
git_sha=$(git rev-parse HEAD)
base_sha=$git_sha
if [[ -f .omo/evidence/base-state.json ]]; then
  base_sha=$(jq -er '.base_sha' .omo/evidence/base-state.json)
fi
log_bytes=$(stat -c %s "$log_file")
log_sha=$(sha256sum "$log_file" | cut -d' ' -f1)
commands=$(jq -c '.' "$commands_json")
os=$(uname -srm)
cpu=$(awk -F: '/model name/ {gsub(/^[[:space:]]+/, "", $2); print $2; exit}' /proc/cpuinfo 2>/dev/null || true)
cpu=${cpu:-unavailable}
rustc=$(rustc --version 2>/dev/null || printf unavailable)
power=$(powerprofilesctl get 2>/dev/null || printf unavailable)
thermal=$(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null || printf unavailable)
changed_paths=$(
  git status --porcelain=v1 --untracked-files=all \
    | sed 's/^...//' \
    | grep -Ev '^\.omo/evidence/|^\.omo/start-work/ledger\.jsonl$' \
    | sort
)
paths_sha=$(printf '%s' "$changed_paths" | sha256sum | cut -d' ' -f1)
diff_stat=$(
  git diff --stat -- . ':!.omo/plans' ':!.omo/drafts' ':!.omo/evidence' \
    ':!.omo/start-work/ledger.jsonl' | tr '\n' ';'
)
requested=""
resolved=""
for file in "${expanded[@]}"; do
  value=$(jq -r '.requested_backend? // empty' "$file" 2>/dev/null || true)
  [[ -z $requested && -n $value ]] && requested=$value
  value=$(jq -r '.resolved_backend? // empty' "$file" 2>/dev/null || true)
  [[ -z $resolved && -n $value ]] && resolved=$value
done
machine=$(jq -cn --arg os "$os" --arg cpu "$cpu" --arg rustc "$rustc" --arg power "$power" --arg thermal "$thermal" '{os:$os,cpu:$cpu,gpu:"unavailable",driver:"unavailable",rustc:$rustc,power_policy:$power,thermal_policy:$thermal}')
backend_identity=$(jq -cn --arg requested "$requested" --arg resolved "$resolved" '{requested:$requested,resolved:$resolved,device:"",driver:""}')
diff_summary=$(jq -cn --arg stat "$diff_stat" --arg paths_sha256 "$paths_sha" '{stat:$stat,paths_sha256:$paths_sha256}')
result=$(jq -cn --arg status "$status" '{verified:true,status:$status}')
notes=$(jq -cn '{task_contract:"pi-casso-search-throughput"}')

mkdir -p "$(dirname "$output")"
temp_output=$(mktemp "$(dirname "$output")/.manifest.XXXXXX")
if [[ -n $task ]]; then
  jq -n --argjson schema_version 1 --argjson task "$task" --arg status "$status" --arg skip_reason "$skip_reason" --arg plan_sha256 "$plan_sha" --arg git_sha "$git_sha" --arg base_sha "$base_sha" --argjson commands "$commands" --argjson machine "$machine" --argjson raw_files "$raw_files" --argjson raw_file_digests "$raw_digests" --argjson directory_digests "$directory_digests" --argjson path_manifests "$path_manifests_json" --argjson diff_summary "$diff_summary" --argjson backend_identity "$backend_identity" --argjson result "$result" --arg log_file "$log_file" --argjson log_bytes "$log_bytes" --arg log_sha256 "$log_sha" --argjson notes "$notes" '{schema_version:$schema_version,plan_sha256:$plan_sha256,task:$task,status:$status,commands:$commands,git_sha:$git_sha,base_sha:$base_sha,machine:$machine,raw_files:$raw_files,raw_file_digests:$raw_file_digests,directory_digests:$directory_digests,path_manifests:$path_manifests,diff_summary:$diff_summary,backend_identity:$backend_identity,result:$result,log_file:$log_file,log_bytes:$log_bytes,log_sha256:$log_sha256,skip_reason:$skip_reason,notes:$notes}' > "$temp_output"
else
  jq -n --argjson schema_version 1 --arg gate "$gate" --arg status "$status" --arg skip_reason "$skip_reason" --arg plan_sha256 "$plan_sha" --arg base_sha "$base_sha" --arg final_git_sha "$git_sha" --argjson commands "$commands" --argjson raw_files "$raw_files" --argjson raw_file_digests "$raw_digests" --argjson directory_digests "$directory_digests" --argjson path_manifests "$path_manifests_json" --arg log_file "$log_file" --argjson log_bytes "$log_bytes" --arg log_sha256 "$log_sha" --argjson result "$result" '{schema_version:$schema_version,gate:$gate,status:$status,skip_reason:$skip_reason,plan_sha256:$plan_sha256,base_sha:$base_sha,final_git_sha:$final_git_sha,commands:$commands,raw_files:$raw_files,raw_file_digests:$raw_file_digests,directory_digests:$directory_digests,path_manifests:$path_manifests,log_file:$log_file,log_bytes:$log_bytes,log_sha256:$log_sha256,result:$result}' > "$temp_output"
fi
mv -f -- "$temp_output" "$output"
