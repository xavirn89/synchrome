"""macOS Full Disk Access — positive and negative case with the RELEASE signing identity.

The disposable Chrome profile lives INSIDE Chrome's TCC-protected folder
(`~/Library/Application Support/Google/Chrome/Synchrome-E2E-disposable`): Chrome itself creates
and populates it (launched as its own TCC-responsible process, DevTools pipe for the fixtures),
so no process of this harness ever reads it. Synchrome's access is then measured with the
Developer-ID-signed DEBUG bundle (same bundle id + Team ID as /Applications/Synchrome.app, so a
Full Disk Access grant to the release app covers it) launched through `open` (own TCC identity):
without the grant every read is `requires_full_disk_access`; with it, Preferences, History
(read-only) and the fixture's extension settings are readable. Counts, states and key names only.

  python3 e2e_fda.py populate     # Chrome fills the disposable profile (prefs, history, dummy storage, a bookmark)
  python3 e2e_fda.py verify       # Synchrome (release identity) reads it — run before AND after the FDA grant
"""
import http.server, json, os, select, signal, subprocess, sys, threading, time, urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import e2e_handshake as h  # noqa: E402
import e2e_extensions as xe  # noqa: E402
import tccspawn  # noqa: E402

check = h.check
ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
UDD = os.path.expanduser("~/Library/Application Support/Google/Chrome/Synchrome-E2E-disposable")
DBG = os.path.join(ROOT, "target/debug/bundle/macos/Synchrome.app")
SP = os.environ.get("E2E_SCRATCH", "/tmp/synchrome-e2e")
DATA = os.path.join(SP, "fda-data")
PORT = 9333
DUMMY = xe.DUMMY


class PipeChrome:
    def __init__(self, udd, extensions=()):
        r_in, self.w_in = os.pipe()
        self.r_out, w_out = os.pipe()
        self.err = open(os.path.join(SP, "fda-chrome.err"), "w")
        self.pid = tccspawn.spawn_disclaimed(
            [CHROME, f"--user-data-dir={udd}", "--headless=new", "--no-first-run", "--no-default-browser-check",
             "--disable-gpu", "--remote-debugging-pipe", f"--remote-debugging-port={PORT}",
             "--enable-unsafe-extension-debugging", "--disable-background-networking", "about:blank"],
            dup_fds=[(r_in, 3), (w_out, 4), (self.err.fileno(), 2)])
        os.close(r_in)
        os.close(w_out)
        self.n = 0
        self.buf = b""
        time.sleep(4)
        self.call("Browser.getVersion")
        self.ids = [(self.call("Extensions.loadUnpacked", {"path": e}).get("result") or {}).get("id") for e in extensions]
        time.sleep(2)

    def call(self, method, params=None, timeout=30):
        self.n += 1
        os.write(self.w_in, (json.dumps({"id": self.n, "method": method, "params": params or {}}) + "\0").encode())
        end = time.time() + timeout
        while time.time() < end:
            r, _, _ = select.select([self.r_out], [], [], 1)
            if not r:
                continue
            ch = os.read(self.r_out, 65536)
            if not ch:
                raise RuntimeError("pipe closed")
            self.buf += ch
            while b"\0" in self.buf:
                msg, self.buf = self.buf.split(b"\0", 1)
                m = json.loads(msg)
                if m.get("id") == self.n:
                    return m
        raise TimeoutError(method)

    def http(self, path):
        return json.load(urllib.request.urlopen(f"http://127.0.0.1:{PORT}{path}", timeout=5))

    def eval_in(self, url, expr, wait_ok=None, tries=30):
        """Create a page target at `url`, evaluate `expr` there (awaiting promises)."""
        t = self.call("Target.createTarget", {"url": url})["result"]["targetId"]
        s = self.call("Target.attachToTarget", {"targetId": t, "flatten": True})["result"]["sessionId"]
        v = None
        for _ in range(tries):
            time.sleep(0.4)
            if wait_ok:
                ok = self.send_s(s, "Runtime.evaluate", {"expression": wait_ok, "returnByValue": True})
                if not ok.get("result", {}).get("result", {}).get("value"):
                    continue
            r = self.send_s(s, "Runtime.evaluate", {"expression": expr, "awaitPromise": True, "returnByValue": True})
            if "exceptionDetails" not in r.get("result", {}):
                v = r.get("result", {}).get("result", {}).get("value")
                break
        self.call("Target.closeTarget", {"targetId": t})
        return v

    def send_s(self, session, method, params):
        self.n += 1
        os.write(self.w_in, (json.dumps({"id": self.n, "sessionId": session, "method": method, "params": params}) + "\0").encode())
        end = time.time() + 30
        while time.time() < end:
            r, _, _ = select.select([self.r_out], [], [], 1)
            if not r:
                continue
            ch = os.read(self.r_out, 65536)
            if not ch:
                raise RuntimeError("pipe closed")
            self.buf += ch
            while b"\0" in self.buf:
                msg, self.buf = self.buf.split(b"\0", 1)
                m = json.loads(msg)
                if m.get("id") == self.n:
                    return m
        raise TimeoutError(method)

    def stop(self):
        time.sleep(4)  # let Chromium commit its files
        os.kill(self.pid, signal.SIGTERM)
        for _ in range(60):
            if os.waitpid(self.pid, os.WNOHANG) != (0, 0):
                break
            time.sleep(0.5)
        self.err.close()


def populate():
    print("=== FDA: Chrome populates the disposable profile inside its protected folder ===")
    os.makedirs(SP, exist_ok=True)
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), http.server.SimpleHTTPRequestHandler)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    base = f"http://127.0.0.1:{srv.server_address[1]}"
    c = PipeChrome(UDD, extensions=[h.EXT, xe.DUMMY_DIR])
    check("P.1 Chrome (own TCC identity) created the nested disposable data folder and loaded the companion + dummy fixture",
          c.ids == [h.EXT_ID, DUMMY], json.dumps(c.ids))
    v = c.eval_in(f"chrome-extension://{DUMMY}/page.html", "chrome.storage.local.set({fixture: 'fda-nested', round: 1}).then(() => chrome.storage.local.get(null)).then(o => Object.keys(o).sort())")
    check("P.2 the dummy wrote its extension settings (keys)", v == ["fixture", "round"], json.dumps(v))
    v = c.eval_in("chrome://settings/", "Promise.all([['download.prompt_for_download', true], ['intl.selected_languages', 'es,en']].map(([k, x]) => new Promise(r => chrome.settingsPrivate.setPref(k, x, '', ok => r(ok)))))",
                  wait_ok="!!(window.chrome && chrome.settingsPrivate)")
    check("P.3 two allow-listed preferences set through Chrome's settings page", v == [True, True], json.dumps(v))
    for i in range(3):
        t = c.call("Target.createTarget", {"url": f"{base}/?fda={i}"})["result"]["targetId"]
        time.sleep(1.2)
        c.call("Target.closeTarget", {"targetId": t})
    hist = c.eval_in(f"chrome-extension://{h.EXT_ID}/popup.html", "chrome.history.search({text: '', maxResults: 50}).then(r => r.length)") if False else None
    bm = c.eval_in(f"chrome-extension://{h.EXT_ID}/popup.html",
                   "(async () => { const bar = (await chrome.bookmarks.getTree())[0].children.find(x => x.folderType === 'bookmarks-bar' || x.id === '1'); await chrome.bookmarks.create({parentId: bar.id, title: 'FDA probe', url: 'https://example.com/fda'}); return (await chrome.bookmarks.getChildren(bar.id)).length; })()")
    check("P.4 three local pages visited (History rows) and one bookmark created", bm == 1, json.dumps({"bar_children": bm}))
    c.stop()
    srv.shutdown()
    try:
        os.listdir(UDD)
        check("P.5 the harness shell itself cannot list the disposable folder (TCC)", False, "listable")
    except OSError as e:
        check("P.5 the harness shell itself cannot list the disposable folder (TCC)", e.errno == 1, e.strerror)


def history():
    """Fixture: Chrome records three visits to local pages and commits them (graceful close)."""
    print("=== FDA: Chrome records history in the disposable profile ===")
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), http.server.SimpleHTTPRequestHandler)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    base = f"http://127.0.0.1:{srv.server_address[1]}"
    c = PipeChrome(UDD)
    for i in range(3):
        t = c.call("Target.createTarget", {"url": f"{base}/?visit={i}"})["result"]["targetId"]
        time.sleep(2)
        c.call("Target.closeTarget", {"targetId": t})
    time.sleep(15)  # HistoryBackend commits in batches
    c.call("Browser.close")
    for _ in range(60):
        if os.waitpid(c.pid, os.WNOHANG) != (0, 0):
            break
        time.sleep(0.5)
    c.err.close()
    srv.shutdown()
    check("H.1 Chrome closed gracefully after recording the visits", True, "")


def dev(*args):
    """Run the Developer-ID-signed debug bundle as its own TCC-responsible process."""
    out = os.path.join(SP, f"fda-{int(time.time() * 1000)}.json")
    env = {}
    for line in open(os.path.join(ROOT, ".env")):
        k, _, v = line.strip().partition("=")
        if k in ("SUPABASE_URL", "SUPABASE_ANON_KEY"):
            env[k] = v.strip().strip('"')
    cmd = ["open", "-W", "--stdout", out, "--env", f"SYNCHROME_DATA_DIR={DATA}", "--env", "SYNCHROME_DEV_EXTRA_DIRS_ONLY=1"]
    for k, v in env.items():
        cmd += ["--env", f"{k}={v}"]
    cmd += ["-a", DBG, "--args", *args]
    subprocess.run(cmd, check=True, timeout=120)
    try:
        return json.loads(open(out).read().strip().splitlines()[-1])
    finally:
        os.remove(out)


def verify():
    print("=== FDA: Synchrome (release signing identity) reads the disposable profile ===")
    os.makedirs(DATA, exist_ok=True)
    json.dump([{"kind": "chrome", "user_data_dir": UDD, "label": "E2E disposable (nested)"}], open(os.path.join(DATA, "browsers.json"), "w"))
    sig = subprocess.run(["codesign", "-dv", DBG], capture_output=True, text=True).stderr
    rel = subprocess.run(["codesign", "-dv", "/Applications/Synchrome.app"], capture_output=True, text=True).stderr
    ids = lambda s: {l.split("=")[0]: l.split("=", 1)[1] for l in s.splitlines() if l.startswith(("Identifier=", "TeamIdentifier="))}
    check("V.0 the probing bundle has the SAME identifier and Team ID as /Applications/Synchrome.app (the TCC grant target)", ids(sig) == ids(rel) and ids(rel).get("TeamIdentifier"), json.dumps(ids(rel)))
    cd = lambda path: next((l.split("=", 1)[1] for l in subprocess.run(["codesign", "-dvvv", path], capture_output=True, text=True).stderr.splitlines() if l.startswith("CDHash=")), None)
    M_cd = {"probe_bundle": cd(DBG), "release_app": cd("/Applications/Synchrome.app")}
    bs = {b["display_name"]: b for b in dev("--dev-browsers")["browsers"]}
    nested = bs.get("Google Chrome — E2E disposable (nested)", {})
    check("V.0b only the approved disposable folder is enumerated (the harness never lists or reads the real browser folders)",
          list(bs) == ["Google Chrome — E2E disposable (nested)"], json.dumps(sorted(bs)))
    granted = nested.get("access") == "accessible"
    print(f"  access: nested={nested.get('access')} profiles={nested.get('profiles')}")
    if not granted:
        check("V.neg without Full Disk Access: the nested disposable folder is 'requires_full_disk_access', 0 profiles read",
              nested.get("access") == "requires_full_disk_access" and nested.get("profiles") == 0, "")
        print("  (grant Full Disk Access to /Applications/Synchrome.app and run `verify` again)")
        return
    check("V.1 with Full Disk Access: the nested disposable folder is accessible with its 1 profile (version readable)",
          nested.get("profiles") == 1 and bool(nested.get("version")), json.dumps({"version": nested.get("version")}))
    check("V.2 the grant follows the signing identity, not the binary: a DIFFERENT build (other CDHash) with the same Team ID + bundle id is covered",
          M_cd["probe_bundle"] and M_cd["release_app"] and M_cd["probe_bundle"] != M_cd["release_app"], json.dumps({k: (v or "")[:12] for k, v in M_cd.items()}))
    p = dev("--dev-read-prefs", UDD, "Default", "chrome")
    vals = p.get("values") or {}
    check("V.3 Preferences of the disposable profile read (closed): the two values set by Chrome",
          vals.get("download.prompt_for_download") is True and vals.get("intl.selected_languages") == "es,en", json.dumps({k: vals.get(k) for k in ("download.prompt_for_download", "intl.selected_languages")}))
    hst = dev("--dev-read-history", UDD, "Default")
    check("V.4 History SQLite opened strictly read-only (immutable URI): schema version and rows counted, 3 visits recorded",
          hst.get("readable") is True and hst.get("schema_version", 0) >= 60 and hst.get("urls", 0) >= 1 and hst.get("visits", 0) >= 3, json.dumps(hst))
    kv = dev("--dev-read-ext-area", UDD, "Default", DUMMY, "local").get("keys") or {}
    check("V.5 the fixture's extension settings (LevelDB) read with the browser closed: keys fixture, round", set(kv) == {"fixture", "round"}, json.dumps(sorted(kv)))
    try:
        open(os.path.join(UDD, "Default", "Preferences"), "rb").close()
        denied = False
    except PermissionError:
        denied = True
    check("V.6 the harness shell still cannot READ the folder (the grant is Synchrome's, not this shell's; stat is allowed by TCC)", denied, "")
    # re-sign the probe bundle with the same Developer ID (a new signature / timestamp)
    ident = "Developer ID Application: Xavi Ramon Nicolau (YV44FR3LG3)"
    r = subprocess.run(["codesign", "--force", "--deep", "--options", "runtime", "--timestamp", "-s", ident, DBG], capture_output=True, text=True)
    bs2 = {b["display_name"]: b for b in dev("--dev-browsers")["browsers"]}
    check("V.7 after re-signing with the same Developer ID + bundle id the grant still applies",
          r.returncode == 0 and bs2.get("Google Chrome — E2E disposable (nested)", {}).get("access") == "accessible",
          json.dumps({"resign_rc": r.returncode, "access": bs2.get("Google Chrome — E2E disposable (nested)", {}).get("access")}))


if __name__ == "__main__":
    mode = sys.argv[1] if len(sys.argv) > 1 else "verify"
    try:
        {"populate": populate, "history": history}.get(mode, verify)()
    except Exception as ex:  # noqa: BLE001
        import traceback
        traceback.print_exc()
        check("harness completed without exception", False, type(ex).__name__ + ": " + str(ex)[:200])
    failed = [n for n, ok in h.results if not ok]
    print(f"\n=== fda {mode}: {len(h.results) - len(failed)} pass, {len(failed)} fail ===")
    if failed:
        print("  failures: " + ", ".join(failed))
    sys.exit(1 if failed else 0)
