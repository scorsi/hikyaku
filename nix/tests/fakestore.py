"""Test-only refusing proxy in front of a real store (M2 nixosTest `rejected`).

fakestore <listen port> <upstream port> <cert> <key> <state dir>

Forwards every request unchanged to https://127.0.0.1:<upstream port> and relays the answer,
except as `<state dir>/mode` says (read at each push, absent = pass):

- pass: forward everything;
- refuse-once: answer 400 PushRejected{record_index, "test-refused"} for the first record
  holding the marker `refuse-me`, then switch to pass;
- refuse-once-then-down: the same, then switch to down;
- refuse-replacements: refuse a record holding the marker, and any replacement (a nisshi
  `rejected` record or a masu `rejected:` scrape), every time;
- down: answer 503 to every push.

Every push is logged to `<state dir>/requests.ndjson` (time, first_seq, records, code) and every
refusal to `<state dir>/refused.ndjson` (seq, the refused bytes in base64), the oracle of the
test. The real stores never refuse a record hikyaku builds (same rules, ADR 0003), so a
disagreement can only be staged; this proxy stages it while the real store still validates and
stores the replacement.
"""

import base64
import http.client
import http.server
import json
import pathlib
import ssl
import subprocess
import sys
import time

MARKER = b"refuse-me"


def varint(buf: bytes, i: int) -> tuple[int, int]:
    n = shift = 0
    while True:
        b = buf[i]
        i += 1
        n |= (b & 0x7F) << shift
        shift += 7
        if b < 0x80:
            return n, i


def encode_varint(n: int) -> bytes:
    out = bytearray()
    while True:
        b = n & 0x7F
        n >>= 7
        out.append(b | (0x80 if n else 0))
        if not n:
            return bytes(out)


def push_request(pb: bytes) -> tuple[int, list[bytes]]:
    """first_seq (field 3) and records (field 4) of a PushRequest, both stores' shape."""
    first_seq, records, i = 0, [], 0
    while i < len(pb):
        key, i = varint(pb, i)
        field, wire = key >> 3, key & 7
        if wire == 0:
            value, i = varint(pb, i)
            if field == 3:
                first_seq = value
        elif wire == 2:
            length, i = varint(pb, i)
            if field == 4:
                records.append(pb[i : i + length])
            i += length
        else:
            raise ValueError(f"wire type {wire}")
    return first_seq, records


def rejected(index: int) -> bytes:
    reason = b"test-refused"
    return b"\x08" + encode_varint(index) + b"\x12" + encode_varint(len(reason)) + reason


def is_replacement(record: bytes) -> bool:
    return b'"event":"rejected"' in record or b'"error":"rejected:' in record


LISTEN, UPSTREAM = int(sys.argv[1]), int(sys.argv[2])
CERT, KEY = sys.argv[3], sys.argv[4]
STATE = pathlib.Path(sys.argv[5])
STATE.mkdir(parents=True, exist_ok=True)
# Loopback to the real store: its certificate is the agent's business, not the proxy's.
UPSTREAM_TLS = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
UPSTREAM_TLS.check_hostname = False
UPSTREAM_TLS.verify_mode = ssl.CERT_NONE


def mode() -> str:
    try:
        return (STATE / "mode").read_text().strip()
    except FileNotFoundError:
        return "pass"


def log(name: str, entry: dict) -> None:
    with open(STATE / name, "a") as f:
        f.write(json.dumps(entry) + "\n")


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def answer(self, code: int, body: bytes, headers: list[tuple[str, str]]) -> None:
        self.send_response(code)
        for k, v in headers:
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def forward(self, method: str, body: bytes | None) -> int:
        conn = http.client.HTTPSConnection("127.0.0.1", UPSTREAM, context=UPSTREAM_TLS)
        headers = {k: v for k, v in self.headers.items() if k.lower() not in ("host", "connection")}
        conn.request(method, self.path, body=body, headers=headers)
        resp = conn.getresponse()
        data = resp.read()
        keep = [(k, v) for k, v in resp.getheaders() if k.lower() in ("content-type", "www-authenticate")]
        self.answer(resp.status, data, keep)
        conn.close()
        return resp.status

    def do_GET(self) -> None:
        self.forward("GET", None)

    def do_POST(self) -> None:
        body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
        current = mode()
        first_seq, records = 0, []
        try:
            pb = subprocess.run(["zstd", "-dcq"], input=body, capture_output=True, check=True).stdout
            first_seq, records = push_request(pb)
        except (subprocess.CalledProcessError, ValueError, IndexError):
            pass  # the store answers a malformed body itself
        refuse = None
        if current in ("refuse-once", "refuse-once-then-down", "refuse-replacements"):
            for i, r in enumerate(records):
                if MARKER in r or (current == "refuse-replacements" and is_replacement(r)):
                    refuse = i
                    break
        entry = {"t": time.time(), "first_seq": first_seq, "records": len(records)}
        if current == "down":
            self.answer(503, b"", [])
            code = 503
        elif refuse is not None:
            log("refused.ndjson", {"t": time.time(), "seq": first_seq + refuse,
                                   "b64": base64.b64encode(records[refuse]).decode()})
            self.answer(400, rejected(refuse), [("Content-Type", "application/x-protobuf")])
            code = 400
            follow = {"refuse-once": "pass", "refuse-once-then-down": "down"}.get(current)
            if follow:
                (STATE / "mode").write_text(follow + "\n")
        else:
            code = self.forward("POST", body)
        log("requests.ndjson", dict(entry, code=code))

    def log_message(self, format: str, *args: object) -> None:
        pass


server = http.server.ThreadingHTTPServer(("0.0.0.0", LISTEN), Handler)
tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
tls.load_cert_chain(CERT, KEY)
server.socket = tls.wrap_socket(server.socket, server_side=True)
server.serve_forever()
