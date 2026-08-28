#!/usr/bin/env bash
set -euo pipefail

mode=""
output=""
architecture='compute_89'
while (($#)); do
  case "$1" in
    --mode) mode=${2:?}; shift 2 ;;
    --output) output=${2:?}; shift 2 ;;
    --architecture) architecture=${2:?}; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
case "$mode" in valid|missing|corrupt) ;; *) echo "mode must be valid, missing, or corrupt" >&2; exit 2;; esac
[[ $architecture =~ ^compute_[0-9]+$ ]] || { echo "architecture must look like compute_N" >&2; exit 2; }
[[ -n $output && ! -e $output ]] || { echo "fixture output must not already exist" >&2; exit 2; }

cuda_dir="$output/kernels/cuda"
artifact_path="$cuda_dir/emergence.ptx"
mkdir -p "$cuda_dir"
cp kernels/cuda/emergence.cu "$cuda_dir/emergence.cu"
source_sha=$(sha256sum "$cuda_dir/emergence.cu" | cut -d' ' -f1)
if [[ $mode == missing ]]; then
  {
    printf 'source_sha256=%s\n' "$source_sha"
    printf 'artifact_sha256=missing\n'
    printf 'toolchain=nvcc fixture\n'
    printf 'architecture=%s\n' "$architecture"
  } > "$cuda_dir/README.md"
  exit 0
fi

sed "s/^\\.target sm_[0-9][0-9]*/.target sm_${architecture#compute_}/" \
  tests/fixtures/cuda/emergence.compute_89.ptx > "$artifact_path"
artifact_sha=$(sha256sum "$artifact_path" | cut -d' ' -f1)
toolchain='nvcc fixture-12.0'
command="nvcc -std=c++17 -O3 --ptx --gpu-architecture=$architecture kernels/cuda/emergence.cu -o kernels/cuda/emergence.ptx"
{
  printf 'source_sha256=%s\n' "$source_sha"
  printf 'artifact_sha256=%s\n' "$artifact_sha"
  printf 'toolchain=%s\n' "$toolchain"
  printf 'architecture=%s\n' "$architecture"
} > "$cuda_dir/README.md"
jq -n \
  --arg source_sha "$source_sha" \
  --arg artifact_sha "$artifact_sha" \
  --arg toolchain "$toolchain" \
  --arg architecture "$architecture" \
  --arg command "$command" \
  '{
    schema_version:1,
    owner:"performance-maintainer-fixture",
    source_path:"kernels/cuda/emergence.cu",
    artifact_path:"kernels/cuda/emergence.ptx",
    architecture:$architecture,
    toolchain:$toolchain,
    nvcc_command:$command,
    source_sha256:$source_sha,
    artifact_sha256:$artifact_sha,
    designated_host:{gpu:"fixture-sm89",driver:"fixture-driver",toolkit:$toolchain}
  }' > "$cuda_dir/handoff.json"
if [[ $mode == corrupt ]]; then
  printf '\n.corrupt\n' >> "$artifact_path"
fi
