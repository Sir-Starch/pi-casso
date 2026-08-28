#!/usr/bin/env bash
set -euo pipefail

timeout_seconds=120
xdg_root=""
artifact_prefix=""
raw_path=""
commands_json=""
log_file=""
evidence_dir=".omo/evidence"
print_run_id=false
test_target=""
while (($#)); do
  case "$1" in
    --timeout-seconds) timeout_seconds=${2:?}; shift 2 ;;
    --xdg-root) xdg_root=${2:?}; shift 2 ;;
    --artifact-prefix) artifact_prefix=${2:?}; shift 2 ;;
    --raw) raw_path=${2:?}; shift 2 ;;
    --commands-json) commands_json=${2:?}; shift 2 ;;
    --log) log_file=${2:?}; shift 2 ;;
    --evidence-dir) evidence_dir=${2:?}; shift 2 ;;
    --test-target) test_target=${2:?}; shift 2 ;;
    --print-run-id) print_run_id=true; shift ;;
    --) shift; break ;;
    -*) echo "unknown argument: $1" >&2; exit 2 ;;
    *) break ;;
  esac
done
if [[ "$#" -eq 2 && $2 == --print-run-id ]]; then
  print_run_id=true
  set -- "$1"
fi
test "$#" -eq 1 || { echo "one test name is required" >&2; exit 2; }
test_name=$1
[[ $timeout_seconds =~ ^[1-9][0-9]*$ ]] || { echo "timeout must be positive" >&2; exit 2; }
[[ $test_name =~ ^[A-Za-z0-9_:-]+$ ]] || { echo "unsafe test name" >&2; exit 2; }
[[ -z $test_target || $test_target =~ ^[A-Za-z0-9_.:-]+$ ]] || {
  echo "unsafe test target" >&2
  exit 2
}
if [[ -n ${PI_CASSO_DATA_DIR:-} || -n ${PI_CASSO_CONFIG:-} ]]; then
  echo "PI_CASSO_DATA_DIR and PI_CASSO_CONFIG must be unset" >&2
  exit 2
fi
unset PI_CASSO_DATA_DIR PI_CASSO_CONFIG
[[ $xdg_root != *$'\n'* && $xdg_root != *$'\r'* ]] || { echo "XDG root contains a line break" >&2; exit 2; }
case "$xdg_root" in
  /*) ;;
  *) xdg_root="$PWD/$xdg_root" ;;
esac
[[ ! -L $xdg_root && (! -e $xdg_root || -d $xdg_root) ]] || {
  echo "XDG root must be a non-symlink directory" >&2
  exit 2
}

if [[ -z $artifact_prefix ]]; then
  artifact_prefix=standalone
else
  [[ $artifact_prefix =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]] || {
    echo "artifact prefix is not path-safe" >&2
    exit 2
  }
fi
artifact_dir="$evidence_dir/named-tests/$artifact_prefix"
path_has_symlink() {
  local probe=$1
  while [[ $probe != / && $probe != . ]]; do
    [[ -L $probe ]] && return 0
    probe=$(dirname -- "$probe")
  done
  return 1
}
path_has_symlink "$artifact_dir" && { echo "artifact directory contains a symlink" >&2; exit 2; }
mkdir -p "$artifact_dir"
[[ -d $artifact_dir && ! -L $artifact_dir ]] || { echo "unsafe artifact directory" >&2; exit 2; }
artifact_dir_canonical=$(realpath -e -- "$artifact_dir")
evidence_dir_canonical=$(realpath -e -- "$evidence_dir")
raw_path=${raw_path:-"$artifact_dir/$test_name.raw.json"}
commands_json=${commands_json:-"$artifact_dir/$test_name.commands.json"}
log_file=${log_file:-"$artifact_dir/$test_name.log"}
paths_file="$artifact_dir/$test_name.paths.json"
artifact_path_allowed() {
  local path=$1 canonical
  case "/$path/" in */../*|*/./*) return 1;; esac
  path_has_symlink "$path" && return 1
  canonical=$(realpath -m -- "$path")
  case "$canonical" in
    "$artifact_dir_canonical"/*) return 0 ;;
    "$evidence_dir_canonical"/task-13-*-raw.json|"$evidence_dir_canonical"/task-13-*-commands.json|"$evidence_dir_canonical"/task-13-*.log)
      [[ $artifact_prefix == task-13-* ]]
      return
      ;;
    *) return 1 ;;
  esac
}
for path in "$raw_path" "$commands_json" "$log_file" "$paths_file"; do
  artifact_path_allowed "$path" || { echo "artifact path escapes prefix" >&2; exit 2; }
  [[ ! -e $path && ! -L $path ]] || { echo "artifact path already exists: $path" >&2; exit 2; }
done

owned_root=""
if [[ -z $xdg_root ]]; then
  owned_root=$(mktemp -d)
  xdg_root=$owned_root
fi
mkdir -p "$xdg_root/data" "$xdg_root/config" "$xdg_root/tmp"
xdg_root=$(cd -- "$xdg_root" && pwd)
cleanup() {
  if [[ -n $owned_root && -d $owned_root && ! -L $owned_root ]]; then
    case "$owned_root" in /tmp/*|/var/tmp/*) rm -rf -- "$owned_root";; esac
  fi
}
trap cleanup EXIT INT TERM

stdout_file=$(mktemp "$xdg_root/tmp/named-test-stdout.XXXXXX")
stderr_file=$(mktemp "$xdg_root/tmp/named-test-stderr.XXXXXX")
paths_temp=$(mktemp "$xdg_root/tmp/named-test-paths.XXXXXX")
export XDG_DATA_HOME="$xdg_root/data"
export XDG_CONFIG_HOME="$xdg_root/config"
export TMPDIR="$xdg_root/tmp"
export PI_CASSO_TEST_MODE="${PI_CASSO_TEST_MODE:-1}"
export CARGO_TARGET_DIR="${PI_CASSO_NAMED_TEST_TARGET_DIR:-target/named-tests}"
cargo_args=(test --locked --all-features)
if [[ -n $test_target ]]; then
  cargo_args+=(--test "$test_target")
fi
set +e
timeout --signal=TERM --kill-after=5s "$timeout_seconds" \
  scripts/run-evidence-command.sh --commands-json "$commands_json" --log "$log_file" --expected-exit 0 -- \
  cargo "${cargo_args[@]}" "$test_name" -- --nocapture >"$stdout_file" 2>"$stderr_file"
child_exit=$?
set -e
timed_out=false
[[ $child_exit -eq 124 || $child_exit -eq 137 ]] && timed_out=true
redact_private_root() {
  local path=$1 temporary
  [[ -f $path ]] || return 0
  temporary=$(mktemp "$xdg_root/tmp/named-test-redact.XXXXXX")
  if command -v perl >/dev/null 2>&1; then
    REDACT_ROOT="$xdg_root" perl -0pe 's/\Q$ENV{REDACT_ROOT}\E/<xdg-root>/g' "$path" > "$temporary"
  else
    jq -Rsr --arg root "$xdg_root" 'split($root) | join("<xdg-root>")' "$path" > "$temporary"
  fi
  mv -- "$temporary" "$path"
}
redact_private_root "$stdout_file"
redact_private_root "$stderr_file"
redact_private_root "$log_file"
jq --arg root "$xdg_root" 'walk(if type == "string" then (split($root) | join("<xdg-root>")) else . end)' "$commands_json" > "$commands_json.tmp"
mv -- "$commands_json.tmp" "$commands_json"
stdout_sha=$(sha256sum "$stdout_file" | cut -d' ' -f1)
stderr_sha=$(sha256sum "$stderr_file" | cut -d' ' -f1)
jq -n \
  --arg test_name "$test_name" \
  --argjson child_exit "$child_exit" \
  --argjson timed_out "$timed_out" \
  --arg stdout_sha256 "$stdout_sha" \
  --arg stderr_sha256 "$stderr_sha" \
  --arg xdg_data_home "<xdg-root>/data" \
  --arg xdg_config_home "<xdg-root>/config" \
  --arg tmpdir "<xdg-root>/tmp" \
  '{schema_version:1,test_name:$test_name,child_exit:$child_exit,timed_out:$timed_out,env:{PI_CASSO_DATA_DIR:"",PI_CASSO_CONFIG:"",TMPDIR:$tmpdir,XDG_CONFIG_HOME:$xdg_config_home,XDG_DATA_HOME:$xdg_data_home},stdout_sha256:$stdout_sha256,stderr_sha256:$stderr_sha256,assertions:{completed:($child_exit == 0),within_timeout:($timed_out|not)}}' > "$raw_path"
observed_json=$(jq -Rrc 'fromjson? | select(type == "object")' "$stdout_file" | tail -n 1)
if [[ -n $observed_json ]]; then
  jq --argjson observed "$observed_json" '. + {observed:$observed}' "$raw_path" > "$raw_path.tmp"
  mv -- "$raw_path.tmp" "$raw_path"
fi
jq -n --arg artifact_prefix "$artifact_prefix" --arg test_name "$test_name" --arg raw "$raw_path" --arg commands_json "$commands_json" --arg log "$log_file" --arg paths "$paths_file" '{schema_version:1,artifact_prefix:$artifact_prefix,test_name:$test_name,raw:$raw,commands_json:$commands_json,log:$log,paths:$paths}' > "$paths_temp"
mv -- "$paths_temp" "$paths_file"
redact_private_root "$paths_file"
cat "$stdout_file" >> "$log_file"
cat "$stderr_file" >> "$log_file"
redact_private_root "$log_file"
if $print_run_id; then
  seeded_run_id=$(grep -aEio '(run_id|resume_run_id)[[:space:]]*"?[[:space:]]*[:=][[:space:]]*"?[A-Za-z0-9._:-]+"?' "$stdout_file" \
    | tail -n 1 \
    | sed -E 's/.*[:=][[:space:]]*"?([A-Za-z0-9._:-]+)"?/\1/' || true)
  if [[ -z $seeded_run_id ]]; then
    while IFS= read -r line; do
      if [[ $line =~ ^[[:space:]]*[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}[[:space:]]*$ ]]; then
        seeded_run_id=${line//[[:space:]]/}
      fi
    done < "$stdout_file"
  fi
  [[ -n $seeded_run_id ]] || { echo "--print-run-id requires the named test to emit run_id" >&2; exit 1; }
  printf '%s\n' "$seeded_run_id"
fi
cleanup
trap - EXIT INT TERM
exit "$child_exit"
