#!/usr/bin/env python3
import argparse
import json
import os
import time


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--artifact-dir", required=True)
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--scenario", required=True)
    parser.add_argument("--mode", required=True)
    parser.add_argument("--exit-code", required=True, type=int)
    args = parser.parse_args()

    os.makedirs(args.artifact_dir, exist_ok=True)
    files = []
    for root, _, names in os.walk(args.artifact_dir):
        for name in sorted(names):
            path = os.path.join(root, name)
            files.append(os.path.relpath(path, args.artifact_dir))

    payload = {
        "run_id": args.run_id,
        "scenario": args.scenario,
        "mode": args.mode,
        "exit_code": args.exit_code,
        "finished_at_unix_ms": int(time.time() * 1000),
        "artifacts": files,
    }
    with open(os.path.join(args.artifact_dir, "artifact-index.json"), "w", encoding="utf-8") as f:
        json.dump(payload, f, indent=2)
        f.write("\n")


if __name__ == "__main__":
    main()
