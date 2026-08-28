#!/usr/bin/env bash
set -euo pipefail

mode=""
output=""
while (($#)); do
  case "$1" in
    --mode) mode=${2:?}; shift 2 ;;
    --output) output=${2:?}; shift 2 ;;
    --) shift; break ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[[ $mode == baseline || $mode == measured ]] || { echo "mode must be baseline or measured" >&2; exit 2; }
if [[ -z $output ]] || (($# == 0)); then
  echo "output and command are required" >&2
  exit 2
fi
mkdir -p "$(dirname "$output")"
time_output=$(mktemp "${TMPDIR:-/tmp}/pi-casso-time.XXXXXX")
stdout_file=$(mktemp "${TMPDIR:-/tmp}/pi-casso-memory-stdout.XXXXXX")
stderr_file=$(mktemp "${TMPDIR:-/tmp}/pi-casso-memory-stderr.XXXXXX")

method="time-v"
set +e
/usr/bin/time -v -o "$time_output" "$@" >"$stdout_file" 2>"$stderr_file"
child_exit=$?
set -e
rss_kb=$(awk -F: '/Maximum resident set size/ {gsub(/^[[:space:]]+/, "", $2); print $2}' "$time_output")
if [[ ! $rss_kb =~ ^[0-9]+$ ]]; then
  method="proc-sample"
  : > "$stdout_file"
  : > "$stderr_file"
  set +e
  "$@" >"$stdout_file" 2>"$stderr_file" &
  child_pid=$!
  set -e
  rss_kb=0
  while kill -0 "$child_pid" 2>/dev/null; do
    sample=$(awk '/VmRSS:/ {print $2}' "/proc/$child_pid/status" 2>/dev/null || true)
    [[ $sample =~ ^[0-9]+$ ]] && ((sample > rss_kb)) && rss_kb=$sample
    sleep 0.02
  done
  set +e
  wait "$child_pid"
  child_exit=$?
  set -e
fi
rss_peak_mb=$(((rss_kb + 1023) / 1024))
rss_baseline_mb=0
[[ $mode == baseline ]] && rss_baseline_mb=$rss_peak_mb
margin=$(((rss_baseline_mb + 9) / 10))
((margin < 64)) && margin=64
jq -n \
  --arg mode "$mode" \
  --arg measurement_method "$method" \
  --argjson child_exit "$child_exit" \
  --argjson rss_baseline_mb "$rss_baseline_mb" \
  --argjson rss_peak_mb "$rss_peak_mb" \
  --argjson rss_margin_mb "$margin" \
  --arg os "$(uname -srm)" \
  '{schema_version:1,mode:$mode,measurement_method:$measurement_method,child_exit:$child_exit,rss_baseline_mb:$rss_baseline_mb,rss_peak_mb:$rss_peak_mb,rss_margin_mb:$rss_margin_mb,os:$os}' > "$output"
cat "$stdout_file"
cat "$stderr_file" >&2
exit "$child_exit"
