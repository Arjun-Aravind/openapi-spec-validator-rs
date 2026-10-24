#!/usr/bin/env bash
# Downloads the benchmark specs into bench/data (not committed).
set -euo pipefail

DATA="$(cd "$(dirname "$0")" && pwd)/data"
mkdir -p "$DATA"

fetch() {
  local name=$1 url=$2
  [ -s "$DATA/$name" ] || curl -sSfL -o "$DATA/$name" "$url"
}

fetch github.json https://raw.githubusercontent.com/github/rest-api-description/main/descriptions/api.github.com/api.github.com.json
fetch github.yaml https://raw.githubusercontent.com/github/rest-api-description/main/descriptions/api.github.com/api.github.com.yaml
fetch stripe.json https://raw.githubusercontent.com/stripe/openapi/master/openapi/spec3.json
fetch kubernetes.json https://raw.githubusercontent.com/kubernetes/kubernetes/master/api/openapi-spec/swagger.json

if [ ! -d "$DATA/openapi-directory" ]; then
  git clone -q --depth 1 https://github.com/APIs-guru/openapi-directory.git "$DATA/openapi-directory"
fi
