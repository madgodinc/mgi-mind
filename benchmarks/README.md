# Benchmarks

Reproducible benchmark results for mgi-mind retrieval and recall.

## Layout

```
benchmarks/
├── README.md                           # this file
├── v0.14.3-gpu/                        # baseline: pre-v1.4 retrieval
│   ├── README.md                       # what was run + how
│   └── results.json                    # headline R@k numbers
└── (future runs land here, one dir per version)
```

Each `<version>-<platform>/` directory contains:

- `README.md` — what was run, hardware, model, env vars, reproduction steps.
- `results.json` — machine-parseable numbers (R@k by mode, wall-time, raw counts).
- Optional: `raw.json` — per-question outputs if the run produced them.

## Headlines

| Version | Dataset | R@1 | R@5 | R@10 | Wall | Hardware | Notes |
|---------|---------|-----|-----|------|------|----------|-------|
| **v0.14.3** + e5-base + reranker | LongMemEval-S (500q) | 92.6% | **99.2%** | 100.0% | 1539s | RTX 3090 | best run; CHANGELOG anchor for v1.0 |
| v0.14.3 + e5-base (no reranker) | LongMemEval-S (500q) | 88.4% | 98.0% | 99.4% | 629s | RTX 3090 | reranker contributes +4.2pp R@1 |
| v0.14.3 + MiniLM (variance check) | LongMemEval-S (500q) | 85.0% | 98.0% | 99.6% | 351s | RTX 3090 | MiniLM GPU ≈ MiniLM INT8 CPU (audit) |
| v0.8.1 baseline (MiniLM INT8 CPU) | LongMemEval-S (500q) | 85.2% | 98.2% | 99.4% | 1h45m | RTX 5060 Ti | comparison baseline |

R@k is **retrieval recall** — was the right memory in the top-K returned. Not QA accuracy. See [issue #17](https://github.com/madgodinc/mgi-mind/issues/17) for the QA-accuracy plan.

## Reproducing v0.14.3 numbers locally

Hardware target: any consumer GPU with ≥ 8 GB VRAM (tested on RTX 3090 and RTX 5060 Ti 16 GB).

The single biggest gotcha: `mgimind doctor --fix` downloads **INT8 quantized** embedding models. ONNX Runtime's CUDA execution provider does NOT implement INT8 ops (`MatMulInteger`, `DynamicQuantizeLinear`); inputs fall back to CPU and the GPU sits idle. To actually use the GPU you have to swap in the FP16 versions.

Steps:

```sh
# 1. Build with CUDA feature.
cargo build --release --features cuda

# 2. Download ORT GPU 1.24.2 (302 MB; default install is CPU only).
wget https://github.com/microsoft/onnxruntime/releases/download/v1.24.2/onnxruntime-linux-x64-gpu-1.24.2.tgz
# Place libonnxruntime.so, libonnxruntime_providers_cuda.so,
# libonnxruntime_providers_shared.so next to the binary.

# 3. Download FP16 model from Xenova on HuggingFace.
#    e5-base FP16: ~530 MB.
#    MiniLM FP16:  ~45 MB.
wget https://huggingface.co/Xenova/multilingual-e5-base/resolve/main/onnx/model_fp16.onnx

# 4. Replace $MGIMIND_HOME/models/multilingual-e5-base/model.onnx with the FP16 file.

# 5. Patch src/integrity.rs with the new sha256:
#    e5-base FP16:  5d760477f691b665da2b94e1528eb6938b795f76064d9392e6af7118b8a3f54a
#    MiniLM FP16:   2cdb5e58291813b6d6e248ed69010100246821a367fa17b1b81ae9483744533d

# 6. Rebuild.
cargo build --release --features cuda

# 7. Run with CUDA enabled.
MGIMIND_USE_CUDA=1 \
  LD_LIBRARY_PATH=./target/release:/usr/local/cuda/lib64:/usr/lib/x86_64-linux-gnu \
  ./target/release/mgimind bench longmemeval-s.json --output run.json
```

Sanity check: `nvidia-smi` during the run should show >20% utilisation. <5% means CUDA EP did not register and you're on CPU fallback.

## Reproducing the v0.8.1 CPU baseline

Default install. No GPU setup. `mgimind doctor --fix` is enough. Set `--output` if you want raw numbers.

## What's next (issue #16)

The v1.4 / v1.5 changes (duel rule, doubt window, install-mode profile, active re-test pass) need their own R@k regression check — STALE bench gate is `R@5 regression < 1.0pp` against the v0.14.3 numbers above. Tooling scaffold lives in `src/bench_stale.rs`; CLI surface is `mgimind bench-stale` / `mgimind bench-stale-sweep`. The dataset adapter is the missing piece.
