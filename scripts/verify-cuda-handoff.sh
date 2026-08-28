#!/usr/bin/env bash
set -euo pipefail

manifest=""
readme=""
source=""
artifact=""
while (($#)); do
  case "$1" in
    --manifest) manifest=${2:?}; shift 2 ;;
    --readme) readme=${2:?}; shift 2 ;;
    --source) source=${2:?}; shift 2 ;;
    --artifact) artifact=${2:?}; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
for path in "$manifest" "$readme" "$source" "$artifact"; do
  [[ -n $path && -f $path && ! -L $path ]] || {
    echo "missing or unsafe CUDA handoff path: $path" >&2
    exit 2
  }
done

source_sha=$(sha256sum "$source" | cut -d' ' -f1)
artifact_sha=$(sha256sum "$artifact" | cut -d' ' -f1)
readme_source=$(sed -n 's/^source_sha256=//p' "$readme")
readme_artifact=$(sed -n 's/^artifact_sha256=//p' "$readme")
readme_toolchain=$(sed -n 's/^toolchain=//p' "$readme")
readme_architecture=$(sed -n 's/^architecture=//p' "$readme")
[[ $readme_source =~ ^[0-9a-f]{64}$ && $readme_source == "$source_sha" ]]
[[ $readme_artifact =~ ^[0-9a-f]{64}$ && $readme_artifact == "$artifact_sha" ]]
[[ $readme_toolchain == nvcc\ * ]]
[[ $readme_architecture =~ ^compute_[0-9]+$ ]]
ptx_target="sm_${readme_architecture#compute_}"
grep -Fq ".target $ptx_target" "$artifact"
grep -Fq '.entry emergence' "$artifact"

jq -e \
  --arg source_sha "$source_sha" \
  --arg artifact_sha "$artifact_sha" \
  --arg toolchain "$readme_toolchain" \
  --arg architecture "$readme_architecture" \
  '.schema_version == 1
    and (.owner | type == "string" and length > 0)
    and .source_path == "kernels/cuda/emergence.cu"
    and .artifact_path == "kernels/cuda/emergence.ptx"
    and .architecture == $architecture
    and .toolchain == $toolchain
    and (.nvcc_command | type == "string" and startswith("nvcc ") and contains($architecture) and contains("emergence.ptx"))
    and .source_sha256 == $source_sha
    and .artifact_sha256 == $artifact_sha
    and (.designated_host.gpu | type == "string" and length > 0)
    and (.designated_host.driver | type == "string" and length > 0)
    and (.designated_host.toolkit | type == "string" and length > 0)' \
  "$manifest" >/dev/null
