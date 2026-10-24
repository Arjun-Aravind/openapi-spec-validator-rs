"""Lists the corpus specs every benchmarked tool accepts.

Both CLIs stop at the first invalid file, so the corpus benchmark only passes files
that openapi-spec-validator accepts with both of its schema backends.

Usage: python bench/valid_corpus.py APIS_DIR OPENAPI_SPEC_VALIDATOR [CACHE.jsonl ...]

CACHE files are reference results written by scripts/agreement.py; without them the
reference CLI is run on every file.
"""

import json
import os
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor

BACKENDS = ("jsonschema", "jsonschema-rs")


def specs(root):
    for directory, _, names in os.walk(root):
        for name in names:
            if name.endswith((".yaml", ".yml", ".json")):
                yield os.path.join(directory, name)


def accepted(cli, path):
    for backend in BACKENDS:
        env = dict(os.environ, OPENAPI_SPEC_VALIDATOR_SCHEMA_VALIDATOR_BACKEND=backend)
        if subprocess.run([cli, path], capture_output=True, env=env).returncode:
            return False
    return True


def main():
    root, cli, *caches = sys.argv[1:]
    files = sorted(specs(root))
    if caches:
        ok = {backend: set() for backend in BACKENDS}
        for cache in caches:
            with open(cache) as fh:
                for line in fh:
                    entry = json.loads(line)
                    if entry["verdict"] == "OK":
                        ok[entry["backend"]].add(os.path.abspath(entry["path"]))
        keep = [f for f in files if all(os.path.abspath(f) in ok[b] for b in BACKENDS)]
    else:
        with ThreadPoolExecutor(os.cpu_count()) as pool:
            keep = [f for f, good in zip(files, pool.map(lambda f: accepted(cli, f), files)) if good]
    print("\n".join(keep))


if __name__ == "__main__":
    main()
