# mgi-mind on CUDA — Docker image

This is the GPU build of mgi-mind, isolated to a Docker image so the main
release stays a single CPU-only Rust binary. It exists for two reasons:

1. **Bench acceleration.** Running `mgimind bench` on LongMemEval (~500
   questions × ~150 sessions each) takes ~30-60 min on CPU. On a
   recent NVIDIA GPU it drops to ~5-10 min, which makes the bench
   actually re-runnable as the retrieval stack evolves.
2. **Power-user GPU path.** If you ingest a lot of long documents at
   once (`mind_ingest` with thousands of candidates), the embedding
   batch is the bottleneck — CUDA shifts that from CPU-seconds to
   GPU-milliseconds.

The image is **not** part of the standard `install.sh` install. The
default user keeps a single `~/.local/bin/mgimind` binary on CPU; this
Docker setup is opt-in.

## Requirements

- Linux host with an NVIDIA GPU (driver 555+ recommended for Blackwell)
- Docker
- `nvidia-container-toolkit` installed and Docker restarted

One-time toolkit install on Pop!_OS / Ubuntu 24.04:

```sh
curl -fsSL https://nvidia.github.io/libnvidia-container/gpgkey \
  | sudo gpg --dearmor -o /usr/share/keyrings/nvidia-container-toolkit-keyring.gpg
curl -fsSL https://nvidia.github.io/libnvidia-container/stable/deb/nvidia-container-toolkit.list \
  | sed 's#deb https://#deb [signed-by=/usr/share/keyrings/nvidia-container-toolkit-keyring.gpg] https://#g' \
  | sudo tee /etc/apt/sources.list.d/nvidia-container-toolkit.list
sudo apt update && sudo apt install -y nvidia-container-toolkit
sudo nvidia-ctk runtime configure --runtime=docker
sudo systemctl restart docker

# Verify
sudo docker run --rm --gpus all nvidia/cuda:12.8.0-base-ubuntu24.04 nvidia-smi
```

## Build

From the repo root:

```sh
sudo docker build -t mgi-mind-cuda -f docker/cuda/Dockerfile .
```

First build takes ~5-10 min (Rust compile + 600 MB ORT-GPU download).
Subsequent builds are cached.

## Run the bench

```sh
sudo docker run --rm --gpus all \
  -v ~/mgimind:/data \
  -v /absolute/path/to/longmemeval_s.json:/dataset.json:ro \
  mgi-mind-cuda bench /dataset.json --output /data/bench-results.json
```

What this does:

- Mounts your existing `~/mgimind` as `/data` so models cached on the
  host are reused (no re-download inside the container).
- Mounts the LongMemEval JSON read-only.
- Runs the bench with CUDA enabled by default (`MGIMIND_USE_CUDA=1`).
- Writes per-question raw results to `~/mgimind/bench-results.json`.

The summary R@1 / R@5 / R@10 goes to stdout.

## A/B sanity check

Same image, CPU mode (to verify the GPU path is actually doing work):

```sh
sudo docker run --rm --gpus all \
  -v ~/mgimind:/data \
  -v /path/to/longmemeval_s.json:/dataset.json:ro \
  -e MGIMIND_USE_CUDA=0 \
  mgi-mind-cuda bench /dataset.json --limit 20 --output /data/bench-cpu.json
```

A `--limit 20` CPU run takes ~3 min; the same on GPU should take ~30s.
Recall numbers should match (within rounding) — CUDA is for speed, not
for changing the model behavior.

## Versions / Blackwell notes

- Base image: `nvidia/cuda:12.8.0-cudnn-runtime-ubuntu24.04`. CUDA 12.8
  is the first toolkit with native Blackwell (sm_120) codegen.
- ONNX Runtime: 1.24.2 GPU build (`onnxruntime-linux-x64-gpu`).
- The image bundles libonnxruntime + the CUDA / CUDA EP shared libs;
  the host only needs the NVIDIA driver, not the full CUDA toolkit.

If you see "fell back to CPU EP" in the logs, the most common causes
are: wrong base image (CUDA < 12.8 on Blackwell), missing
`nvidia-container-toolkit`, or `--gpus all` left off the `docker run`.
