"""Times `validate(spec_dict)` in-process: openapi-spec-validator vs openapi-spec-validator-rs.

This is the cost a test suite pays when it validates an already-loaded document (for
example FastAPI's `app.openapi()`); parsing and interpreter start-up are excluded.

Run in an environment with both packages (and jsonschema-rs) installed:
    python bench/python_api.py bench/data/github.json bench/data/stripe.json ...
"""

import json
import os
import statistics
import subprocess
import sys
import time

RUNS = int(os.environ.get("RUNS", "5"))

# Each measurement runs in a fresh interpreter: upstream's backend is chosen from the
# environment at import time, and caches would otherwise carry over between runs.
TIMER = r"""
import json, sys, time
path, impl = sys.argv[1:]
spec = json.load(open(path))
if impl == "rs":
    from openapi_spec_validator_rs import validate
else:
    from openapi_spec_validator import validate
start = time.perf_counter()
validate(spec)
print(time.perf_counter() - start)
"""


def measure(path, impl, backend):
    env = dict(os.environ, OPENAPI_SPEC_VALIDATOR_SCHEMA_VALIDATOR_BACKEND=backend)
    times = []
    for _ in range(RUNS):
        out = subprocess.run(
            [sys.executable, "-c", TIMER, path, impl],
            env=env, capture_output=True, text=True, check=True,
        )
        times.append(float(out.stdout))
    return statistics.median(times)


def main():
    rows = []
    for path in sys.argv[1:]:
        rs = measure(path, "rs", "jsonschema")
        up_rs = measure(path, "upstream", "jsonschema-rs")
        up_py = measure(path, "upstream", "jsonschema")
        rows.append((os.path.basename(path), rs, up_rs, up_py))
    print("| Spec | openapi-spec-validator-rs | openapi-spec-validator (jsonschema-rs backend) "
          "| openapi-spec-validator (default backend) |")
    print("|---|---:|---:|---:|")
    for name, rs, up_rs, up_py in rows:
        print(f"| {name} | {rs * 1000:.0f} ms | {up_rs:.2f} s ({up_rs / rs:.0f}×) "
              f"| {up_py:.2f} s ({up_py / rs:.0f}×) |")
    # Named after the specs measured, so a partial run does not replace a full one.
    stems = "-".join(os.path.splitext(name)[0] for name, *_ in rows)
    output = os.path.join(os.path.dirname(__file__), "results", f"python-api-{stems}.json")
    with open(output, "w") as fh:
        json.dump(
            [
                dict(spec=n, rs=a, upstream_jsonschema_rs=b, upstream_default=c)
                for n, a, b, c in rows
            ],
            fh,
            indent=2,
        )


if __name__ == "__main__":
    main()
