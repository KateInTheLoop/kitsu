#!/usr/bin/env bash
# Dogfooding runner: exercise kitsu across scenarios, capture transcripts +
# exit codes + latency. Emits one progress line per case (for Monitor); full
# transcripts land in results/. Bounded by per-case timeouts (no hangs).
set -u
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
BIN="${BIN:-$ROOT/target/debug/kitsu}"
export KITSU_CONFIG="${KITSU_CONFIG:-$ROOT/kitsu.toml}"
GATEWAY="${GATEWAY:-ollama}"
MODEL="${MODEL:-qwen2.5-coder:latest}"
DF="$ROOT/tests/dogfood"
RES="$DF/results"; [ -n "${KEEP_RESULTS:-}" ] || rm -rf "$RES"; mkdir -p "$RES"
GEN="$RES/gen"; mkdir -p "$GEN"
READFILE="$RES/readme_fixture.txt"; printf 'FIRST LINE MARKER\nsecond line\n' > "$READFILE"
CASE_TIMEOUT="${CASE_TIMEOUT:-240}"

run() { # name  "cmd..."   (cmd already includes the binary)
  local name="$1"; shift
  local out="$RES/$name.txt"
  local t0 t1 ms code
  t0=$(date +%s%3N)
  { echo "### CASE: $name"; echo "### CMD: $*"; echo "### --- output (stdout+stderr) ---"; } > "$out"
  timeout "$CASE_TIMEOUT" bash -c "$*" >> "$out" 2>&1
  code=$?
  t1=$(date +%s%3N); ms=$((t1 - t0))
  { echo "### --- end ---"; echo "### EXIT: $code"; echo "### MS: $ms"; } >> "$out"
  if [ "$code" -eq 124 ]; then
    echo "[TIMEOUT] $name (>${CASE_TIMEOUT}s)"
  else
    echo "[done] $name exit=$code ms=$ms"
  fi
}

echo "== CLI-behavior cases (no model) =="
run cli_missing_token   "env -u ANTHROPIC_API_KEY '$BIN' --gateway anthropic ask 'hi'"
run cli_unknown_gateway "'$BIN' --gateway nope_not_a_gateway ask 'hi'"
run cli_help            "'$BIN' --help"
run cli_bad_script_path "'$BIN' --gateway $GATEWAY chat --script /no/such/script.txt"

echo "== model scenarios (gateway=$GATEWAY model=$MODEL) =="
COMMON="--gateway $GATEWAY --model '$MODEL' --timeout-secs 200 --max-output-tokens 400 --max-rounds 6"
for scn in "$DF"/${SCN_DIR:-scenarios}/*.txt; do
  name="scn_$(basename "$scn" .txt)"
  work="$RES/$name.script"
  sed -e "s#__GEN__#$GEN#g" -e "s#__READFILE__#$READFILE#g" "$scn" > "$work"
  run "$name" "'$BIN' $COMMON chat --allow-path '$GEN' --allow-path '$(dirname "$READFILE")' --allow-write '$GEN' --script '$work'"
done

echo "== generated files =="
ls -la "$GEN" | sed 's/^/[gen] /'
echo "[ALL DONE] results in $RES"
