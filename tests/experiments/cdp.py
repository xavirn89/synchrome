"""Minimal Chrome DevTools Protocol client (stdlib only) for E2E tests on DISPOSABLE profiles.

Launch the browser with --remote-debugging-port=0; the port is read from
<user-data-dir>/DevToolsActivePort. Only what the bookmarks E2E needs: attach to the
Synchrome extension's service worker and evaluate expressions there (chrome.bookmarks.*).
"""
import base64, json, os, socket, struct, time


class WS:
    def __init__(self, host, port, path, timeout=30):
        self.s = socket.create_connection((host, port), timeout=timeout)
        key = base64.b64encode(os.urandom(16)).decode()
        req = (f"GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nUpgrade: websocket\r\n"
               f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n")
        self.s.sendall(req.encode())
        resp = b""
        while b"\r\n\r\n" not in resp:
            chunk = self.s.recv(4096)
            if not chunk:
                raise ConnectionError("websocket handshake failed")
            resp += chunk
        head, self.buf = resp.split(b"\r\n\r\n", 1)
        if b" 101 " not in head.split(b"\r\n")[0]:
            raise ConnectionError(head.split(b"\r\n")[0].decode())

    def send(self, text):
        data = text.encode()
        hdr = bytearray([0x81])
        n = len(data)
        if n < 126:
            hdr.append(0x80 | n)
        elif n < 65536:
            hdr.append(0x80 | 126); hdr += struct.pack("!H", n)
        else:
            hdr.append(0x80 | 127); hdr += struct.pack("!Q", n)
        mask = os.urandom(4)
        hdr += mask
        self.s.sendall(bytes(hdr) + bytes(b ^ mask[i % 4] for i, b in enumerate(data)))

    def _read(self, n):
        while len(self.buf) < n:
            chunk = self.s.recv(65536)
            if not chunk:
                raise ConnectionError("websocket closed")
            self.buf += chunk
        out, self.buf = self.buf[:n], self.buf[n:]
        return out

    def recv(self):
        msg = b""
        while True:
            b0, b1 = self._read(2)
            op, n = b0 & 0x0F, b1 & 0x7F
            if n == 126:
                n = struct.unpack("!H", self._read(2))[0]
            elif n == 127:
                n = struct.unpack("!Q", self._read(8))[0]
            payload = self._read(n)
            if op == 0x8:
                raise ConnectionError("websocket closed by peer")
            if op in (0x9, 0xA):
                continue  # ping/pong
            msg += payload
            if b0 & 0x80:
                return msg.decode()


class CDP:
    def __init__(self, user_data_dir, wait=30):
        f = os.path.join(user_data_dir, "DevToolsActivePort")
        for _ in range(wait * 4):
            if os.path.exists(f) and len(open(f).read().split()) >= 2:
                break
            time.sleep(0.25)
        port, path = open(f).read().split()[:2]
        self.ws = WS("127.0.0.1", int(port), path)
        self.id = 0
        self.session = None

    def call(self, method, params=None, session=None, timeout=60):
        self.id += 1
        mid = self.id
        msg = {"id": mid, "method": method, "params": params or {}}
        if session:
            msg["sessionId"] = session
        self.ws.send(json.dumps(msg))
        end = time.time() + timeout
        while time.time() < end:
            m = json.loads(self.ws.recv())
            if m.get("id") == mid:
                if "error" in m:
                    raise RuntimeError(f"{method}: {m['error']}")
                return m.get("result", {})
        raise TimeoutError(method)

    def attach_extension_worker(self, ext_id, wait=30):
        url = f"chrome-extension://{ext_id}/background.js"
        for _ in range(wait * 2):
            t = [x for x in self.call("Target.getTargets")["targetInfos"]
                 if x["type"] == "service_worker" and x["url"] == url]
            if t:
                self.session = self.call("Target.attachToTarget",
                                         {"targetId": t[0]["targetId"], "flatten": True})["sessionId"]
                return
            time.sleep(0.5)
        raise TimeoutError("extension service worker not found")

    def eval(self, expr, timeout=60):
        r = self.call("Runtime.evaluate", {"expression": expr, "awaitPromise": True,
                                           "returnByValue": True}, session=self.session, timeout=timeout)
        if "exceptionDetails" in r:
            raise RuntimeError(r["exceptionDetails"].get("text", "evaluation failed"))
        return r.get("result", {}).get("value")
