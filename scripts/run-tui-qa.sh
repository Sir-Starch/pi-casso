#!/usr/bin/env bash
set -euo pipefail

output=""
raw=""
transcript=""
commands_json=""
log_file=""
xdg_root=""
run_id=""
mode=""
timeout_seconds=30
resume_args=()

usage() {
  echo "usage: $0 --output TRANSCRIPT --raw PATH -- COMMAND..." >&2
  echo "   or: $0 --run-id RUN_ID --resume-in-app|--cli-resume --commands-json PATH --log PATH --transcript PATH --xdg-root ROOT --raw PATH [OPTIONS] [-- COMMAND...]" >&2
}
set_mode() {
  local requested=$1
  if [[ -n $mode && $mode != "$requested" ]]; then
    echo "resume modes are mutually exclusive" >&2
    exit 2
  fi
  mode=$requested
}

while (($#)); do
  case "$1" in
    --output) output=${2:?}; shift 2 ;;
    --raw) raw=${2:?}; shift 2 ;;
    --transcript) transcript=${2:?}; shift 2 ;;
    --commands-json) commands_json=${2:?}; shift 2 ;;
    --log) log_file=${2:?}; shift 2 ;;
    --xdg-root) xdg_root=${2:?}; shift 2 ;;
    --run-id) run_id=${2:?}; shift 2 ;;
    --resume-in-app) set_mode in_app; shift ;;
    --cli-resume) set_mode cli; shift ;;
    --timeout-seconds) timeout_seconds=${2:?}; shift 2 ;;
    --work-windows|--profile|--backend|--gpu|--gpu-device|--generator-backend|--cpu-workers|--workers|--chunk-size|--queue-depth|--memory-limit-mb|--ui-refresh-ms|--max-fps|--limit|--max-offset|--checkpoint-every)
      resume_args+=("$1" "${2:?}")
      shift 2
      ;;
    --show-metrics|--no-show-metrics|--yes|--force)
      resume_args+=("$1")
      shift
      ;;
    --) shift; break ;;
    *) echo "unknown argument: $1" >&2; usage; exit 2 ;;
  esac
done

if [[ -z $mode ]]; then
  [[ -n $output && -n $raw && $# -gt 0 ]] || { usage; exit 2; }
  [[ ${#resume_args[@]} -eq 0 ]] || { echo "resume options require a resume mode" >&2; exit 2; }
  [[ -z $transcript && -z $commands_json && -z $log_file && -z $xdg_root && -z $run_id ]] || {
    echo "Task 13 options require a resume mode" >&2
    exit 2
  }
  [[ $timeout_seconds =~ ^[1-9][0-9]*$ ]] || { echo "timeout must be positive" >&2; exit 2; }
  mkdir -p "$(dirname "$output")" "$(dirname "$raw")"
  command_string=$(printf '%q ' "$@")
  pty_command="stty rows 40 cols 120 2>/dev/null || true; exec $command_string"
  set +e
  printf 'q' | TERM=xterm-256color COLUMNS=120 LINES=40 timeout --signal=TERM --kill-after=5s "$timeout_seconds" script --quiet --return --command "$pty_command" "$output"
  child_exit=$?
  set -e
  timed_out=false
  [[ $child_exit -eq 124 || $child_exit -eq 137 ]] && timed_out=true
  jq -n --argjson child_exit "$child_exit" --argjson timed_out "$timed_out" --arg transcript "$output" '{schema_version:1,status:(if $child_exit==0 then "ok" else "failed" end),child_exit:$child_exit,timed_out:$timed_out,terminal:{term:"xterm-256color",columns:120,lines:40,pty:true},transcript:$transcript}' > "$raw"
  exit "$child_exit"
fi

[[ -n $run_id && -n $commands_json && -n $log_file && -n $transcript && -n $xdg_root && -n $raw ]] || {
  usage
  exit 2
}
[[ $timeout_seconds =~ ^[1-9][0-9]*$ ]] || { echo "timeout must be positive" >&2; exit 2; }
[[ $run_id =~ ^[A-Za-z0-9][A-Za-z0-9._:-]*$ ]] || { echo "unsafe run id" >&2; exit 2; }
for value in "$commands_json" "$log_file" "$transcript" "$raw" "$xdg_root"; do
  [[ $value != *$'\n'* && $value != *$'\r'* ]] || { echo "path contains a line break" >&2; exit 2; }
done
if [[ -n ${PI_CASSO_DATA_DIR:-} || -n ${PI_CASSO_CONFIG:-} ]]; then
  echo "PI_CASSO_DATA_DIR and PI_CASSO_CONFIG must be unset" >&2
  exit 2
fi
unset PI_CASSO_DATA_DIR PI_CASSO_CONFIG
case "$xdg_root" in
  /*) ;;
  *) xdg_root="$PWD/$xdg_root" ;;
esac
[[ ! -L $xdg_root && (! -e $xdg_root || -d $xdg_root) ]] || {
  echo "XDG root must be a non-symlink directory" >&2
  exit 2
}
mkdir -p "$xdg_root/data" "$xdg_root/config" "$xdg_root/tmp" "$(dirname "$commands_json")" "$(dirname "$log_file")" "$(dirname "$transcript")" "$(dirname "$raw")"
[[ -e $commands_json ]] || printf '[]\n' > "$commands_json"
jq -e 'type == "array"' "$commands_json" >/dev/null
export XDG_DATA_HOME="$xdg_root/data" XDG_CONFIG_HOME="$xdg_root/config" TMPDIR="$xdg_root/tmp" TERM=xterm-256color COLUMNS=120 LINES=40
quote_argv() {
  local argument quoted
  for argument in "$@"; do
    printf -v quoted '%q' "$argument"
    printf '%s ' "$quoted"
  done
}
command=()
if (($#)); then
  command=("$@")
elif [[ $mode == cli ]]; then
  command=(cargo run --release --locked -- resume "$run_id" --tui "${resume_args[@]}")
else
  command=(cargo run --release --locked --)
fi
if [[ $mode == cli ]]; then input_keys=q; else input_keys=2rq; fi
command_string=$(quote_argv "${command[@]}")
pty_command="stty rows 40 cols 120 2>/dev/null || true; exec $command_string"

runner_stdout=$(mktemp "$TMPDIR/tui-qa-stdout.XXXXXX")
runner_stderr=$(mktemp "$TMPDIR/tui-qa-stderr.XXXXXX")
plain_transcript=$(mktemp "$TMPDIR/tui-qa-plain.XXXXXX")
cleanup() {
  rm -f -- "$runner_stdout" "$runner_stderr" "$plain_transcript"
}
trap cleanup EXIT INT TERM
: > "$transcript"
runner="$PWD/scripts/run-evidence-command.sh"
[[ -x $runner ]] || runner="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/run-evidence-command.sh"
set +e
printf '%s' "$input_keys" | "$runner" --commands-json "$commands_json" --log "$log_file" --expected-exit 0 -- timeout --signal=TERM --kill-after=5s "$timeout_seconds" script --quiet --return --command "$pty_command" "$transcript" >"$runner_stdout" 2>"$runner_stderr"
child_exit=$?
set -e

redact_file() {
  local path=$1 temporary
  [[ -f $path ]] || return 0
  temporary=$(mktemp "$TMPDIR/tui-qa-redact.XXXXXX")
  if command -v perl >/dev/null 2>&1; then
    REDACT_ROOT="$xdg_root" perl -0pe 's/\Q$ENV{REDACT_ROOT}\E/<xdg-root>/g' "$path" > "$temporary"
  else
    jq -Rsr --arg root "$xdg_root" 'split($root) | join("<xdg-root>")' "$path" > "$temporary"
  fi
  mv -- "$temporary" "$path"
}
redact_file "$transcript"
redact_file "$log_file"
redact_file "$runner_stdout"
redact_file "$runner_stderr"
jq --arg root "$xdg_root" 'walk(if type == "string" then (split($root) | join("<xdg-root>")) else . end)' "$commands_json" > "$commands_json.tmp"
mv -- "$commands_json.tmp" "$commands_json"
if command -v perl >/dev/null 2>&1; then
  perl -0pe 's/\e\][^\a]*(?:\a|\e\\)//g; s/\e\[[0-?]*[ -\/]*[@-~]//g; s/\e\([0-2A-Z0-9]//g' "$transcript" > "$plain_transcript"
else
  sed -E 's/\033\[[0-9;?]*[ -\/]*[@-~]//g' "$transcript" > "$plain_transcript"
fi

contains_backend=false
contains_queue=false
contains_wait=false
contains_stop_reason=false
contains_resume=false
grep -aEiq 'backend([[:space:]]*[:= -]+|[[:space:]]+)cpu' "$plain_transcript" && contains_backend=true
grep -aEiq 'queue' "$plain_transcript" && contains_queue=true
grep -aEiq 'wait' "$plain_transcript" && contains_wait=true
grep -aEiq 'stop([[:space:]_-]*reason)' "$plain_transcript" && contains_stop_reason=true
grep -aEiq 'resume' "$plain_transcript" && contains_resume=true

extract_number() {
  local key=$1
  grep -aEio "${key}[[:space:]]*[:=][[:space:]]*[0-9]+" "$plain_transcript" | tail -n 1 | grep -aoE '[0-9]+' | tail -n 1 || true
}
extract_bool() {
  local key=$1
  grep -aEio "${key}[[:space:]]*[:=][[:space:]]*(true|false)" "$plain_transcript" | tail -n 1 | grep -aoE '(true|false)$' | tail -n 1 || true
}
extract_run_id() {
  grep -aEio '(resume_run_id|resume[[:space:]]+run[[:space:]]+id)[[:space:]]*[:=][[:space:]]*[A-Za-z0-9._:-]+' "$plain_transcript" | tail -n 1 | sed -E 's/^[^:=]+[[:space:]]*[:=][[:space:]]*//I' || true
}

max_fps=$(extract_number max_fps)
ui_refresh_ms=$(extract_number ui_refresh_ms)
resume_state_restored=$(extract_bool resume_state_restored)
event_loop_handoff=$(extract_bool event_loop_handoff)
resume_run_id=$(extract_run_id)
[[ $resume_state_restored == true ]] || resume_state_restored=false
[[ $event_loop_handoff == true ]] || event_loop_handoff=false

assertions_ok=true
for marker in "$contains_backend" "$contains_queue" "$contains_wait" "$contains_stop_reason" "$contains_resume"; do
  [[ $marker == true ]] || assertions_ok=false
done
[[ $child_exit -eq 0 && $max_fps == 60 && $ui_refresh_ms == 1000 && $resume_state_restored == true && $event_loop_handoff == true && $resume_run_id == "$run_id" ]] || assertions_ok=false

if [[ -n $max_fps ]]; then max_fps_json=$max_fps; else max_fps_json=null; fi
if [[ -n $ui_refresh_ms ]]; then ui_refresh_ms_json=$ui_refresh_ms; else ui_refresh_ms_json=null; fi
if [[ -n $resume_run_id ]]; then resume_run_id_json=$(jq -cn --arg value "$resume_run_id" '$value'); else resume_run_id_json=null; fi
timed_out=false
[[ $child_exit -eq 124 || $child_exit -eq 137 ]] && timed_out=true
transcript_ref=$transcript
case "$transcript_ref" in
  "$xdg_root"/*) transcript_ref="<xdg-root>/${transcript_ref#"$xdg_root"/}" ;;
esac

jq -n --arg mode "$mode" --arg run_id "$run_id" --arg transcript "$transcript_ref" --argjson child_exit "$child_exit" --argjson timed_out "$timed_out" --argjson contains_backend "$contains_backend" --argjson contains_queue "$contains_queue" --argjson contains_wait "$contains_wait" --argjson contains_stop_reason "$contains_stop_reason" --argjson contains_resume "$contains_resume" --argjson max_fps "$max_fps_json" --argjson ui_refresh_ms "$ui_refresh_ms_json" --argjson resume_state_restored "$resume_state_restored" --argjson event_loop_handoff "$event_loop_handoff" --argjson resume_run_id "$resume_run_id_json" --argjson assertions_ok "$assertions_ok" '{schema_version:1,status:(if ($child_exit == 0 and $assertions_ok) then "ok" else "failed" end),child_exit:$child_exit,timed_out:$timed_out,terminal:{term:"xterm-256color",columns:120,lines:40,pty:true},mode:$mode,run_id:$run_id,transcript:$transcript,transcript_contains:{backend:$contains_backend,queue:$contains_queue,wait:$contains_wait,stop_reason:$contains_stop_reason,resume:$contains_resume},snapshot_ui:{max_fps:$max_fps,ui_refresh_ms:$ui_refresh_ms},resume_state_restored:$resume_state_restored,event_loop_handoff:$event_loop_handoff,resume_run_id:$resume_run_id,assertions_ok:$assertions_ok}' > "$raw"

if [[ $child_exit -ne 0 ]]; then
  cleanup
  trap - EXIT INT TERM
  exit "$child_exit"
fi
if [[ $assertions_ok != true ]]; then
  echo "TUI evidence assertions failed; inspect $transcript and $raw" >&2
  cleanup
  trap - EXIT INT TERM
  exit 1
fi
cleanup
trap - EXIT INT TERM
exit 0
