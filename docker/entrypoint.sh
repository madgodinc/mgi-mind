#!/bin/sh
# Entrypoint for the mgi-mind one-command image.
#
# - Seeds an empty /data volume from the baked /opt/mind-seed on first boot, so a
#   `-v mgimind-data:/data` mount doesn't shadow the baked models.
# - Picks a bearer token: MGIMIND_TOKEN if set (stable), else a fresh random one
#   printed at startup. Never a fixed default — with `-p` the brain is reachable
#   from the host, so a known token would be an open door.
# - Binds 0.0.0.0 so the published port is reachable (the server refuses this
#   without a token, which we always provide here).
set -e

SEED=/opt/mind-seed
DATA=/data

# Seed once. `cp -a` into an empty dir; an already-populated volume is left as is.
mkdir -p "$DATA"
if [ -z "$(ls -A "$DATA" 2>/dev/null)" ]; then
    echo "mgi-mind: seeding fresh data dir from baked models..."
    cp -a "$SEED/." "$DATA/" 2>/dev/null || true
fi

# Start Qdrant explicitly as a tracked child. mgimind's own start is a DETACHED
# spawn (process_group(0), child dropped) — fine on a host, but inside a
# container PID 1 isn't an init reaper, so the orphaned Qdrant doesn't survive.
# Run it here as a real child of this shell instead, on the storage path and
# loopback host mgimind expects (127.0.0.1:6334 gRPC).
echo "mgi-mind: starting bundled Qdrant..."
# Run from $DATA: Qdrant writes a `.qdrant-initialized` indicator in its working
# directory, and the default CWD (/) isn't writable by the non-root user.
mkdir -p "$DATA/qdrant/storage"
cd "$DATA"
QDRANT__STORAGE__STORAGE_PATH="$DATA/qdrant/storage" \
QDRANT__SERVICE__HOST="127.0.0.1" \
QDRANT__LOG_LEVEL="WARN" \
    /usr/local/bin/qdrant &
QDRANT_PID=$!
trap 'kill "$QDRANT_PID" 2>/dev/null' TERM INT

# Give Qdrant a moment to bind. serve-http then calls ensure_qdrant_running,
# whose port-probe sees our already-running child and short-circuits (its own
# 15s readiness wait covers any remaining startup time), so it never tries the
# detached spawn that doesn't survive here.
sleep 2

# Token: env-provided or generated.
if [ -n "$MGIMIND_TOKEN" ]; then
    TOKEN="$MGIMIND_TOKEN"
    echo "mgi-mind: using MGIMIND_TOKEN from the environment."
else
    # POSIX-ish random token; mgimind itself would generate one, but we need it
    # named (anonymous tokens can't bind 0.0.0.0).
    TOKEN="$(head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n')"
    echo "============================================================"
    echo "  mgi-mind bearer token (no MGIMIND_TOKEN set, generated):"
    echo "    $TOKEN"
    echo "  Use it as:  Authorization: Bearer $TOKEN"
    echo "  Set MGIMIND_TOKEN to pin a stable one."
    echo "============================================================"
fi

# If the user passed their own command (e.g. `docker run ... doctor` or the more
# Docker-idiomatic `docker run ... mgimind doctor`), honor it. The ENTRYPOINT is
# already the mgimind wrapper, so strip a leading `mgimind` if present.
if [ "$1" = "mgimind" ]; then
    shift
fi
if [ "$#" -gt 0 ]; then
    exec mgimind "$@"
fi

# Default: serve on all interfaces (so `-p` reaches it) with the token.
exec mgimind serve-http --host 0.0.0.0 --port 8765 --agent-token "user:$TOKEN"
