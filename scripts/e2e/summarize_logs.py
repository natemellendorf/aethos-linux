#!/usr/bin/env python3
import argparse
import json
import os


def parse_json_lines(path):
    events = []
    if not os.path.exists(path):
        return events
    with open(path, "r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                events.append(json.loads(line))
            except Exception:
                continue
    return events


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--artifact-dir", required=True)
    args = parser.parse_args()

    failure_file = os.path.join(args.artifact_dir, "failure-summary.json")
    if not os.path.exists(failure_file):
        return

    with open(failure_file, "r", encoding="utf-8") as f:
        failure = json.load(f)

    summary = {
        "failure": failure.get("failure"),
        "hints": [],
    }

    a_tail = failure.get("logs", {}).get("wayfarer_1_tail", "")
    b_tail = failure.get("logs", {}).get("wayfarer_2_tail", "")
    if "gossip_send_request_from_relay_ingest" in a_tail and "gossip_transfer_imported_messages" not in b_tail:
        summary["hints"].append("REQUEST observed without subsequent TRANSFER import on recipient")
    if "relay_worker_sync_failed" in a_tail or "relay_worker_sync_failed" in b_tail:
        summary["hints"].append("Relay path failing; verify relay endpoint/proxy scenario")

    with open(os.path.join(args.artifact_dir, "triage-summary.json"), "w", encoding="utf-8") as f:
        json.dump(summary, f, indent=2)
        f.write("\n")


if __name__ == "__main__":
    main()
