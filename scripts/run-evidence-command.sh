#!/usr/bin/env bash
set -euo pipefail

commands_json=""
log_file=""
expected_exit=0
while (($#)); do
  case "$1" in
    --commands-json) commands_json=${2:?}; shift 2 ;;
    --log) log_file=${2:?}; shift 2 ;;
    --expected-exit) expected_exit=${2:?}; shift 2 ;;
    --) shift; break ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
if [[ -z $commands_json || -z $log_file ]] || (($# == 0)); then
  echo "usage: run-evidence-command.sh --commands-json PATH --log PATH [--expected-exit CODE] -- COMMAND..." >&2
  exit 2
fi
[[ $expected_exit =~ ^[0-9]+$ ]] || { echo "expected exit must be an integer" >&2; exit 2; }
if [[ -n ${PI_CASSO_DATA_DIR:-} || -n ${PI_CASSO_CONFIG:-} ]]; then
  echo "PI_CASSO_DATA_DIR and PI_CASSO_CONFIG must be unset" >&2
  exit 2
fi
unset PI_CASSO_DATA_DIR PI_CASSO_CONFIG

owned_root=""
xdg_count=0
for value in "${XDG_DATA_HOME:-}" "${XDG_CONFIG_HOME:-}" "${TMPDIR:-}"; do
  [[ -n $value ]] && xdg_count=$((xdg_count + 1))
done
if ((xdg_count == 0)); then
  owned_root=$(mktemp -d)
  export XDG_DATA_HOME="$owned_root/data"
  export XDG_CONFIG_HOME="$owned_root/config"
  export TMPDIR="$owned_root/tmp"
elif ((xdg_count != 3)); then
  echo "XDG_DATA_HOME, XDG_CONFIG_HOME and TMPDIR must be supplied together" >&2
  exit 2
fi
absolute_path() {
  case "$1" in
    /*) printf '%s\n' "$1" ;;
    *) printf '%s/%s\n' "$PWD" "$1" ;;
  esac
}
XDG_DATA_HOME=$(absolute_path "$XDG_DATA_HOME")
XDG_CONFIG_HOME=$(absolute_path "$XDG_CONFIG_HOME")
TMPDIR=$(absolute_path "$TMPDIR")
export XDG_DATA_HOME XDG_CONFIG_HOME TMPDIR
mkdir -p "$XDG_DATA_HOME" "$XDG_CONFIG_HOME" "$TMPDIR"

cleanup() {
  if [[ -n $owned_root && -d $owned_root && ! -L $owned_root ]]; then
    case "$owned_root" in
      /tmp/*|/var/tmp/*) rm -rf -- "$owned_root" ;;
      *) echo "refusing to clean unexpected private root" >&2 ;;
    esac
  fi
}
trap cleanup EXIT INT TERM

mkdir -p "$(dirname "$commands_json")" "$(dirname "$log_file")"
[[ -e $commands_json ]] || printf '[]\n' > "$commands_json"
jq -e 'type == "array"' "$commands_json" >/dev/null

actual_argv_json=$(jq -cn --args '$ARGS.positional' -- "$@")
argv_sha256=$(printf '%s' "$actual_argv_json" | sha256sum | cut -d' ' -f1)
redacted=("$@")
secret_paths=()
for ((index = 0; index < ${#redacted[@]}; index++)); do
  if [[ ${redacted[index]} == --y-cruncher-path && $((index + 1)) -lt ${#redacted[@]} ]]; then
    secret_paths+=("${redacted[index + 1]}")
    redacted[index + 1]="<redacted-y-cruncher-path>"
  fi
done
if [[ -n ${PI_CASSO_TEST_YCRUNCHER_PATH:-} ]]; then
  secret_paths+=("$PI_CASSO_TEST_YCRUNCHER_PATH")
fi
redacted_argv_json=$(jq -cn --args '$ARGS.positional' -- "${redacted[@]}")

stdout_file=$(mktemp "$TMPDIR/evidence-stdout.XXXXXX")
stderr_file=$(mktemp "$TMPDIR/evidence-stderr.XXXXXX")
set +e
"$@" >"$stdout_file" 2>"$stderr_file"
actual_exit=$?
set -e
for secret_path in "${secret_paths[@]}"; do
  escaped_path=$(printf '%s' "$secret_path" | sed 's/[][\.^$*+?{}|()\/]/\\&/g')
  sed -i "s/$escaped_path/<redacted-y-cruncher-path>/g" "$stdout_file" "$stderr_file"
done

test_y_cruncher_path=""
test_y_cruncher_sha256=""
if [[ -n ${PI_CASSO_TEST_YCRUNCHER_PATH:-} ]]; then
  test_y_cruncher_path="<redacted-y-cruncher-path>"
  if [[ -f $PI_CASSO_TEST_YCRUNCHER_PATH ]]; then
    test_y_cruncher_sha256=$(sha256sum "$PI_CASSO_TEST_YCRUNCHER_PATH" | cut -d' ' -f1)
  fi
fi

env_json=$(jq -cn \
  --arg xdg_data "$XDG_DATA_HOME" \
  --arg xdg_config "$XDG_CONFIG_HOME" \
  --arg tmpdir "$TMPDIR" \
  --arg test_mode "${PI_CASSO_TEST_MODE:-}" \
  --arg generator_variant "${PI_CASSO_TEST_GENERATOR_VARIANT:-}" \
  --arg test_y_cruncher_path "$test_y_cruncher_path" \
  --arg test_y_cruncher_sha256 "$test_y_cruncher_sha256" \
  --arg fake_wgpu_preflight "${PI_CASSO_TEST_FAKE_WGPU_PREFLIGHT:-}" \
  --arg fake_wgpu_execution "${PI_CASSO_TEST_FAKE_WGPU_EXECUTION:-}" \
  --arg fake_cuda_preflight "${PI_CASSO_TEST_FAKE_CUDA_PREFLIGHT:-}" \
  --arg fake_cuda_execution "${PI_CASSO_TEST_FAKE_CUDA_EXECUTION:-}" \
  --arg cuda_artifact_root "${PI_CASSO_TEST_CUDA_ARTIFACT_ROOT:-}" \
  --arg gpu_completion_delay_ms "${PI_CASSO_TEST_GPU_COMPLETION_DELAY_MS:-}" \
  --arg backend_fail_after_preflight "${PI_CASSO_TEST_BACKEND_FAIL_AFTER_PREFLIGHT:-}" \
  --arg stress_runtime_fault "${PI_CASSO_TEST_STRESS_RUNTIME_FAULT:-}" \
  --arg fail_if_source_open "${PI_CASSO_TEST_FAIL_IF_SOURCE_OPEN:-}" \
  --arg min_reservation_bytes "${PI_CASSO_TEST_MIN_RESERVATION_BYTES:-}" \
  '{PI_CASSO_TEST_BACKEND_FAIL_AFTER_PREFLIGHT:$backend_fail_after_preflight,PI_CASSO_TEST_CUDA_ARTIFACT_ROOT:$cuda_artifact_root,PI_CASSO_TEST_FAIL_IF_SOURCE_OPEN:$fail_if_source_open,PI_CASSO_TEST_FAKE_CUDA_EXECUTION:$fake_cuda_execution,PI_CASSO_TEST_FAKE_CUDA_PREFLIGHT:$fake_cuda_preflight,PI_CASSO_TEST_FAKE_WGPU_EXECUTION:$fake_wgpu_execution,PI_CASSO_TEST_FAKE_WGPU_PREFLIGHT:$fake_wgpu_preflight,PI_CASSO_TEST_GENERATOR_VARIANT:$generator_variant,PI_CASSO_TEST_GPU_COMPLETION_DELAY_MS:$gpu_completion_delay_ms,PI_CASSO_TEST_MIN_RESERVATION_BYTES:$min_reservation_bytes,PI_CASSO_TEST_MODE:$test_mode,PI_CASSO_TEST_STRESS_RUNTIME_FAULT:$stress_runtime_fault,PI_CASSO_TEST_YCRUNCHER_EXECUTABLE_SHA256:$test_y_cruncher_sha256,PI_CASSO_TEST_YCRUNCHER_PATH:$test_y_cruncher_path,TMPDIR:$tmpdir,XDG_CONFIG_HOME:$xdg_config,XDG_DATA_HOME:$xdg_data}')
record=$(jq -cn \
  --argjson argv "$redacted_argv_json" \
  --arg argv_sha256 "$argv_sha256" \
  --argjson env "$env_json" \
  --argjson expected_exit_code "$expected_exit" \
  --argjson exit_code "$actual_exit" \
  '{argv:$argv,argv_sha256:$argv_sha256,env:$env,expected_exit_code:$expected_exit_code,exit_code:$exit_code}')
update_file=$(mktemp "$(dirname "$commands_json")/.commands.XXXXXX")
jq --argjson record "$record" '. + [$record]' "$commands_json" > "$update_file"
mv -f -- "$update_file" "$commands_json"

{
  jq -cn --argjson argv "$redacted_argv_json" --argjson expected "$expected_exit" --argjson actual "$actual_exit" '{argv:$argv,expected_exit:$expected,actual_exit:$actual}'
  sed 's/^/[stdout] /' "$stdout_file"
  sed 's/^/[stderr] /' "$stderr_file"
} >> "$log_file"
cat "$stdout_file"
cat "$stderr_file" >&2

cleanup
trap - EXIT INT TERM
if ((actual_exit != expected_exit)); then
  if ((actual_exit == 0)); then
    exit 1
  fi
  exit "$actual_exit"
fi
exit "$actual_exit"
