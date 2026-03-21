#!/usr/bin/env python3
import argparse
import json
import os
import socketserver
import threading
import time


STORE = {}
LOCK = threading.Lock()


def now_ms():
    return int(time.time() * 1000)


def append_log(event):
    log_path = os.environ.get("AETHOS_RELAY_LOG_PATH", "")
    if not log_path:
        return
    os.makedirs(os.path.dirname(log_path), exist_ok=True)
    with open(log_path, "a", encoding="utf-8") as f:
      f.write(json.dumps(event, ensure_ascii=True) + "\n")


class RelayHandler(socketserver.StreamRequestHandler):
    def handle(self):
        peer = f"{self.client_address[0]}:{self.client_address[1]}"
        append_log({"ts": now_ms(), "event": "connect", "peer": peer})
        while True:
            line = self.rfile.readline()
            if not line:
                break
            try:
                msg = json.loads(line.decode("utf-8"))
            except Exception:
                self.wfile.write(b'{"type":"error","message":"invalid_json"}\n')
                continue

            typ = msg.get("type")
            append_log({"ts": now_ms(), "event": "recv", "peer": peer, "type": typ})

            if typ == "send":
                item = {
                    "item_id": msg.get("item_id", f"item-{now_ms()}"),
                    "to": msg.get("to"),
                    "from": msg.get("from"),
                    "text": msg.get("text", ""),
                    "received_at_unix": int(time.time())
                }
                with LOCK:
                    STORE.setdefault(item["to"], []).append(item)
                self.wfile.write((json.dumps({"type": "send_ok", "item_id": item["item_id"]}) + "\n").encode("utf-8"))
            elif typ == "pull":
                target = msg.get("for")
                with LOCK:
                    items = STORE.get(target, [])[:]
                self.wfile.write((json.dumps({"type": "messages", "items": items}) + "\n").encode("utf-8"))
            elif typ == "ack":
                self.wfile.write(b'{"type":"ack_ok"}\n')
            else:
                self.wfile.write(b'{"type":"hello_ok"}\n')

        append_log({"ts": now_ms(), "event": "disconnect", "peer": peer})


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", default="0.0.0.0")
    parser.add_argument("--port", type=int, default=8082)
    args = parser.parse_args()

    server = socketserver.ThreadingTCPServer((args.host, args.port), RelayHandler)
    server.allow_reuse_address = True
    append_log({"ts": now_ms(), "event": "relay_start", "host": args.host, "port": args.port})
    server.serve_forever()


if __name__ == "__main__":
    main()
