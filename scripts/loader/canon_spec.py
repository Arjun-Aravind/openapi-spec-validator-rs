"""Dump upstream's loaded value for a spec file in the type-tagged canonical form.

Usage: python -I scripts/loader/canon_spec.py SPEC OUT.json
"""

import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from upstream import tagged  # noqa: E402
from openapi_spec_validator.readers import read_from_filename  # noqa: E402

spec, _ = read_from_filename(sys.argv[1])
with open(sys.argv[2], "w", encoding="utf-8") as f:
    json.dump(tagged(spec), f, ensure_ascii=False, separators=(",", ":"))
