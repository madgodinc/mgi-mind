# mgi-mind in one command. The whole brain — the binary, a bundled Qdrant, the
# ONNX runtime, and the embedding + reranker models — baked into one image.
#
#   docker run -p 8765:8765 madgodinc/mgi-mind
#
# It prints a bearer token on startup; point the Python client or any agent at
# http://localhost:8765 with that token. Persist memory across restarts with a
# volume:  docker run -p 8765:8765 -v mgimind-data:/data madgodinc/mgi-mind
#
# CPU-only. For GPU (the bench harness), see docker/cuda/Dockerfile.

# ---------- Stage 1: build the binary ----------
# rustc 1.88+ required by the ort/icu transitive deps; pin a recent stable for a
# reproducible build.
FROM rust:1.91-slim-bookworm AS builder

# OpenSSL + pkg-config for reqwest (used by `doctor --fix`), g++ for native deps.
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config libssl-dev ca-certificates curl g++ \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY tests ./tests
# CPU build — do NOT enable the `cuda` or `extractor` features (heavy, optional).
RUN cargo build --release --bin mgimind

# ---------- Stage 2: slim runtime, models baked in ----------
FROM debian:bookworm-slim

# Runtime deps: ca-certificates + libssl3 for the doctor downloads at build time;
# glibc comes with the base (matches the rust:bookworm builder, no version skew).
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /src/target/release/mgimind /usr/local/bin/mgimind

# Bake the whole runtime into the image: `doctor --fix` downloads the bundled
# Qdrant binary + the ONNX runtime (both land next to the binary in
# /usr/local/bin) and the embedding + reranker models (under MGIMIND_HOME). We
# point HOME at /opt/mind-seed so the heavy models live OUTSIDE /data; the
# entrypoint seeds an empty mounted /data volume from this seed on first boot,
# so a `-v` mount never shadows the baked models.
ENV MGIMIND_HOME=/opt/mind-seed
RUN mgimind doctor --fix

# /data is the live data dir at runtime (Qdrant storage + sessions + a copy of
# the models). Declared as a volume so memory can persist across containers.
ENV MGIMIND_HOME=/data
VOLUME /data
EXPOSE 8765

COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

# Run as a non-root user: a network-exposed brain shouldn't be root inside the
# container. Pre-own the writable paths (the baked seed and the data dir). Named
# volumes inherit the image dir's ownership, so chowning /data here is enough;
# a bind mount would need matching host-side ownership.
RUN useradd -r -u 10001 -m -d /home/mind mind \
    && mkdir -p /data \
    && chown -R mind:mind /data /opt/mind-seed
USER mind

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
