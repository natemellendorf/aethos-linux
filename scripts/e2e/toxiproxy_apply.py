#!/usr/bin/env python3
import argparse
import json
import urllib.request


def http(method, url, payload=None):
    body = None
    headers = {"Content-Type": "application/json"}
    if payload is not None:
        body = json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(url=url, data=body, headers=headers, method=method)
    with urllib.request.urlopen(req, timeout=5) as resp:
        raw = resp.read().decode("utf-8")
        return json.loads(raw) if raw else {}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--scenario-file", required=True)
    parser.add_argument("--toxiproxy-url", default="http://127.0.0.1:8474")
    args = parser.parse_args()

    with open(args.scenario_file, "r", encoding="utf-8") as f:
        scenario = json.load(f)

    relay = scenario.get("relay", {}).get("toxiproxy", {})
    if not relay.get("enabled"):
        return

    base = args.toxiproxy_url.rstrip("/")
    proxy_name = "relay"
    toxics = relay.get("toxics", [])

    try:
        existing = http("GET", f"{base}/proxies/{proxy_name}/toxics")
        for toxic in existing:
            http("DELETE", f"{base}/proxies/{proxy_name}/toxics/{toxic['name']}")
    except Exception:
        pass

    for toxic in toxics:
        payload = {
            "name": toxic["name"],
            "type": toxic["type"],
            "stream": toxic.get("stream", "downstream"),
            "attributes": toxic.get("attributes", {}),
        }
        http("POST", f"{base}/proxies/{proxy_name}/toxics", payload)


if __name__ == "__main__":
    main()
