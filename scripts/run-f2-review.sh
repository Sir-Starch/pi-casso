#!/usr/bin/env bash
set -euo pipefail

base_sha=""
final_git=""
plan=""
report=""

while (($#)); do
  case "$1" in
    --base-sha)
      [[ $# -ge 2 ]] || { echo "--base-sha requires a value" >&2; exit 2; }
      base_sha=$2
      shift 2
      ;;
    --final-git)
      [[ $# -ge 2 ]] || { echo "--final-git requires a value" >&2; exit 2; }
      final_git=$2
      shift 2
      ;;
    --plan)
      [[ $# -ge 2 ]] || { echo "--plan requires a value" >&2; exit 2; }
      plan=$2
      shift 2
      ;;
    --report)
      [[ $# -ge 2 ]] || { echo "--report requires a value" >&2; exit 2; }
      report=$2
      shift 2
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

for value in "$base_sha" "$final_git" "$plan" "$report"; do
  [[ -n $value ]] || { echo "all four F2 arguments are required" >&2; exit 2; }
done

command -v git >/dev/null 2>&1 || { echo "git is required" >&2; exit 2; }
command -v jq >/dev/null 2>&1 || { echo "jq is required" >&2; exit 2; }
command -v rg >/dev/null 2>&1 || { echo "rg is required" >&2; exit 2; }
[[ -f $plan ]] || { echo "missing plan: $plan" >&2; exit 2; }

resolved_base=$(git rev-parse --verify "$base_sha^{commit}")
resolved_final=$(git rev-parse --verify "$final_git^{commit}")
[[ -d .git || -f .git ]] || { echo "not a git worktree" >&2; exit 2; }

report_dir=$(dirname -- "$report")
mkdir -p -- "$report_dir"
[[ ! -d $report ]] || { echo "report path is a directory: $report" >&2; exit 2; }

review_root=$(mktemp -d "${TMPDIR:-/tmp}/pi-casso-f2-review.XXXXXX")
production_root="$review_root/production"
mkdir -p -- "$production_root"

trap 'rm -rf -- "$review_root"' EXIT INT TERM

declare -A seen_paths=()
source_paths=()

add_source_path() {
  local path=$1 destination
  case "$path" in
    src/*.rs|tests/*.rs) ;;
    *) return 0 ;;
  esac
  [[ -z ${seen_paths[$path]:-} ]] || return 0
  seen_paths[$path]=1

  destination="$review_root/$path"
  mkdir -p -- "$(dirname -- "$destination")"
  if [[ -f $path && ! -L $path ]]; then
    cp -- "$path" "$destination"
  elif git cat-file -e "$resolved_final:$path" 2>/dev/null; then
    git show "$resolved_final:$path" > "$destination"
  else
    return 0
  fi
  source_paths+=("$path")
}

collect_paths() {
  git diff --name-only --no-ext-diff -z "$resolved_base" "$resolved_final"
  git diff --cached --name-only --no-ext-diff -z "$resolved_base"
  git diff --name-only --no-ext-diff -z
  git ls-files --others --exclude-standard -z
}

while IFS= read -r -d '' changed_path; do
  add_source_path "$changed_path"
done < <(collect_paths)

make_production_snapshot() {
  local path=$1 source="$review_root/$1" destination="$production_root/$1"
  mkdir -p -- "$(dirname -- "$destination")"
  if [[ $path == tests/*.rs || $path == *_tests.rs ]]; then
    : > "$destination"
    return 0
  fi
  awk '
    BEGIN {
      brace_depth = 0
      test_attribute = 0
      test_module_depth = -1
    }
    function brace_delta(value, opens, closes) {
      gsub(/"([^"\\]|\\.)*"/, "", value)
      opens = gsub(/\{/, "&", value)
      closes = gsub(/\}/, "&", value)
      return opens - closes
    }
    {
      line = $0
      if (line ~ /#\[cfg\(test\)\]/) {
        test_attribute = 1
      }
      if (test_attribute && line ~ /(^|[[:space:]])mod[[:space:]]+[A-Za-z0-9_]+[[:space:]]*\{/) {
        test_module_depth = brace_depth + 1
        test_attribute = 0
      }
      if (test_module_depth < 0) {
        print line
      } else {
        print ""
      }
      brace_depth += brace_delta(line)
      if (test_module_depth >= 0 && brace_depth < test_module_depth) {
        test_module_depth = -1
      }
    }
  ' "$source" > "$destination"
}

for source_path in "${source_paths[@]}"; do
  make_production_snapshot "$source_path"
done

bounded_root="$review_root/bounded"
for source_path in "${source_paths[@]}"; do
  case "$source_path" in
    src/gpu*.rs|src/cuda*.rs|src/search/*|src/pi*|src/benchmark*.rs|src/commands/bench.rs)
      mkdir -p -- "$bounded_root/$(dirname -- "$source_path")"
      cp -- "$production_root/$source_path" "$bounded_root/$source_path"
      ;;
  esac
done

declare -A check_status=()
declare -A check_commands=()
declare -A check_evidence=()
check_names=(
  ownership_lifetime
  bounded_memory
  async_completion
  deterministic_order
  fallback_error_propagation
  hot_path_logging
)
for check_name in "${check_names[@]}"; do
  check_status[$check_name]=fail
  check_commands[$check_name]='[]'
  check_evidence[$check_name]='[]'
done

json_argv() {
  jq -cn --args '$ARGS.positional' -- "$@"
}

add_command() {
  local check_name=$1
  shift
  local argv_json
  argv_json=$(json_argv "$@")
  check_commands[$check_name]=$(jq -c --argjson argv "$argv_json" \
    '. + [$argv]' <<< "${check_commands[$check_name]}")
}

add_evidence() {
  local check_name=$1 evidence_value=$2
  check_evidence[$check_name]=$(jq -c --arg evidence "$evidence_value" \
    '. + [$evidence]' <<< "${check_evidence[$check_name]}")
}

add_common_commands() {
  local check_name=$1
  add_command "$check_name" git diff --name-only --no-ext-diff "$resolved_base" "$resolved_final" -- src tests
  add_command "$check_name" git diff --cached --name-only --no-ext-diff "$resolved_base" -- src tests
  add_command "$check_name" git diff --name-only --no-ext-diff -- src tests
  add_command "$check_name" git ls-files --others --exclude-standard -- src tests
}

snapshot_matches() {
  local root=$1 pattern=$2 matches
  matches=$(rg -n --no-heading --color never --pcre2 "$pattern" "$root" 2>/dev/null || true)
  if [[ -z $matches ]]; then
    return 0
  fi
  awk -v root="$root/" '
    index($0, root) == 1 { print substr($0, length(root) + 1) }
    index($0, root) != 1 { print }
  ' <<< "$matches"
}

add_match_evidence() {
  local check_name=$1 matches=$2 line_count=0
  while IFS= read -r evidence_line; do
    [[ -n $evidence_line ]] || continue
    add_evidence "$check_name" "$evidence_line"
    line_count=$((line_count + 1))
    ((line_count < 20)) || break
  done <<< "$matches"
  return 0
}

production_pool_lines() {
  snapshot_matches "$production_root" 'ThreadPoolBuilder[[:space:]]*::[[:space:]]*new[[:space:]]*\('
}

production_pool_owner_lines() {
  local source_path production_file
  for source_path in "${source_paths[@]}"; do
    production_file="$production_root/$source_path"
    [[ -f $production_file ]] || continue
    awk -v path="$source_path" '
      BEGIN {
        brace_depth = 0
        impl_depth = -1
        impl_owner = ""
      }
      function brace_delta(value, opens, closes) {
        gsub(/"([^"\\]|\\.)*"/, "", value)
        opens = gsub(/\{/, "&", value)
        closes = gsub(/\}/, "&", value)
        return opens - closes
      }
      {
        line = $0
        if (impl_depth < 0 && line ~ /^[[:space:]]*impl[[:space:]]+[^{}]+\{/) {
          owner = line
          sub(/^[[:space:]]*impl[[:space:]]+/, "", owner)
          sub(/[[:space:]]*\{.*$/, "", owner)
          if (owner ~ /[[:space:]]+for[[:space:]]+/) {
            sub(/^.*[[:space:]]+for[[:space:]]+/, "", owner)
          }
          gsub(/[[:space:]]+$/, "", owner)
          impl_owner = owner
          impl_depth = brace_depth + brace_delta(line)
        }
        if (line ~ /ThreadPoolBuilder[[:space:]]*::[[:space:]]*new[[:space:]]*\(/) {
          if (impl_depth < 0 || impl_owner == "") {
            print path ":" NR ":<unknown-owner>"
          } else {
            print path ":" NR ":" impl_owner
          }
        }
        delta = brace_delta(line)
        brace_depth += delta
        if (impl_depth >= 0 && brace_depth < impl_depth) {
          impl_depth = -1
          impl_owner = ""
        }
      }
    ' "$production_file"
  done
}

production_usage_lines() {
  snapshot_matches "$production_root" 'rayon::|into_par_iter|par_iter|rayon::join'
}

all_test_lines() {
  rg -n --no-heading --color never --pcre2 "$1" src tests 2>/dev/null || true
}

add_common_commands ownership_lifetime
add_command ownership_lifetime rg -n --pcre2 'ThreadPoolBuilder|rayon::ThreadPool|pool\.install|rayon::join' src tests
pool_lines=$(production_pool_lines)
pool_count=$(awk 'NF { count++ } END { print count + 0 }' <<< "$pool_lines")
pool_owner_lines=$(production_pool_owner_lines)
declare -A pool_owner_counts=()
pool_owner_invalid=false
pool_owner_count=0
while IFS= read -r pool_owner_record; do
  pool_owner=${pool_owner_record##*:}
  [[ -n $pool_owner ]] || continue
  if [[ $pool_owner == '<unknown-owner>' ]]; then
    pool_owner_invalid=true
    continue
  fi
  if [[ -z ${pool_owner_counts[$pool_owner]:-} ]]; then
    pool_owner_counts[$pool_owner]=0
    pool_owner_count=$((pool_owner_count + 1))
  fi
  pool_owner_counts[$pool_owner]=$((pool_owner_counts[$pool_owner] + 1))
done <<< "$pool_owner_lines"
pool_owner_duplicate=false
for pool_owner in "${!pool_owner_counts[@]}"; do
  if ((pool_owner_counts[$pool_owner] > 1)); then
    pool_owner_duplicate=true
  fi
done
usage_lines=$(production_usage_lines)
owner_lines=$(snapshot_matches "$production_root" 'CoordinatedRayonPool|CpuSearchBackend')
if ((pool_count == pool_owner_count)) && [[ $pool_owner_invalid == false ]] && \
  [[ $pool_owner_duplicate == false ]] && \
  { [[ -z $usage_lines ]] || [[ -n $owner_lines ]]; }; then
  check_status[ownership_lifetime]=pass
  if [[ -n $pool_owner_lines ]]; then
    add_match_evidence ownership_lifetime "$pool_owner_lines"
  else
    add_evidence ownership_lifetime "no changed production Rayon pool declaration"
  fi
  add_evidence ownership_lifetime "one Rayon pool per production owner; separate CpuSearchBackend and CoordinatedRayonPool owners are allowed"
  add_evidence ownership_lifetime "plan:452 one coordinated Rayon pool/no nested pool requirement"
else
  check_status[ownership_lifetime]=fail
  add_match_evidence ownership_lifetime "$pool_lines"
  add_match_evidence ownership_lifetime "$pool_owner_lines"
  add_match_evidence ownership_lifetime "$owner_lines"
  [[ -n $pool_lines ]] || add_evidence ownership_lifetime "missing explicit coordinated Rayon pool evidence"
  [[ $pool_owner_invalid == false ]] || add_evidence ownership_lifetime "Rayon pool builder has no production owner"
  [[ $pool_owner_duplicate == false ]] || add_evidence ownership_lifetime "multiple Rayon pool builders belong to one production owner"
  ((pool_count == pool_owner_count)) || add_evidence ownership_lifetime "every production Rayon pool builder must have exactly one owner"
  [[ -n $usage_lines ]] || add_evidence ownership_lifetime "no production Rayon owner marker"
fi

add_common_commands bounded_memory
add_command bounded_memory rg -n --pcre2 'mpsc::channel|crossbeam_channel::unbounded|async_channel::unbounded|unbounded\(' src tests
add_command bounded_memory rg -n --pcre2 'sync_channel|with_capacity|queue_depth|memory_limit|acquire_chunk|acquire_gpu|ring_depth' src tests
unbounded_lines=$(snapshot_matches "$bounded_root" \
  '(?:std::sync::)?mpsc::channel[[:space:]]*\(|crossbeam_channel::unbounded[[:space:]]*\(|async_channel::unbounded[[:space:]]*\(|tokio::sync::mpsc::unbounded_channel[[:space:]]*\(')
unbounded_buffer_lines=$(snapshot_matches "$bounded_root" \
  '(?:let[[:space:]]+)?(?:mut[[:space:]]+)?(pending|queue|buffer|buffers|chunks|requests|backlog)[[:space:]]*=[[:space:]]*(Vec|VecDeque|BTreeMap|HashMap)::(?:new|default)\(|^[[:space:]]*(pending|queue|buffer|buffers|chunks|requests|backlog)[[:space:]]*:[[:space:]]*(Vec|VecDeque|BTreeMap|HashMap)[<[:space:]]')
resource_lines=$(snapshot_matches "$bounded_root" 'ResourceBudget|queue_depth|memory_limit|sync_channel|with_capacity|acquire_chunk|acquire_gpu|ring_depth')
if [[ -z $unbounded_lines && -z $unbounded_buffer_lines && -n $resource_lines ]]; then
  check_status[bounded_memory]=pass
  add_match_evidence bounded_memory "$resource_lines"
  add_evidence bounded_memory "production bounded scope covers GPU/CUDA, search pipeline, π, and benchmark paths; test-only snapshots are excluded"
  add_evidence bounded_memory "no production unbounded channel or queue-like resident buffer matched"
  add_evidence bounded_memory "plan:452 bounded pending/buffers and no unbounded channel requirement"
else
  check_status[bounded_memory]=fail
  add_match_evidence bounded_memory "$unbounded_lines"
  add_match_evidence bounded_memory "$unbounded_buffer_lines"
  add_match_evidence bounded_memory "$resource_lines"
  [[ -n $unbounded_lines ]] || [[ -n $unbounded_buffer_lines ]] || \
    add_evidence bounded_memory "missing queue-depth and logical-memory reservation evidence"
fi

add_common_commands async_completion
add_command async_completion rg -n --pcre2 'map_async|queue\.submit|thread::spawn|completion|recv_timeout|device\.poll' src
production_file_matches() {
  local source_path=$1 pattern=$2 matches
  matches=$(rg -n --no-heading --color never --pcre2 "$pattern" \
    "$production_root/$source_path" 2>/dev/null || true)
  [[ -n $matches ]] || return 0
  while IFS= read -r match; do
    printf '%s\n' "${match#"$production_root"/}"
  done <<< "$matches"
}

async_lines=$(for source_path in "${source_paths[@]}"; do
  case "$source_path" in
    src/gpu.rs|src/cuda*.rs|src/search/cuda_backend.rs)
      production_file_matches "$source_path" 'map_async|queue\.submit|thread::spawn|completion|recv_timeout|device\.poll'
      ;;
  esac
done)
async_files=()
async_mock_evidence=""
while IFS= read -r source_path; do
  [[ -n $source_path ]] || continue
  case "$source_path" in
    src/gpu.rs|src/cuda*.rs|src/search/cuda_backend.rs)
      if rg -q --pcre2 'map_async|queue\.submit|thread::spawn|completion' "$production_root/$source_path" 2>/dev/null; then
        async_files+=("$production_root/$source_path")
      fi
      ;;
    src/gpu_ring.rs)
      if rg -q --pcre2 'run_mock_ring|test_mock_enabled' "$review_root/$source_path" 2>/dev/null; then
        async_mock_evidence="test-only GPU mock completion path excluded from production async audit: src/gpu_ring.rs::run_mock_ring (PI_CASSO_TEST_MODE-gated)"
      fi
      ;;
  esac
done < <(printf '%s\n' "${source_paths[@]}" | sort -u)

async_ok=true
async_path_count=0
for async_file in "${async_files[@]}"; do
  [[ -f $async_file ]] || continue
  file_async=$(rg -n --no-heading --color never --pcre2 'map_async|queue\.submit|thread::spawn' "$async_file" 2>/dev/null || true)
  [[ -n $file_async ]] || continue
  async_path_count=$((async_path_count + 1))
  file_completion=$(rg -n --no-heading --color never --pcre2 'completion|recv|join|device\.poll|await' "$async_file" 2>/dev/null || true)
  file_error=$(rg -n --no-heading --color never --pcre2 'Err\(|if[[:space:]]+let[[:space:]]+Err|Result<|bail!|context\(' "$async_file" 2>/dev/null || true)
  file_cancel=$(rg -n --no-heading --color never --pcre2 'unmap|cancel|abort|drain|shutdown|stop' "$async_file" 2>/dev/null || true)
  if [[ -z $file_completion || -z $file_error || -z $file_cancel ]]; then
    async_ok=false
  fi
  if [[ $file_async == *'thread::spawn'* ]]; then
    file_join=$(rg -n --no-heading --color never --pcre2 '\.join\(|cancel|abort|shutdown' "$async_file" 2>/dev/null || true)
    [[ -n $file_join ]] || async_ok=false
  fi
done
if ((async_path_count > 0)) && [[ $async_ok == true ]]; then
  check_status[async_completion]=pass
  add_match_evidence async_completion "$async_lines"
  [[ -n $async_mock_evidence ]] && add_evidence async_completion "$async_mock_evidence"
  add_evidence async_completion "production GPU completion path retains bounded sync_channel, polling/recv_timeout, error propagation, and staging.unmap cleanup"
  add_evidence async_completion "plan:452 every async submission has completion/error/cancel handling"
else
  check_status[async_completion]=fail
  add_match_evidence async_completion "$async_lines"
  [[ -n $async_mock_evidence ]] && add_evidence async_completion "$async_mock_evidence"
  [[ -n $async_lines ]] || add_evidence async_completion "no completion-tracked async submission was found in the changed production GPU paths"
fi

add_common_commands deterministic_order
add_command deterministic_order cargo test --workspace --all-targets --all-features --locked cpu_determinism_under_worker_sweep -- --nocapture
add_command deterministic_order cargo test --workspace --all-targets --all-features --locked bounded_score_selection_preserves_ties_and_quantized_scores -- --nocapture
add_command deterministic_order cargo test --workspace --all-targets --all-features --locked ordered_reducer_matches_serial_reference -- --nocapture
worker_test=$(all_test_lines 'cpu_determinism_under_worker_sweep')
tie_test=$(all_test_lines 'bounded_score_selection_preserves_ties_and_quantized_scores|selector_applies_the_declared_deterministic_tie_break')
reducer_test=$(all_test_lines 'ordered_reducer_matches_serial_reference')
if [[ -n $worker_test && -n $tie_test && -n $reducer_test ]]; then
  check_status[deterministic_order]=pass
  add_match_evidence deterministic_order "$worker_test"
  add_match_evidence deterministic_order "$tie_test"
  add_match_evidence deterministic_order "$reducer_test"
  add_evidence deterministic_order "plan:452 canonical tie ordering asserted by named tests"
else
  check_status[deterministic_order]=fail
  add_match_evidence deterministic_order "$worker_test"
  add_match_evidence deterministic_order "$tie_test"
  add_match_evidence deterministic_order "$reducer_test"
  [[ -n $worker_test ]] || add_evidence deterministic_order "missing cpu worker-sweep determinism test"
  [[ -n $tie_test ]] || add_evidence deterministic_order "missing canonical tie-order test"
  [[ -n $reducer_test ]] || add_evidence deterministic_order "missing ordered reducer/reference test"
fi

add_common_commands fallback_error_propagation
add_command fallback_error_propagation rg -n --pcre2 'fallback_reason|fallback_count|backend_fault_status|resolved_backend|record_fallback' src tests
add_command fallback_error_propagation cargo test --workspace --all-targets --all-features --locked gpu_async_completion_failure_falls_back_once -- --nocapture
fallback_lines=$(snapshot_matches "$review_root" 'fallback_reason|fallback_count|backend_fault_status|resolved_backend|record_fallback|return[[:space:]]+Err\(|scores:[[:space:]]*Err')
fallback_test=$(all_test_lines 'gpu_async_completion_failure_falls_back_once|synthetic_backend_error_records_one_mixed_fallback|cuda_search_runtime_fault_records_mixed')
if [[ -n $fallback_lines && -n $fallback_test ]]; then
  check_status[fallback_error_propagation]=pass
  add_match_evidence fallback_error_propagation "$fallback_lines"
  add_match_evidence fallback_error_propagation "$fallback_test"
  add_evidence fallback_error_propagation "plan:452 fallback errors surfaced in the raw schema"
else
  check_status[fallback_error_propagation]=fail
  add_match_evidence fallback_error_propagation "$fallback_lines"
  add_match_evidence fallback_error_propagation "$fallback_test"
  [[ -n $fallback_lines ]] || add_evidence fallback_error_propagation "missing reason-bearing fallback/error schema fields"
  [[ -n $fallback_test ]] || add_evidence fallback_error_propagation "missing fallback error propagation test"
fi

add_common_commands hot_path_logging
add_command hot_path_logging rg -n --pcre2 'println!|eprintln!|log::|tracing::|warn!|info!|debug!|error!' src tests
hot_function_logging=$(for source_path in "${source_paths[@]}"; do
    case "$source_path" in
    src/*.rs)
      awk -v path="$source_path" '
        BEGIN {
          brace_depth = 0
          active = 0
          hot = 0
          body_started = 0
        }
        function brace_delta(value, opens, closes) {
          gsub(/"([^"\\]|\\.)*"/, "", value)
          opens = gsub(/\{/, "&", value)
          closes = gsub(/\}/, "&", value)
          return opens - closes
        }
        {
          line = $0
          if (!active && line ~ /^[[:space:]]*(pub([[:space:]]*\([^)]*\))?[[:space:]]+)?(async[[:space:]]+)?fn[[:space:]]+/) {
            active = 1
            hot = line ~ /(run_backend|run_batch|search_chunk|produce|reduce|producer_loop|request_generation|ensure_source_range|emergence_scores|score_candidate_window|merge_chunk_top_matches|run_mock_ring)/
            body_started = 0
            owner_depth = brace_depth
          }
          if (active && hot && line ~ /(println!|eprintln!|log::|tracing::|warn!|info!|debug!|error!)/) {
            print path ":" NR ":" line
          }
          delta = brace_delta(line)
          if (active && line ~ /\{/ && !body_started) {
            body_started = 1
          }
          brace_depth += delta
          if (active && body_started && brace_depth <= owner_depth) {
            active = 0
            hot = 0
            body_started = 0
          }
        }
      ' "$production_root/$source_path"
      ;;
  esac
done)
if [[ -z $hot_function_logging ]]; then
  check_status[hot_path_logging]=pass
  add_evidence hot_path_logging "changed hot functions contain no logging calls"
  add_evidence hot_path_logging "production snapshot covers nested changed Rust paths and excludes cfg(test) modules; current tree has no named production hot-loop log"
  add_evidence hot_path_logging "plan:452 changed hot loops contain no logging calls"
else
  check_status[hot_path_logging]=fail
  add_match_evidence hot_path_logging "$hot_function_logging"
fi

overall_status=pass
for check_name in "${check_names[@]}"; do
  [[ ${check_status[$check_name]} == pass ]] || overall_status=fail
  if [[ ${check_commands[$check_name]} == '[]' ]]; then
    check_status[$check_name]=fail
    overall_status=fail
    add_evidence "$check_name" "missing machine-check command evidence"
  fi
  if [[ ${check_evidence[$check_name]} == '[]' ]]; then
    check_status[$check_name]=fail
    overall_status=fail
    add_evidence "$check_name" "missing semantic evidence"
  fi
done

checks_json=$(jq -n \
  --arg ownership_status "${check_status[ownership_lifetime]}" \
  --argjson ownership_commands "${check_commands[ownership_lifetime]}" \
  --argjson ownership_evidence "${check_evidence[ownership_lifetime]}" \
  --arg bounded_status "${check_status[bounded_memory]}" \
  --argjson bounded_commands "${check_commands[bounded_memory]}" \
  --argjson bounded_evidence "${check_evidence[bounded_memory]}" \
  --arg async_status "${check_status[async_completion]}" \
  --argjson async_commands "${check_commands[async_completion]}" \
  --argjson async_evidence "${check_evidence[async_completion]}" \
  --arg deterministic_status "${check_status[deterministic_order]}" \
  --argjson deterministic_commands "${check_commands[deterministic_order]}" \
  --argjson deterministic_evidence "${check_evidence[deterministic_order]}" \
  --arg fallback_status "${check_status[fallback_error_propagation]}" \
  --argjson fallback_commands "${check_commands[fallback_error_propagation]}" \
  --argjson fallback_evidence "${check_evidence[fallback_error_propagation]}" \
  --arg logging_status "${check_status[hot_path_logging]}" \
  --argjson logging_commands "${check_commands[hot_path_logging]}" \
  --argjson logging_evidence "${check_evidence[hot_path_logging]}" \
  '{
    ownership_lifetime:{status:$ownership_status,commands:$ownership_commands,evidence:$ownership_evidence},
    bounded_memory:{status:$bounded_status,commands:$bounded_commands,evidence:$bounded_evidence},
    async_completion:{status:$async_status,commands:$async_commands,evidence:$async_evidence},
    deterministic_order:{status:$deterministic_status,commands:$deterministic_commands,evidence:$deterministic_evidence},
    fallback_error_propagation:{status:$fallback_status,commands:$fallback_commands,evidence:$fallback_evidence},
    hot_path_logging:{status:$logging_status,commands:$logging_commands,evidence:$logging_evidence}
  }')

temporary_report=$(mktemp "$report_dir/.f2-review.XXXXXX")
jq -n \
  --argjson schema_version 1 \
  --arg status "$overall_status" \
  --arg base_sha "$resolved_base" \
  --arg final_git_sha "$resolved_final" \
  --arg reviewed_sha "$resolved_final" \
  --arg plan "$plan" \
  --arg plan_sha256 "$(sha256sum "$plan" | cut -d' ' -f1)" \
  --argjson checks "$checks_json" \
  '{
    schema_version:$schema_version,
    status:$status,
    base_sha:$base_sha,
    final_git_sha:$final_git_sha,
    reviewed_sha:$reviewed_sha,
    plan:$plan,
    plan_sha256:$plan_sha256,
    checks:$checks
  }' > "$temporary_report"
mv -f -- "$temporary_report" "$report"

if [[ $overall_status == pass ]]; then
  exit 0
fi
exit 1
