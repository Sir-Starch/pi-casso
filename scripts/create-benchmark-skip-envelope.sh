#!/usr/bin/env bash
set -euo pipefail

requested_backend=""
reason=""
output=""
while (($#)); do
  case "$1" in
    --requested-backend) requested_backend=${2:?}; shift 2 ;;
    --reason) reason=${2:?}; shift 2 ;;
    --output) output=${2:?}; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
test -n "$requested_backend" && test -n "$reason" && test -n "$output" || exit 2
mkdir -p "$(dirname "$output")"
temp_output=$(mktemp "$(dirname "$output")/.skip.XXXXXX")
jq -n --arg backend "$requested_backend" --arg reason "$reason" '{
  schema_version:1,status:"skip",reason:"",skip_reason:$reason,
  requested_backend:$backend,resolved_backend:null,backend_fault_status:"none",fallback:false,fallback_reason:"",
  workload_id:"",workload_identity:{},source_mode:"",cache_state:"",cache_reset:false,warm_up_completed:false,page_cache_control:"uncontrolled",
  start_offset:0,effective_end:0,source_end_exclusive:0,window_len:0,scanned_windows:0,
  scanned_windows_per_second:0,source_digits_per_second:0,logical_window_digits_per_second:0,elapsed_seconds:0,stop_reason:"",best_score:0,
  repetitions:0,warmup:0,median:{scanned_windows_per_second:0,source_digits_per_second:0,logical_window_digits_per_second:0,elapsed_seconds:0,overlap_wait_ms:0,cache_write_ms:0,generation_wait_ms:0},p95:{scanned_windows_per_second:0,source_digits_per_second:0,logical_window_digits_per_second:0,elapsed_seconds:0,overlap_wait_ms:0,cache_write_ms:0,generation_wait_ms:0},
  stage_timings:{read_ms:0,parse_ms:0,queue_wait_ms:0,backend_compute_ms:0,gpu_allocation_ms:0,gpu_upload_ms:0,gpu_dispatch_ms:0,gpu_readback_map_ms:0,reduction_ms:0,persistence_ms:0,generation_wait_ms:0,throttle_wait_ms:0},
  waits:{source_ms:0,queue_ms:0,generator_ms:0,throttle_ms:0},overlap_wait_ms:0,cache_write_ms:0,producer_epochs:0,
  config:{profile:"",cpu_workers:0,cpu_utilization:0,gpu_utilization:0,chunk_size:0,queue_depth:0,memory_limit_mb:0},
  memory:{logical_peak_mb:0,rss_peak_mb:0,rss_baseline_mb:0,rss_margin_mb:0,gpu_vram_status:"unavailable",gpu_vram_baseline_mb:0,gpu_vram_margin_mb:0,gpu_vram_peak_mb:0},
  source:{reader_pool_size:0,reader_open_count:0,reader_reuse_count:0,cache_hit_ms:0},queue:{max_occupancy:0,permits:0,global_limit:0},
  gpu:{resource_reuses:0,overlap_ms:0,submissions:0,completions:0,fallback_count:0,max_in_flight:0,overlap_events:0,capability:{schema_version:1,capability_state:"unavailable",available:false,backend:$backend,device:"",driver:"",feature:"",reason:$reason,cuda_available:false,cuda_driver_version:"",cuda_device_compute_capability:"",cuda_ptx_compatible:false,kernel_sha256:"",kernel_source_sha256:"",kernel_load_status:"not_attempted"}},
  gpu_duty_policy_percent:0,gpu_duty_window_ms:0,gpu_duty_wait_ms:0,gpu_initial_submission_wait_ms:0,active_submission_ratio:0,dispatch_quantum_ratio:0,
  raw_run_paths:[],raw_runs:[],git_sha:"",machine:{os:"",cpu:"",gpu:"",driver:"",rustc:"",power_policy:"unavailable",thermal_policy:"unavailable"}
}' > "$temp_output"
mv -f -- "$temp_output" "$output"
