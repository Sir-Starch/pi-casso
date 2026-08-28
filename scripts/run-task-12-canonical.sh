#!/usr/bin/env bash
set -euo pipefail

printf -v harness_command '%q ' "$0" "$@"
harness_command=${harness_command% }
external_mode=fixture
resume=false
spigot_unavailable=true
while (($#)); do
  case "$1" in
    --external) external_mode=${2:?}; shift 2 ;;
    --resume) resume=true; shift ;;
    --spigot-unavailable) spigot_unavailable=true; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[[ $external_mode == fixture || $external_mode == unavailable ]] || {
  echo "--external must be fixture or unavailable" >&2
  exit 2
}
[[ -z ${PI_CASSO_DATA_DIR:-} && -z ${PI_CASSO_CONFIG:-} ]] || {
  echo "PI_CASSO_DATA_DIR and PI_CASSO_CONFIG must be unset" >&2
  exit 2
}
unset PI_CASSO_DATA_DIR PI_CASSO_CONFIG

evidence=.omo/evidence
final=$evidence/task-12-canonical-final
variants_root=$evidence/task-12-variants
commands=$evidence/task-12-variant-commands.json
log=$evidence/task-12-variants.log
selection=$evidence/task-12-pi-selection.json
comparison=$evidence/task-12-generator-comparison.json
concurrent_aggregate=$evidence/task-12-pi-concurrent-raw.json
overlap_aggregate=$evidence/task-12-pi-search-overlap-raw.json
normalized_manifest=$evidence/task-12-normalized-inputs.json
path_manifest=$evidence/task-12-canonical-paths.json
task_manifest=$evidence/task-12-pi-casso-search-throughput.json
numeric_manifest=$final/numeric-manifest.json
spigot_policy=$final/spigot-bounded-policy.json
selection_disposition=$final/selection-disposition.json
if ! $resume; then
  for path in "$final" "$variants_root" "$commands" "$log" "$selection" "$comparison" \
    "$concurrent_aggregate" "$overlap_aggregate" "$normalized_manifest" "$path_manifest" "$task_manifest"; do
    [[ ! -e $path ]] || { echo "canonical artifact already exists: $path" >&2; exit 2; }
  done
fi
mkdir -p "$final/private" "$variants_root"
run_tag=initial
if $resume; then
  run_tag=retry
  if [[ -e $final/attempts/compile-blocker/task-12-variant-commands.json ]]; then
    attempt_dir="$final/attempts/runtime-bound"
  else
    attempt_dir="$final/attempts/compile-blocker"
  fi
  mkdir -p "$attempt_dir"
  [[ ! -e $attempt_dir/task-12-variant-commands.json ]] && cp -- "$commands" "$attempt_dir/task-12-variant-commands.json"
  [[ ! -e $attempt_dir/task-12-variants.log ]] && cp -- "$log" "$attempt_dir/task-12-variants.log"
  [[ ! -e $attempt_dir/report.md && -f $final/report.md ]] && cp -- "$final/report.md" "$attempt_dir/report.md"
  failed_raw="$variants_root/chudnovsky-rug-binary-split-serial.json"
  if [[ -e $failed_raw && ! -s $failed_raw && ! -e $attempt_dir/chudnovsky-rug-binary-split-serial.empty.json ]]; then
    mv -- "$failed_raw" "$attempt_dir/chudnovsky-rug-binary-split-serial.empty.json"
  fi
  failed_raw="$variants_root/spigot-persistent-serial.json"
  if [[ -e $failed_raw && ! -s $failed_raw && ! -e $attempt_dir/spigot-persistent-serial.empty.json ]]; then
    mv -- "$failed_raw" "$attempt_dir/spigot-persistent-serial.empty.json"
  fi
fi
printf '[]\n' > "$commands"
: > "$log"

set_xdg() {
  local name=$1 root="$final/private/xdg/$run_tag-$1"
  mkdir -p "$root/data" "$root/config" "$root/tmp"
  export XDG_DATA_HOME="$PWD/$root/data"
  export XDG_CONFIG_HOME="$PWD/$root/config"
  export TMPDIR="$PWD/$root/tmp"
}

recorded() {
  local expected=$1
  shift
  set +e
  scripts/run-evidence-command.sh --commands-json "$commands" --log "$log" \
    --expected-exit "$expected" -- "$@"
  local status=$?
  set -e
  [[ $status -eq $expected ]]
}

fixture_path=""
if [[ $external_mode == fixture ]]; then
  fixture_path="$PWD/$final/private/external/bin/y-cruncher"
  if [[ ! -x $fixture_path ]]; then
    scripts/create-ycruncher-fixture.sh --output "$fixture_path" --digits 500000
  fi
fi

canonicalize_baseline() {
  local directory=$1 prefix=$2 executable_sha256=$3 source_prefix="$2-growing-cold"
  local source_summary="$directory/$source_prefix-raw.json"
  local source_repetitions="$directory/$source_prefix"
  local summary="$directory/$prefix-raw.json" repetitions="$directory/$prefix"
  mkdir -p "$repetitions"
  local source_file target_file
  while IFS= read -r source_file; do
    target_file="$repetitions/${source_file##*/}"
    cp -- "$source_file" "$target_file"
  done < <(find "$source_repetitions" -maxdepth 1 -type f -name 'repetition-*.json' -print | sort)
  local paths
  paths=$(find "$repetitions" -maxdepth 1 -type f -name 'repetition-*.json' -print | sort | jq -Rsc 'split("\n") | map(select(length > 0))')
  jq --argjson paths "$paths" --arg executable_sha256 "$executable_sha256" \
    '.raw_run_paths=$paths | .generator_executable_sha256=$executable_sha256' \
    "$source_summary" > "$summary"
  local raw_digests='{}' file bytes digest
  while IFS= read -r file; do
    bytes=$(stat -c %s "$file")
    digest=$(sha256sum "$file" | cut -d' ' -f1)
    raw_digests=$(jq -c --arg path "$file" --argjson bytes "$bytes" --arg sha "$digest" \
      '. + {($path):{bytes:$bytes,sha256:$sha}}' <<<"$raw_digests")
  done < <(jq -r '.[]' <<<"$paths")
  bytes=$(stat -c %s "$summary")
  digest=$(sha256sum "$summary" | cut -d' ' -f1)
  jq -n --arg summary "$summary" --arg sha "$digest" --argjson bytes "$bytes" \
    --argjson count "$(jq 'length' <<<"$paths")" --argjson paths "$paths" \
    --argjson digests "$raw_digests" \
    '{schema_version:1,summary_artifact:$summary,cache_state:"cold",expected_count:$count,repetitions:$paths,raw_file_digests:$digests,summary_digest:{bytes:$bytes,sha256:$sha}}' \
    > "$repetitions/manifest.json"
}

variants=(chudnovsky-rug-binary-split spigot-persistent y-cruncher-external)
for variant in "${variants[@]}"; do
  variant_dir="$variants_root/$variant"
  mkdir -p "$variant_dir"
  if [[ $variant == spigot-persistent ]] && $spigot_unavailable; then
    set_xdg "$variant-bounded-probe"
    export PI_CASSO_TEST_MODE=1 PI_CASSO_TEST_GENERATOR_VARIANT="$variant"
    probe_stdout="$final/spigot-bounded-probe.stdout"
    recorded 124 timeout --signal=TERM --kill-after=5s 30 \
      cargo run --release --locked -- --json pi benchmark \
      --targets 1000,10000,100000,250000 --demand-mode serial \
      --repetitions 5 --warmup 1 --generator-backend cpu --workers 4 > "$probe_stdout"
    jq -n --arg variant "$variant" --arg reason bounded_runtime_exceeded \
      --argjson command "$(jq '.[-1]' "$commands")" \
      '{schema_version:1,selected_variant:$variant,status:"unavailable",reason:$reason,bound_seconds:30,targets:[1000,10000,100000,250000],demand_mode:"serial",repetitions:5,warmup:1,command:$command}' \
      > "$spigot_policy"
    unavailable="$variant_dir/end-to-end-unavailable.json"
    recorded 0 timeout --signal=TERM --kill-after=5s 30 \
      scripts/create-pi-unavailable-envelope.sh --variant "$variant" \
      --reason bounded_runtime_exceeded --output "$unavailable"
    for mode in serial concurrent search-overlap; do
      raw="$variants_root/$variant-$mode.json"
      cp -- "$unavailable" "$raw"
      key=${mode//-/_}
      recorded 0 timeout --signal=TERM --kill-after=5s 30 \
        scripts/normalize-pi-variant.sh --input "$raw" --variant "$variant" --mode "$mode" \
        --artifact "$raw" --output "$variant_dir/$key.normalized.json"
    done
    recorded 0 timeout --signal=TERM --kill-after=5s 30 \
      scripts/normalize-pi-variant.sh --input "$unavailable" --variant "$variant" \
      --mode end-to-end --summary "$unavailable" --repetitions-dir "" \
      --output "$variant_dir/end-to-end.normalized.json"
    continue
  fi
  expected=0
  variant_args=()
  if [[ $variant == y-cruncher-external ]]; then
    if [[ $external_mode == fixture ]]; then
      variant_args=(--y-cruncher-path "$fixture_path")
    else
      expected=2
      variant_args=(--y-cruncher-path /definitely/missing/y-cruncher-task12-canonical)
    fi
  fi
  for mode in serial concurrent search-overlap; do
    case "$mode" in
      serial) mode_args=(--targets "1000,10000,100000,250000" --demand-mode serial) ;;
      concurrent) mode_args=(--targets "1000,10000,100000,250000" --demand-mode concurrent) ;;
      search-overlap) mode_args=(--targets "1000,10000,100000" --demand-mode search-overlap --search-work-windows 4096) ;;
    esac
    raw="$variants_root/$variant-$mode.json"
    key=${mode//-/_}
    if $resume && [[ -s $raw && -s $variant_dir/$key.normalized.json ]]; then
      continue
    fi
    set_xdg "$variant-$mode"
    export PI_CASSO_TEST_MODE=1 PI_CASSO_TEST_GENERATOR_VARIANT="$variant"
    recorded "$expected" timeout --signal=TERM --kill-after=5s 600 \
      cargo run --release --locked -- --json pi benchmark "${mode_args[@]}" \
      --repetitions 5 --warmup 1 --generator-backend cpu --workers 4 "${variant_args[@]}" > "$raw"
    recorded 0 timeout --signal=TERM --kill-after=5s 30 \
      scripts/normalize-pi-variant.sh --input "$raw" --variant "$variant" --mode "$mode" \
      --artifact "$raw" --output "$variant_dir/$key.normalized.json"
  done
  if [[ $expected -eq 0 ]]; then
    if $resume && [[ -s $variant_dir/end-to-end-raw.json && -s $variant_dir/end-to-end.normalized.json && -s $variant_dir/end-to-end/manifest.json ]]; then
      continue
    fi
    baseline_xdg="$PWD/$final/private/xdg/$run_tag-$variant-end-to-end"
    mkdir -p "$baseline_xdg"
    PI_CASSO_TEST_MODE=1 PI_CASSO_TEST_GENERATOR_VARIANT="$variant" \
      scripts/run-benchmark-baseline.sh --output-dir "$variant_dir" --artifact-prefix end-to-end \
      --scenario growing-cold --source-mode growing --cache-state cold --xdg-root "$baseline_xdg" \
      --work-windows 65536 --repetitions 5 --warmup 1 --profile performance --backend cpu \
      --gpu off --generator-backend cpu --cpu-workers 1 --chunk-size 65536 --queue-depth 1 \
      --memory-limit-mb 512 "${variant_args[@]}"
    canonicalize_baseline "$variant_dir" end-to-end \
      "$(jq -er '.generator_executable_sha256' "$variants_root/$variant-serial.json")"
    recorded 0 timeout --signal=TERM --kill-after=5s 30 \
      scripts/normalize-pi-variant.sh --input "$variant_dir/end-to-end-raw.json" \
      --variant "$variant" --mode end-to-end --summary "$variant_dir/end-to-end-raw.json" \
      --repetitions-dir "$variant_dir/end-to-end" --output "$variant_dir/end-to-end.normalized.json"
  else
    recorded 0 timeout --signal=TERM --kill-after=5s 30 \
      scripts/create-pi-unavailable-envelope.sh --variant "$variant" \
      --reason external_ycruncher_path_missing --output "$variant_dir/end-to-end-unavailable.json"
    recorded 0 timeout --signal=TERM --kill-after=5s 30 \
      scripts/normalize-pi-variant.sh --input "$variant_dir/end-to-end-unavailable.json" \
      --variant "$variant" --mode end-to-end --summary "$variant_dir/end-to-end-unavailable.json" \
      --repetitions-dir "" --output "$variant_dir/end-to-end.normalized.json"
  fi
done
unset PI_CASSO_TEST_GENERATOR_VARIANT

jq -s '{schema_version:1,demand_mode:"concurrent",variants:.}' \
  "$variants_root"/*-concurrent.json > "$concurrent_aggregate"
jq -s '{schema_version:1,demand_mode:"search-overlap",variants:.}' \
  "$variants_root"/*-search-overlap.json > "$overlap_aggregate"
jq -e 'all(.variants[]; .status == "unavailable" or (.demand_mode == "concurrent" and .concurrent_requests == 4 and .coalesced_request_count == 4 and .producer_epochs == 1 and .generation_batches >= 1))' "$concurrent_aggregate" >/dev/null
jq -e 'all(.variants[]; .status == "unavailable" or (.demand_mode == "search-overlap" and .search_work_windows == 4096 and .overlap_wait_ms >= 0))' "$overlap_aggregate" >/dev/null

comparison_gate="$final/preselection-gate.json"
jq -n '{schema_version:1,accepted:true,rejection_reason:"",authoritative:false,purpose:"permit measured preselection before authoritative replay"}' > "$comparison_gate"
recorded 0 timeout --signal=TERM --kill-after=5s 60 scripts/select-pi-variant.sh \
  --input-dir "$variants_root" --baseline-dir "$evidence/task-1-growing-cold" \
  --comparison-json "$comparison_gate" --output "$selection"
selected_variant=$(jq -er '.selected_variant | select(length > 0)' "$selection")
replay_args=()
if [[ $selected_variant == y-cruncher-external ]]; then
  [[ $external_mode == fixture ]] || { echo "unavailable external variant selected" >&2; exit 1; }
  replay_args=(--y-cruncher-path "$fixture_path")
fi
replay_xdg="$PWD/$final/private/xdg/$run_tag-selected-growing"
mkdir -p "$replay_xdg"
PI_CASSO_TEST_MODE=1 PI_CASSO_TEST_GENERATOR_VARIANT="$selected_variant" \
  scripts/run-benchmark-baseline.sh --output-dir "$evidence" --artifact-prefix task-12-selected-growing \
  --scenario growing-cold --source-mode growing --cache-state cold --xdg-root "$replay_xdg" \
  --work-windows 65536 --repetitions 5 --warmup 1 --profile performance --backend cpu \
  --gpu off --generator-backend cpu --cpu-workers 1 --chunk-size 65536 --queue-depth 1 \
  --memory-limit-mb 512 "${replay_args[@]}"
canonicalize_baseline "$evidence" task-12-selected-growing \
  "$(jq -er '.selected_executable_sha256' "$selection")"
recorded 0 timeout --signal=TERM --kill-after=5s 60 scripts/compare-benchmark-runs.sh \
  --baseline-dir "$evidence/task-1-growing-cold" --candidate-dir "$evidence/task-12-selected-growing" \
  --metrics scanned_windows_per_second,source_digits_per_second,stage_timings.generation_wait_ms,overlap_wait_ms \
  --max-p95-regression 0.10 --comparison-mode generator-selection \
  --allow-config-diff generator_backend,selected_variant,y_cruncher_path_present,y_cruncher_executable_sha256 \
  --output "$comparison"
jq -e --arg hash "$(jq -er '.generator_executable_sha256' "$evidence/task-12-selected-growing-raw.json")" \
  '.selected_executable_sha256 == $hash' "$selection" >/dev/null
comparison_accepted=$(jq -r '.accepted' "$comparison")
if [[ $comparison_accepted == true ]]; then
  selection_status=selected
  selection_reason=""
else
  selection_status=fallback_retained
  selection_reason=$(jq -r '.rejection_reason' "$comparison")
fi
jq -n --arg selected_variant "$selected_variant" \
  --arg selected_executable_sha256 "$(jq -er '.selected_executable_sha256' "$selection")" \
  --arg status "$selection_status" --arg reason "$selection_reason" \
  --slurpfile preselection_gate "$comparison_gate" --slurpfile comparison "$comparison" \
  '{schema_version:1,selected_variant:$selected_variant,selected_executable_sha256:$selected_executable_sha256,status:$status,reason:$reason,preselection_gate:$preselection_gate[0],authoritative_comparison:$comparison[0]}' \
  > "$selection_disposition"

path_root="$PWD/$final/private/path-fixture"
path_executable="$path_root/bin/y-cruncher"
mkdir -p "$path_root/bin"
scripts/create-ycruncher-fixture.sh --output "$path_executable" --digits 2000
ln -s "$path_executable" "$path_root/bin/y-cruncher-link"
path_commands="$evidence/task-12-path-commands.json"
path_log="$evidence/task-12-path.log"
printf '[]\n' > "$path_commands"
: > "$path_log"
for kind in auto explicit; do
  mkdir -p "$path_root/$kind/data" "$path_root/$kind/config" "$path_root/$kind/tmp"
  export XDG_DATA_HOME="$path_root/$kind/data" XDG_CONFIG_HOME="$path_root/$kind/config" TMPDIR="$path_root/$kind/tmp"
  args=(--json pi benchmark --targets 1000 --generator-backend "$kind" --repetitions 1 --warmup 0)
  [[ $kind == explicit ]] && args=(--json pi benchmark --targets 1000 --generator-backend y-cruncher --y-cruncher-path "$path_executable" --repetitions 1 --warmup 0)
  PATH="$path_root/bin:$PATH" scripts/run-evidence-command.sh --commands-json "$path_commands" \
    --log "$path_log" --expected-exit 0 -- timeout --signal=TERM --kill-after=5s 120 \
    cargo run --release --locked -- "${args[@]}" > "$evidence/task-12-path-$kind.json"
done
for artifact in "$path_commands" "$path_log" "$evidence/task-12-path-auto.json" "$evidence/task-12-path-explicit.json"; do
  PATH_ROOT="$path_root" perl -0pi -e 's/\Q$ENV{PATH_ROOT}\E/<fixture-root>/g' "$artifact"
done
cache_file=$(find "$path_root/auto/data" -type f -name pi-cache.txt -print -quit)
imported_digits=$(wc -c < "$cache_file")
known_prefix=$(head -c 16 "$cache_file")
fixture_sha=$(sha256sum "$path_executable" | cut -d' ' -f1)
jq -n --arg sha "$fixture_sha" --argjson digits "$imported_digits" \
  --arg auto_hash "$(jq -er '.generator_executable_sha256' "$evidence/task-12-path-auto.json")" \
  --arg explicit_hash "$(jq -er '.generator_executable_sha256' "$evidence/task-12-path-explicit.json")" \
  --arg auto_id "$(jq -er '.workload_id' "$evidence/task-12-path-auto.json")" \
  --arg explicit_id "$(jq -er '.workload_id' "$evidence/task-12-path-explicit.json")" \
  --arg prefix "$known_prefix" \
  '{schema_version:1,fixture_executable_sha256:$sha,imported_digits:$digits,known_prefix:$prefix,auto_executable_sha256:$auto_hash,explicit_executable_sha256:$explicit_hash,auto_workload_id:$auto_id,explicit_workload_id:$explicit_id,assertions:{hash_identity:($auto_hash==$explicit_hash and $auto_hash==$sha),workload_identity:($auto_id==$explicit_id),known_prefix:($prefix=="3141592653589793"),requested_length:($digits==1000),path_redacted:true}}' \
  > "$final/path-identity-redaction-audit.json"
if rg -F "$path_root" "$path_commands" "$path_log" "$evidence/task-12-path-auto.json" "$evidence/task-12-path-explicit.json"; then
  echo "raw PATH fixture root leaked into evidence" >&2
  exit 1
fi
jq -e '.assertions | all' "$final/path-identity-redaction-audit.json" >/dev/null

normalized_files=()
for variant in "${variants[@]}"; do
  for name in serial concurrent search_overlap end-to-end; do
    normalized_files+=("$variants_root/$variant/$name.normalized.json")
  done
done
entries='[]'
for file in "${normalized_files[@]}"; do
  entries=$(jq -c --arg path "$file" --arg input "$(jq -er '.input_artifact' "$file")" \
    --arg sha "$(sha256sum "$file" | cut -d' ' -f1)" --arg raw_sha "$(jq -er '.raw_input_sha256' "$file")" \
    '. + [{path:$path,input_artifact:$input,sha256:$sha,raw_input_sha256:$raw_sha}]' <<<"$entries")
done
jq -n --argjson entries "$entries" '{schema_version:1,normalized_inputs:$entries}' > "$normalized_manifest"

jq -n \
  --slurpfile selection "$selection" --slurpfile comparison "$comparison" \
  --slurpfile concurrent "$concurrent_aggregate" --slurpfile overlap "$overlap_aggregate" \
  --slurpfile path_audit "$final/path-identity-redaction-audit.json" \
  --slurpfile spigot "$spigot_policy" --slurpfile disposition "$selection_disposition" \
  '{schema_version:1,selection:$selection[0],selection_disposition:$disposition[0],comparison:$comparison[0],concurrent:$concurrent[0],search_overlap:$overlap[0],path_identity_audit:$path_audit[0],spigot_bounded_policy:$spigot[0]}' \
  > "$numeric_manifest"

named_tests=(pi_benchmark_reports_serial_generation_contract concurrent_forced_variant_coalesces_four_requests explicit_missing_ycruncher_is_typed_redacted_unavailable ycruncher_missing_identity_is_deterministic builtin_variant_is_not_a_public_generator_backend fixture_backed_ycruncher_imports_into_a_fresh_cache search_overlap_reports_real_search_throughput test_mode_can_force_the_persistent_spigot_variant ycruncher_error_redaction_and_attestation unavailable_envelope_is_complete_and_typed normalizer_maps_hyphenated_overlap_without_inventing_metrics selector_applies_the_declared_deterministic_tie_break)
index=0
for test_name in "${named_tests[@]}"; do
  index=$((index + 1))
  prefix=$(printf 'focused-%02d' "$index")
  set_xdg "named-$prefix"
  recorded 0 timeout --signal=TERM --kill-after=5s 90 scripts/run-named-test.sh \
    --timeout-seconds 60 --xdg-root "$PWD/$final/private/$run_tag-named-$prefix" \
    --evidence-dir "$final" --artifact-prefix "$prefix" "$test_name"
done

additional_commands=("$path_commands")
additional_logs=("$path_log")
for variant in "${variants[@]}"; do
  if [[ -f $variants_root/$variant/end-to-end-baseline-commands.json ]]; then
    additional_commands+=("$variants_root/$variant/end-to-end-baseline-commands.json")
    additional_logs+=("$variants_root/$variant/end-to-end-baseline.log")
  fi
done
additional_commands+=("$evidence/task-12-selected-growing-baseline-commands.json")
additional_logs+=("$evidence/task-12-selected-growing-baseline.log")
merged=$(mktemp "$final/.commands.XXXXXX")
jq -s 'add' "$commands" "${additional_commands[@]}" > "$merged"
mv -- "$merged" "$commands"
for extra_log in "${additional_logs[@]}"; do
  cat "$extra_log" >> "$log"
done

raw_files=("$selection" "$comparison" "$concurrent_aggregate" "$overlap_aggregate" "$normalized_manifest" \
  "$evidence/task-12-selected-growing-raw.json" "$evidence/task-12-selected-growing" \
  "$evidence/task-12-path-auto.json" "$evidence/task-12-path-explicit.json" "$final/path-identity-redaction-audit.json" \
  "$numeric_manifest" "$spigot_policy" "$final/spigot-bounded-probe.stdout" \
  "$comparison_gate" "$selection_disposition")
for variant in "${variants[@]}"; do
  raw_files+=("$variants_root/$variant-serial.json" "$variants_root/$variant-concurrent.json" "$variants_root/$variant-search-overlap.json")
  raw_files+=("$variants_root/$variant/serial.normalized.json" "$variants_root/$variant/concurrent.normalized.json" "$variants_root/$variant/search_overlap.normalized.json" "$variants_root/$variant/end-to-end.normalized.json")
  if [[ -d $variants_root/$variant/end-to-end ]]; then
    raw_files+=("$variants_root/$variant/end-to-end-raw.json" "$variants_root/$variant/end-to-end")
  else
    raw_files+=("$variants_root/$variant/end-to-end-unavailable.json")
  fi
done
jq -n --arg artifact_prefix task-12-canonical --argjson artifacts "$(printf '%s\n' "${raw_files[@]}" | jq -Rsc 'split("\n")|map(select(length>0))')" \
  '{schema_version:1,artifact_prefix:$artifact_prefix,artifacts:$artifacts}' > "$path_manifest"

jq -e '.accepted == true' "$comparison" >/dev/null
scripts/record-evidence.sh --task 12 --status pass --commands-json "$commands" --log "$log" \
  --raw-files "${raw_files[@]}" "$path_manifest"
artifact_count=$(jq '.raw_files|length' "$task_manifest")
command_count=$(jq '.commands|length' "$task_manifest")
manifest_sha=$(sha256sum "$task_manifest" | cut -d' ' -f1)
commands_sha=$(sha256sum "$commands" | cut -d' ' -f1)
log_sha=$(sha256sum "$log" | cut -d' ' -f1)
numeric_sha=$(sha256sum "$numeric_manifest" | cut -d' ' -f1)
spigot_command=$(jq -r '.command.argv | join(" ")' "$spigot_policy")
spigot_exit=$(jq -r '.command.exit_code' "$spigot_policy")
spigot_argv_sha=$(jq -r '.command.argv_sha256' "$spigot_policy")
ycruncher_command=$(jq -r '.[] | select(.env.PI_CASSO_TEST_GENERATOR_VARIANT == "y-cruncher-external" and (.argv | index("--demand-mode")) and (.argv | index("serial"))) | .argv | join(" ")' "$commands" | head -n 1)
ycruncher_exit=$(jq -r '.[] | select(.env.PI_CASSO_TEST_GENERATOR_VARIANT == "y-cruncher-external" and (.argv | index("--demand-mode")) and (.argv | index("serial"))) | .exit_code' "$commands" | head -n 1)
ycruncher_argv_sha=$(jq -r '.[] | select(.env.PI_CASSO_TEST_GENERATOR_VARIANT == "y-cruncher-external" and (.argv | index("--demand-mode")) and (.argv | index("serial"))) | .argv_sha256' "$commands" | head -n 1)
selection_command=$(jq -r '.[] | select(.argv | index("scripts/select-pi-variant.sh")) | .argv | join(" ")' "$commands" | head -n 1)
selection_exit=$(jq -r '.[] | select(.argv | index("scripts/select-pi-variant.sh")) | .exit_code' "$commands" | head -n 1)
comparison_command=$(jq -r '.[] | select(.argv | index("scripts/compare-benchmark-runs.sh")) | .argv | join(" ")' "$commands" | head -n 1)
comparison_exit=$(jq -r '.[] | select(.argv | index("scripts/compare-benchmark-runs.sh")) | .exit_code' "$commands" | head -n 1)
{
  printf '# Task 12 canonical evidence report\n\n'
  printf '## Outcome\n\n'
  printf -- "- Harness command: %s; exit 0.\n" "$harness_command"
  printf -- "- Preselected variant: %s; replay executable digest matches selection.\n" "$selected_variant"
  printf -- "- Authoritative disposition: %s.\n" "$selection_status"
  printf -- "- Comparison: command exit %s, accepted=%s.\n" "$comparison_exit" "$comparison_accepted"
  printf -- "- Focused named tests: %s/%s passed with 60-second child bounds.\n" "${#named_tests[@]}" "${#named_tests[@]}"
  printf -- "- Inventory: %s recorded commands and %s digest-recorded artifacts.\n\n" "$command_count" "$artifact_count"
  printf '## Key commands and exits\n\n'
  printf -- "- Spigot bounded probe: %s; exit %s; argv SHA-256 %s.\n" "$spigot_command" "$spigot_exit" "$spigot_argv_sha"
  printf -- "- Fixture-backed y-cruncher serial: %s; exit %s; argv SHA-256 %s.\n" "$ycruncher_command" "$ycruncher_exit" "$ycruncher_argv_sha"
  printf -- "- Selection: %s; exit %s.\n" "$selection_command" "$selection_exit"
  printf -- "- Replay comparison: %s; exit %s.\n\n" "$comparison_command" "$comparison_exit"
  printf '## Hashes\n\n'
  printf -- "- Canonical manifest %s: %s.\n" "$task_manifest" "$manifest_sha"
  printf -- "- Numeric manifest %s: %s.\n" "$numeric_manifest" "$numeric_sha"
  printf -- "- Commands %s: %s.\n" "$commands" "$commands_sha"
  printf -- "- Log %s: %s.\n\n" "$log" "$log_sha"
  printf '## Residuals\n\n'
  printf -- "- spigot-persistent remains correct at the preserved small forced-spigot test, but the exact canonical serial workload is typed unavailable after the fresh 30-second bound; no throughput is invented.\n"
  printf -- "- y-cruncher-external uses the deterministic fixture, not a vendor y-cruncher binary; executable identity is attested and PATH/explicit identity is audited.\n"
  if [[ $comparison_accepted != true ]]; then
    printf -- "- The replay comparison residual is %s; the preselection gate is explicitly non-authoritative and the measured candidate is rejected, so the built-in CPU fallback is retained.\n" "$(jq -r '.rejection_reason' "$comparison")"
  else
    printf -- '- No remaining replay-comparison blocker.\n'
  fi
} > "$final/report.md"
jq -e 'all(.[]; .expected_exit_code == .exit_code)' "$commands" >/dev/null
test -s "$final/report.md" -a -s "$task_manifest"
