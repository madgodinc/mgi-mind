#!/usr/bin/env bash
# scripts/local-bench-gpu.sh — reproduce the v0.14.3 GPU LongMemEval-S run
# on a local NVIDIA card without paying for a cloud pod.
#
# What it does, in order:
#   1. Downloads + extracts the ORT 1.24.2 GPU build (libonnxruntime.so +
#      libonnxruntime_providers_cuda.so) next to the cargo build dir.
#   2. Builds mgimind with --features cuda.
#   3. Downloads longmemeval_s.json from HuggingFace if it's not already
#      next to the script.
#   4. Runs `mgimind doctor --fix` with MGIMIND_MODEL_VARIANT=gpu so the
#      FP16 e5-base model lands in the cache (re-downloads if a previous
#      INT8 install is there).
#   5. Runs the full 500-question bench and writes raw.json + a printed
#      summary into ./results/<timestamp>/.
#
# Requirements (you provide):
#   - NVIDIA driver (`nvidia-smi` works)
#   - CUDA 12.x runtime + cuDNN 9.x installed system-wide (Ubuntu:
#     apt install nvidia-cuda-toolkit + libcudnn9-cuda-12)
#   - cargo / rustc
#   - ~6 GB free disk (ORT GPU tarball + extracted libs + FP16 model)
#
# Tested config: RTX 3090 (24 GB) and RTX 5060 Ti (16 GB) on Pop!_OS 24.04.
#
# Usage:
#   ./scripts/local-bench-gpu.sh                    # full run (~25 min with rerank)
#   ./scripts/local-bench-gpu.sh --no-rerank        # ~10 min, R@5 ~98%
#   ./scripts/local-bench-gpu.sh --limit 20         # smoke run, ~1 min

set -euo pipefail

# --- config ------------------------------------------------------------------
ORT_VERSION="1.24.2"
ORT_TARBALL="onnxruntime-linux-x64-gpu-${ORT_VERSION}.tgz"
ORT_URL="https://github.com/microsoft/onnxruntime/releases/download/v${ORT_VERSION}/${ORT_TARBALL}"
DATASET_URL="https://huggingface.co/datasets/xiaowu0162/LongMemEval/resolve/main/longmemeval_s.json"
DATASET_NAME="longmemeval_s.json"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK_DIR="${REPO_ROOT}/.local-bench-gpu"
TARGET_DIR="${REPO_ROOT}/target/release"
RESULTS_ROOT="${REPO_ROOT}/benchmark/results/local-gpu"

RUN_RERANK=true
BENCH_LIMIT=""
for arg in "$@"; do
    case "$arg" in
        --no-rerank) RUN_RERANK=false ;;
        --limit) shift; BENCH_LIMIT="$1" ;;
        --limit=*) BENCH_LIMIT="${arg#--limit=}" ;;
        -h|--help)
            sed -n '2,30p' "$0"
            exit 0
            ;;
        *) echo "unknown arg: $arg" >&2; exit 1 ;;
    esac
done

# --- preflight ---------------------------------------------------------------
echo "[1/5] Preflight"
if ! command -v nvidia-smi >/dev/null 2>&1; then
    echo "  nvidia-smi not found. Install the NVIDIA driver before running this script." >&2
    exit 1
fi
nvidia-smi --query-gpu=name,memory.total --format=csv,noheader | sed 's/^/  GPU: /'
if ! command -v cargo >/dev/null 2>&1; then
    echo "  cargo not found in PATH. Source \$HOME/.cargo/env or install rustup." >&2
    exit 1
fi
mkdir -p "${WORK_DIR}" "${RESULTS_ROOT}"

# --- ORT GPU runtime ---------------------------------------------------------
echo "[2/5] ORT ${ORT_VERSION} GPU runtime"
ORT_EXTRACT_DIR="${WORK_DIR}/onnxruntime-linux-x64-gpu-${ORT_VERSION}"
if [[ ! -d "${ORT_EXTRACT_DIR}" ]]; then
    if [[ ! -f "${WORK_DIR}/${ORT_TARBALL}" ]]; then
        echo "  downloading ${ORT_TARBALL} (~300 MB)..."
        curl -fsSL "${ORT_URL}" -o "${WORK_DIR}/${ORT_TARBALL}"
    fi
    echo "  extracting..."
    tar -xzf "${WORK_DIR}/${ORT_TARBALL}" -C "${WORK_DIR}"
else
    echo "  already extracted at ${ORT_EXTRACT_DIR}"
fi

ORT_LIB_DIR="${ORT_EXTRACT_DIR}/lib"
[[ -f "${ORT_LIB_DIR}/libonnxruntime_providers_cuda.so" ]] \
    || { echo "  cuda EP shared library missing from ORT tarball" >&2; exit 1; }

# Place the runtime libs alongside the mgimind binary so it loads them at start.
mkdir -p "${TARGET_DIR}"
for lib in libonnxruntime.so.${ORT_VERSION} libonnxruntime_providers_cuda.so libonnxruntime_providers_shared.so; do
    if [[ -f "${ORT_LIB_DIR}/${lib}" ]]; then
        cp -f "${ORT_LIB_DIR}/${lib}" "${TARGET_DIR}/"
    fi
done
# mgimind opens `libonnxruntime.so`; symlink the versioned file there.
ln -sf "libonnxruntime.so.${ORT_VERSION}" "${TARGET_DIR}/libonnxruntime.so"

# --- build mgimind --features cuda -------------------------------------------
echo "[3/5] Build mgimind --release --features cuda"
( cd "${REPO_ROOT}" && cargo build --release --features cuda )

MGIMIND_BIN="${TARGET_DIR}/mgimind"
[[ -x "${MGIMIND_BIN}" ]] || { echo "build did not produce ${MGIMIND_BIN}" >&2; exit 1; }

# --- dataset -----------------------------------------------------------------
echo "[4/5] LongMemEval-S dataset"
DATASET_PATH="${WORK_DIR}/${DATASET_NAME}"
if [[ ! -f "${DATASET_PATH}" ]]; then
    echo "  downloading ${DATASET_NAME} from HuggingFace (~50 MB)..."
    curl -fsSL "${DATASET_URL}" -o "${DATASET_PATH}"
else
    echo "  already cached at ${DATASET_PATH}"
fi

# --- doctor with the right variant, then bench -------------------------------
echo "[5/5] doctor --fix (GPU variant) + bench"
export MGIMIND_MODEL_VARIANT=gpu
export MGIMIND_USE_CUDA=1
# The CUDA EP libs sit next to the binary; ORT discovers them from the same
# directory. The cuDNN/cuBLAS libs come from the system loader path.
export LD_LIBRARY_PATH="${TARGET_DIR}:${LD_LIBRARY_PATH:-}"

# Make sure rerank flag matches the requested mode. There is no
# `mgimind config set` subcommand today; the config file is a plain JSON,
# so we edit it directly. Default after `init` is rerank_enabled=true,
# matching the headline run.
"${MGIMIND_BIN}" init >/dev/null 2>&1 || true
CONFIG_PATH="${MGIMIND_HOME:-$HOME/mgimind}/config.json"
if [[ -f "${CONFIG_PATH}" ]]; then
    if command -v jq >/dev/null 2>&1; then
        TMP="$(mktemp)"
        jq --argjson v "${RUN_RERANK}" '.rerank_enabled = $v' "${CONFIG_PATH}" > "${TMP}"
        mv "${TMP}" "${CONFIG_PATH}"
        echo "  rerank_enabled = ${RUN_RERANK} (config.json updated)"
    else
        echo "  [warn] jq not found — leaving rerank_enabled at its current value in ${CONFIG_PATH}"
    fi
fi

"${MGIMIND_BIN}" doctor --fix

TS="$(date -u +%Y-%m-%dT%H%M%SZ)"
RUN_DIR="${RESULTS_ROOT}/${TS}"
mkdir -p "${RUN_DIR}"

RAW_PATH="${RUN_DIR}/raw.json"
LOG_PATH="${RUN_DIR}/bench.log"

BENCH_ARGS=("${DATASET_PATH}" --output "${RAW_PATH}")
[[ -n "${BENCH_LIMIT}" ]] && BENCH_ARGS+=(--limit "${BENCH_LIMIT}")

echo "  running: mgimind bench ${BENCH_ARGS[*]}"
echo "  log: ${LOG_PATH}"
"${MGIMIND_BIN}" bench "${BENCH_ARGS[@]}" 2>&1 | tee "${LOG_PATH}"

echo ""
echo "Done. Results in ${RUN_DIR}"
echo "  rerank=$( $RUN_RERANK && echo on || echo off )"
echo "  raw   = ${RAW_PATH}"
echo "  log   = ${LOG_PATH}"
