#!/usr/bin/env bash
# STALE bench — 4 параллельные дорожки, каждая со своим Qdrant + своим ключом.
# Free-tier обход: 4 ключа × 15 req/min = 60 req/min. ПК свободен (работа в облаке Google).
#
# Дорожка i: свой MGIMIND_HOME (свой qdrant_port + storage) + свой ключ.
# Батчи раздаются по дорожкам круговым делением (b00→track0, b01→track1, ...).
# Чекпоинт: результат каждого батча сразу на диск; готовые не перегоняются.
set -u
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

BIN="$PWD/target/release/mgimind"
QBIN="$PWD/target/release/qdrant"
MODEL="gemini-3.1-flash-lite"
ROOT="$HOME/Brain/stale-run"
BATCHDIR="$ROOT/batches"
OUTDIR="$ROOT/results"
mkdir -p "$OUTDIR"

mapfile -t KEYS < "$ROOT/keys.txt"
NTRACK=${#KEYS[@]}
echo "[$(date '+%T')] дорожек/ключей: $NTRACK"

# базовые gRPC-порты дорожек (6400, 6410, 6420, 6430...) — не трогают общий 6334
BASEPORT=6400

start_qdrant() {  # $1=track idx
  local t=$1
  local gport=$((BASEPORT + t*10))
  local hport=$((BASEPORT + t*10 + 1))
  local home="$ROOT/track$t"
  local storage="$home/qdrant-storage"
  mkdir -p "$storage"
  # config.json для mgimind: свой порт qdrant
  cat > "$home/config.json" <<CFG
{"version":"0.1.0","data_dir":"$home/data","model_name":"all-MiniLM-L6-v2","qdrant_port":$gport,"vector_size":384}
CFG
  mkdir -p "$home/data"
  # запуск изолированного qdrant
  QDRANT__SERVICE__GRPC_PORT=$gport \
  QDRANT__SERVICE__HTTP_PORT=$hport \
  QDRANT__SERVICE__HOST=127.0.0.1 \
  QDRANT__STORAGE__STORAGE_PATH="$storage" \
  QDRANT__LOG_LEVEL=ERROR \
    nice -n 19 "$QBIN" >/dev/null 2>&1 &
  echo $! > "$home/qdrant.pid"
  echo "[$(date '+%T')]   track$t: qdrant gRPC=$gport http=$hport (pid $(cat $home/qdrant.pid))"
}

run_track() {  # $1=track idx
  local t=$1
  local home="$ROOT/track$t"
  local key="${KEYS[$t]}"
  local log="$ROOT/track$t.log"
  : > "$log"
  # батчи этой дорожки: b{t}, b{t+NTRACK}, b{t+2*NTRACK}...
  for ds in "$BATCHDIR"/b*.json; do
    local name=$(basename "$ds" .json)        # bNN
    local num=$((10#${name#b}))               # NN -> int
    [ $((num % NTRACK)) -eq "$t" ] || continue
    local out="$OUTDIR/$name-result.json"
    if [ -f "$out" ] && python3 -c "import json;json.load(open('$out'))" 2>/dev/null; then
      echo "[$(date '+%T')] track$t: $name уже готов, пропуск" >> "$log"; continue
    fi
    echo "[$(date '+%T')] track$t: === $name старт ===" >> "$log"
    MGIMIND_HOME="$home" STALE_EXTRACT_MODEL="$MODEL" GEMINI_API_KEY="$key" \
      nice -n 19 stdbuf -oL -eL "$BIN" bench-stale "$ds" \
        --llm-extract --backbone "$MODEL" --judge "$MODEL" \
        --focused --haystack reduced --window 2 \
        --output "$out" >> "$log" 2>&1
    if [ -f "$out" ]; then
      echo "[$(date '+%T')] track$t: $name ГОТОВ" >> "$log"
    else
      echo "[$(date '+%T')] track$t: $name НЕ дописал" >> "$log"
    fi
  done
  echo "[$(date '+%T')] track$t: ВСЕ батчи дорожки пройдены" >> "$log"
}

echo "[$(date '+%T')] === поднимаю $NTRACK Qdrant-инстансов ==="
for t in $(seq 0 $((NTRACK-1))); do
  start_qdrant "$t"
  sleep 12   # сдвиг старта — qdrant'ы не дерутся за инициализацию (твоя идея)
done

echo "[$(date '+%T')] === жду готовности qdrant (5с) и запускаю дорожки ==="
sleep 5
for t in $(seq 0 $((NTRACK-1))); do
  run_track "$t" &
  echo "[$(date '+%T')] track$t запущена (pid $!)"
done

wait
echo "[$(date '+%T')] ====== ВСЕ ДОРОЖКИ ЗАВЕРШЕНЫ ======"
ls -la "$OUTDIR"/*.json 2>/dev/null
# гасим qdrant-инстансы
for t in $(seq 0 $((NTRACK-1))); do
  kill "$(cat "$ROOT/track$t/qdrant.pid" 2>/dev/null)" 2>/dev/null
done
echo "[$(date '+%T')] qdrant-инстансы остановлены"
