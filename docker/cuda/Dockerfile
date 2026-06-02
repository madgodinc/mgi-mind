# mgi-mind on CUDA — for the bench harness and power users with an NVIDIA GPU.
#
# Build:
#   sudo docker build -t mgi-mind-cuda -f docker/cuda/Dockerfile .
#
# Run (interactive bench, mounts your existing data dir as /data):
#   sudo docker run --rm --gpus all \
#     -v ~/mgimind:/data \
#     -v /path/to/longmemeval_s.json:/dataset.json:ro \
#     -e MGIMIND_USE_CUDA=1 \
#     -e MGIMIND_HOME=/data \
#     mgi-mind-cuda bench /dataset.json --output /data/bench-results.json
#
# Compatible with Blackwell (sm_120 / RTX 50-series) via CUDA 12.8 + cuDNN 9.
# The host needs nvidia-container-toolkit installed (one-time `apt install`).

# ---------- Stage 1: build the binary with the `cuda` cargo feature ----------
# rustc 1.88+ required by ort 2.0.0-rc.12 (transitive: icu_normalizer, libloading,
# ort-sys all need 1.88). Pinning to a recent stable rather than `:latest` keeps
# the build reproducible.
FROM rust:1.91-slim-bookworm AS builder

# OpenSSL + pkg-config for reqwest (used by `doctor --fix` downloads), plus
# build deps the lockfile needs.
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config libssl-dev ca-certificates curl g++ \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY tests ./tests

# Build with the `cuda` cargo feature. The actual CUDA libraries (libonnxruntime
# + libcudart + libcudnn) come from the runtime image — this stage just enables
# the code path that registers the CUDA execution provider.
RUN cargo build --release --features cuda

# ---------- Stage 2: CUDA 12.8 + cuDNN 9 runtime ----------
# Blackwell needs CUDA 12.8+ for native sm_120 codegen. cuDNN 9 is required by
# the GPU build of ONNX Runtime; the `-cudnn` tag bundles it.
FROM nvidia/cuda:12.8.0-cudnn-runtime-ubuntu24.04

# Minimal runtime deps. curl is kept so `mgimind doctor --fix` can refresh
# models from inside the container if the mounted data dir is empty.
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates curl libssl3 \
    && rm -rf /var/lib/apt/lists/*

# Pre-pull the GPU build of ONNX Runtime 1.24.2 (linux-x64-gpu, bundles the
# CUDA EP shared library). The binary expects libonnxruntime.so to live next
# to it (see embedder.rs::ort_lib_path), so we drop it in /usr/local/bin/.
ARG ORT_VERSION=1.24.2
RUN curl -fsSL -o /tmp/ort.tgz \
        "https://github.com/microsoft/onnxruntime/releases/download/v${ORT_VERSION}/onnxruntime-linux-x64-gpu-${ORT_VERSION}.tgz" \
    && tar -xzf /tmp/ort.tgz -C /tmp \
    && cp /tmp/onnxruntime-linux-x64-gpu-${ORT_VERSION}/lib/libonnxruntime.so* /usr/local/lib/ \
    && cp /tmp/onnxruntime-linux-x64-gpu-${ORT_VERSION}/lib/libonnxruntime_providers_*.so /usr/local/lib/ \
    && ldconfig \
    && rm -rf /tmp/ort.tgz /tmp/onnxruntime-linux-x64-gpu-${ORT_VERSION}

COPY --from=builder /src/target/release/mgimind /usr/local/bin/mgimind

# Symlink ALL ORT shared objects next to the binary. ORT's CUDA EP loads
# `libonnxruntime_providers_shared.so` and `libonnxruntime_providers_cuda.so`
# at runtime via dlopen — and it looks for them in the directory of the main
# libonnxruntime.so, not just on LD_LIBRARY_PATH. So they must all sit
# together. /usr/local/bin/ is where mgimind looks for libonnxruntime.so
# (see embedder.rs::ort_lib_path); the rest follow it there.
RUN for f in /usr/local/lib/libonnxruntime*.so*; do \
        ln -sf "$f" "/usr/local/bin/$(basename $f)"; \
    done

# Default to CUDA on — the whole point of this image. Override with
# `-e MGIMIND_USE_CUDA=0` to force CPU inside the container for an A/B test.
ENV MGIMIND_USE_CUDA=1

# A sensible default data dir; override with `-v <host>:/data -e MGIMIND_HOME=/data`.
ENV MGIMIND_HOME=/data

ENTRYPOINT ["mgimind"]
