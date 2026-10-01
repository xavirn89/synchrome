"""E9 (Realtime) — validate Supabase Realtime over WebSocket with two concurrent clients.

Minimal stdlib WebSocket client (no deps). Opens two connections to the Realtime endpoint,
joins the same topic, sends a Broadcast message from client 1 and measures how long client 2
takes to receive it. Proves the push transport Synchrome Cloud will use instead of polling.
A custom User-Agent is sent to avoid Cloudflare error 1010.

Usage: python3 e9_realtime.py [/path/to/env]
"""
import base64, json, os, socket, ssl, struct, sys, threading, time, urllib.parse, uuid

ENV = sys.argv[1] if len(sys.argv) > 1 else "/tmp/e9-synchrome.env"
E = {}
for line in open(ENV):
    line = line.rstrip("\n")
    if line and not line.startswith("#") and "=" in line:
        k, v = line.split("=", 1)
        E[k.strip()] = v.strip()
URL = E["SUPABASE_URL"].rstrip("/")
ANON = E["SUPABASE_ANON_KEY"]
host = urllib.parse.urlparse(URL).netloc
WS_PATH = f"/realtime/v1/websocket?apikey={ANON}&vsn=1.0.0"


class WS:
    def __init__(self, host, path):
        raw = socket.create_connection((host, 443), timeout=15)
        self.sock = ssl.create_default_context().wrap_socket(raw, server_hostname=host)
        key = base64.b64encode(os.urandom(16)).decode()
        req = (f"GET {path} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
               f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\nUser-Agent: Synchrome-E9/0.1\r\n\r\n")
        self.sock.send(req.encode())
        buf = b""
        while b"\r\n\r\n" not in buf:
            buf += self.sock.recv(4096)
        if b" 101 " not in buf.split(b"\r\n")[0]:
            raise RuntimeError("handshake failed: " + buf.split(b"\r\n")[0].decode("latin1"))
        self._rest = buf.split(b"\r\n\r\n", 1)[1]

    def send(self, obj):
        data = json.dumps(obj).encode()
        hdr = bytearray([0x81])
        n = len(data)
        mask = os.urandom(4)
        if n < 126:
            hdr.append(0x80 | n)
        elif n < 65536:
            hdr.append(0x80 | 126); hdr += struct.pack(">H", n)
        else:
            hdr.append(0x80 | 127); hdr += struct.pack(">Q", n)
        hdr += mask
        self.sock.send(bytes(hdr) + bytes(b ^ mask[i % 4] for i, b in enumerate(data)))

    def _read(self, n):
        while len(self._rest) < n:
            chunk = self.sock.recv(4096)
            if not chunk:
                raise ConnectionError("closed")
            self._rest += chunk
        out, self._rest = self._rest[:n], self._rest[n:]
        return out

    def recv(self):
        b0, b1 = self._read(2)
        ln = b1 & 0x7F
        if ln == 126:
            ln = struct.unpack(">H", self._read(2))[0]
        elif ln == 127:
            ln = struct.unpack(">Q", self._read(8))[0]
        payload = self._read(ln)
        return payload.decode("utf-8", "replace")

    def close(self):
        try:
            self.sock.close()
        except Exception:
            pass


TOPIC = f"realtime:e9-{uuid.uuid4().hex[:8]}"
PASS, FAIL = [], []


def join_msg(ref):
    return {"topic": TOPIC, "event": "phx_join", "ref": str(ref),
            "payload": {"config": {"broadcast": {"self": True, "ack": False}}, "access_token": ANON}}


print(f"=== E9 Realtime against {host} ===")
try:
    c1 = WS(host, WS_PATH)
    c2 = WS(host, WS_PATH)
    print("  [PASS] two WebSocket connections established"); PASS.append("connect")
except Exception as ex:
    print("  [FAIL] WebSocket handshake:", ex); sys.exit(1)

c1.send(join_msg(1))
c2.send(join_msg(2))

# Drain join replies briefly
joined = {"c1": False, "c2": False}
for c, nm in ((c1, "c1"), (c2, "c2")):
    c.sock.settimeout(8)
    try:
        for _ in range(5):
            m = json.loads(c.recv())
            if m.get("event") == "phx_reply" and m.get("payload", {}).get("status") == "ok":
                joined[nm] = True; break
    except Exception:
        pass
check = joined["c1"] and joined["c2"]
print(f"  [{'PASS' if check else 'FAIL'}] both clients joined topic — {joined}")
(PASS if check else FAIL).append("join")

# Client 1 broadcasts; client 2 should receive.
received = {"t": None, "payload": None}


def listen():
    c2.sock.settimeout(10)
    try:
        while True:
            m = json.loads(c2.recv())
            if m.get("event") == "broadcast":
                received["t"] = time.time()
                received["payload"] = m.get("payload", {}).get("payload")
                return
    except Exception:
        return


th = threading.Thread(target=listen); th.start()
time.sleep(0.5)
sent_at = time.time()
nonce = uuid.uuid4().hex
c1.send({"topic": TOPIC, "event": "broadcast", "ref": "9",
         "payload": {"type": "broadcast", "event": "e9", "payload": {"nonce": nonce}}})
th.join(10)

got = received["payload"] and received["payload"].get("nonce") == nonce
latency = (received["t"] - sent_at) * 1000 if received["t"] else None
print(f"  [{'PASS' if got else 'FAIL'}] client 2 received client 1's broadcast"
      + (f" — latency {latency:.0f} ms" if latency else " — timeout"))
(PASS if got else FAIL).append("broadcast")

c1.close(); c2.close()
print(f"\n=== E9 Realtime summary: {len(PASS)} pass, {len(FAIL)} fail ===")
if FAIL:
    print("  failures:", ", ".join(FAIL))
