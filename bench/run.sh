#!/usr/bin/env bash
# Benchmarks openapi-spec-validator-rs against openapi-spec-validator (both schema
# backends) and vacuum. Results go to bench/results.
#
# Environment:
#   UPSTREAM  directory holding the openapi-spec-validator executable (with jsonschema-rs
#             installed in the same environment), e.g. .venv-upstream/bin
#   RUNS      hyperfine runs per command (default 10)
#   CORPUS    set to 1 to also time the APIs.guru corpus (slow: about half an hour)
#   SPECS     set to 0 to skip the per-spec timings
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DATA="$ROOT/bench/data"
OUT="$ROOT/bench/results"
UPSTREAM="${UPSTREAM:?set UPSTREAM to the directory containing openapi-spec-validator}"
RUNS="${RUNS:-10}"
RS="$ROOT/target/release/openapi-spec-validator-rs"
UP="$UPSTREAM/openapi-spec-validator"
BACKEND=OPENAPI_SPEC_VALIDATOR_SCHEMA_VALIDATOR_BACKEND
mkdir -p "$OUT"
cd "$ROOT"

"$ROOT/bench/fetch.sh"
cargo build --release --quiet -p openapi-spec-validator-rs-cli --manifest-path "$ROOT/Cargo.toml"

for spec in github.json github.yaml stripe.json kubernetes.json; do
  [ "${SPECS:-1}" = 1 ] || break
  commands=(
    -n "openapi-spec-validator-rs" "$RS $DATA/$spec"
    -n "openapi-spec-validator (jsonschema-rs backend)" "env $BACKEND=jsonschema-rs $UP $DATA/$spec"
    -n "openapi-spec-validator (default backend)" "env $BACKEND=jsonschema $UP $DATA/$spec"
  )
  if command -v vacuum >/dev/null; then
    commands+=(-n "vacuum (validation rules only)" "vacuum lint -q -x --no-update-check -r $ROOT/bench/vacuum-validate.yaml $DATA/$spec")
  fi
  hyperfine --warmup 1 --runs "$RUNS" --ignore-failure \
    --export-json "$OUT/${spec%.*}-${spec##*.}.json" "${commands[@]}"
done

if [ "${CORPUS:-0}" = 1 ]; then
  # Specs openapi-spec-validator accepts, so every tool processes every file (both CLIs
  # stop at the first invalid file).
  python3 "$ROOT/bench/valid_corpus.py" "$DATA/openapi-directory/APIs" "$UP" $(ls "$OUT"/reference-*.jsonl 2>/dev/null) > "$DATA/valid-specs.txt"
  export CORPUS_LIST="$DATA/valid-specs.txt"
  wrap="$ROOT/bench/with_corpus.sh"
  hyperfine --runs 1 --export-json "$OUT/corpus.json" \
    -n "openapi-spec-validator-rs" "$wrap $RS" \
    -n "openapi-spec-validator-rs (1 thread)" "env RAYON_NUM_THREADS=1 $wrap $RS" \
    -n "openapi-spec-validator (jsonschema-rs backend)" "env $BACKEND=jsonschema-rs $wrap $UP" \
    -n "openapi-spec-validator (default backend)" "env $BACKEND=jsonschema $wrap $UP"
fi

python3 "$ROOT/bench/chart.py" "$OUT" "$ROOT/bench/benchmarks.svg"
