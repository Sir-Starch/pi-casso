# CUDA emergence kernel handoff

source_sha256=6dd9970ba98c26ae0a6798944ba3fb65f9c71a862a8bf6edf70a52677b847f80
artifact_sha256=not_available
toolchain=nvcc not_available
architecture=compute_89

The repository is currently in the portable `artifact_handoff_missing` state. A designated CUDA toolkit host must generate and validate a PTX handoff for its supported compute capability before adding `handoff.json` and `emergence.ptx`. Their absence is intentional and must not be replaced by an unverified artifact. The `compute_89` values above are only the current handoff example, not a device requirement.

Exact generation command:

```sh
nvcc -std=c++17 -O3 --ptx --gpu-architecture=compute_89 kernels/cuda/emergence.cu -o kernels/cuda/emergence.ptx
```

Validation procedure:

```sh
scripts/verify-cuda-handoff.sh --manifest kernels/cuda/handoff.json --readme kernels/cuda/README.md --source kernels/cuda/emergence.cu --artifact kernels/cuda/emergence.ptx
PI_CASSO_TEST_MODE=1 cargo run --release --locked --no-default-features --features cuda-native -- --json gpu info
```

The runtime loads only the checked-in PTX through the CUDA Driver API. It never invokes `nvcc` or NVRTC.
