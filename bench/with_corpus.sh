#!/usr/bin/env bash
# Runs the given command with every file listed in $CORPUS_LIST appended, keeping the
# long argument list out of hyperfine's own command line.
set -euo pipefail
IFS=$'\n' read -r -d '' -a files < "$CORPUS_LIST" || true
exec "$@" "${files[@]}"
