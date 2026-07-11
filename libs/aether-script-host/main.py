#!/usr/bin/env python3
"""Aether Script Host - JSON-Lines subprocess for custom data transformations.

This script is launched as a persistent subprocess by io's ScriptRunner.
It loads a user-provided Python transform script and processes incoming
JSON payloads via stdin/stdout JSON-Lines protocol.

Protocol:
  Request  (stdin):  {"id": <int>, "payload": <object>}
  Response (stdout): {"id": <int>, "points": [<point>, ...]}
  Error    (stdout): {"id": <int>, "error": "<message>"}

Each point dict must contain:
  - point_id:   int   (required)
  - point_type: str   (required, one of "T", "S", "C", "A")
  - value:      any   (required, numeric/bool/string)
  - quality:    str   (optional, default "good")
"""

import importlib.util
import json
import sys
import traceback


def load_transform(script_path: str):
    """Load the user's transform() function from the given script path."""
    spec = importlib.util.spec_from_file_location("user_script", script_path)
    if spec is None or spec.loader is None:
        print(
            json.dumps({"id": -1, "error": f"Cannot load script: {script_path}"}),
            flush=True,
        )
        sys.exit(1)

    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)

    if not hasattr(module, "transform"):
        print(
            json.dumps(
                {"id": -1, "error": f"Script missing transform() function: {script_path}"}
            ),
            flush=True,
        )
        sys.exit(1)

    return module.transform


def main():
    if len(sys.argv) < 2:
        print(
            json.dumps({"id": -1, "error": "Usage: main.py <script_path>"}),
            flush=True,
        )
        sys.exit(1)

    script_path = sys.argv[1]
    transform = load_transform(script_path)

    # Signal ready
    print(json.dumps({"id": -1, "status": "ready"}), flush=True)

    # JSON-Lines event loop
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue

        try:
            req = json.loads(line)
            req_id = req.get("id", 0)
            payload = req.get("payload", {})
            points = transform(payload)

            if not isinstance(points, list):
                points = list(points) if points is not None else []

            print(json.dumps({"id": req_id, "points": points}), flush=True)

        except Exception:
            err_msg = traceback.format_exc()
            req_id = 0
            try:
                req_id = json.loads(line).get("id", 0)
            except Exception:
                pass
            print(json.dumps({"id": req_id, "error": err_msg}), flush=True)


if __name__ == "__main__":
    main()
