#!/usr/bin/env bash
# STALE bench с ротацией ключей + чекпоинтами. Всё на Gemini flash-lite (extract+answer+judge),
# ПК свободен (вся работа в облаке Google). Детачнуто, low-prio — не мешает играм.
#
# Запуск:  setsid nice -n 19 ionice -c3 ./scripts/stale-rotate.sh > ~/Brain/stale-run/run.log 2>&1 &
set -u

cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

BIN=./target/release/mgimind
MODEL="gemini-3.1-flash-lite"
OUTDIR="$HOME/Brain/stale-run"
mkdir -p "$OUTDIR"

# 3 ключа (читаются из файла, НЕ хардкод в скрипте). По одному на строку.
KEYFILE="$HOME/Brain/stale-run/keys.txt"
mapfile -t KEYS < "$KEYFILE"
NKEYS=${#KEYS[@]}
echo "[$(date '+%T')] ключей: $NKEYS"

# Батчи = готовые нарезки по 50 сценариев (oct1..oct8 = 8×50 = 400).
BATCHDIR="$HOME/Brain/bench-stale-2026-06-08/json"
BATCHES=(oct1 oct2 oct3 oct4 oct5 oct6 oct7 oct8)

i=0
for b in "${BATCHES[@]}"; do
  ds="$BATCHDIR/$b.json"
  out="$OUTDIR/$b-result.json"
  log="$OUTDIR/$b.log"
  [ -f "$ds" ] || { echo "[$(date '+%T')] НЕТ датасета $ds — пропуск"; continue; }
  # чекпоинт: если результат батча уже есть и валиден — не перегоняем (дожатие)
  if [ -f "$out" ] && python3 -c "import json,sys; json.load(open('$out'))" 2>/dev/null; then
    echo "[$(date '+%T')] $b — уже готов, пропуск"; continue
  fi
  key="${KEYS[$((i % NKEYS))]}"   # ротация ключей по кругу
  i=$((i+1))
  echo "[$(date '+%T')] === батч $b (ключ #$(( (i-1) % NKEYS + 1 ))) → $out ==="
  STALE_EXTRACT_MODEL="$MODEL" GEMINI_API_KEY="$key" \
    "$BIN" bench-stale "$ds" \
      --llm-extract --backbone "$MODEL" --judge "$MODEL" \
      --focused --haystack reduced --window 2 \
      --output "$out" > "$log" 2>&1
  rc=$?
  if [ -f "$out" ]; then
    echo "[$(date '+%T')] $b ГОТОВ (rc=$rc)"
  else
    echo "[$(date '+%T')] $b НЕ дописал результат (rc=$rc) — см $log"
  fi
done

echo "[$(date '+%T')] === ВСЕ БАТЧИ ПРОЙДЕНЫ ==="
ls -la "$OUTDIR"/*-result.json 2>/dev/null
