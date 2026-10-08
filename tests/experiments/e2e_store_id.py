"""Companion identity after the Chrome Web Store listing — real host, disposable browsers.

The store assigned the companion the id `ceflmoojogokmklgcmcihdgakbefgiee`; the development
manifest now carries the store's public key, so an unpacked build has that same id. The app
keeps accepting the pre-store id `gmpmcnknoaogpjdoonlddhpdpgkebbog` (unpacked 1.0.0–1.0.2).
Checks, with the real debug binary as native host and no account (no cloud):
  1. the repo companion loaded in Helium has the store id and reaches the host (unmapped);
  2. the same in Google Chrome stable (DevTools-pipe side-load);
  3. a copy carrying the OLD key gets the old id and is still accepted by the host;
  4. a foreign origin is still rejected; the host manifest lists exactly the two origins.

  python3 e2e_store_id.py
"""
import json, os, shutil, subprocess, sys, tempfile, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import e2e_handshake as h  # noqa: E402
import e2e_chrome as ec  # noqa: E402
from cdp import CDP  # noqa: E402

check = h.check
STORE_ID = "ceflmoojogokmklgcmcihdgakbefgiee"
LEGACY_ID = "gmpmcnknoaogpjdoonlddhpdpgkebbog"
WORK = os.path.join(tempfile.gettempdir(), "synchrome-e2e-storeid")
DATA = os.path.join(WORK, "data")


def dev(*args):
    p = subprocess.run([h.APP, *args], capture_output=True, text=True, timeout=120, env={**os.environ, "SYNCHROME_DATA_DIR": DATA})
    try:
        return json.loads((p.stdout.strip().splitlines() or ["{}"])[-1])
    except json.JSONDecodeError:
        return {"error": f"rc={p.returncode}"}


def worker_status(c, ext_id, timeout=40):
    c.attach_extension_worker(ext_id)
    end = time.time() + timeout
    st = None
    while time.time() < end:
        st = c.eval("chrome.storage.local.get('synchromeStatus').then(r => r.synchromeStatus ?? null)")
        if st and st.get("status"):
            return st
        time.sleep(0.5)
    return st


def helium_with(udd, ext_dir, ext_id):
    try:
        os.remove(os.path.join(udd, "DevToolsActivePort"))
    except FileNotFoundError:
        pass
    p = subprocess.Popen([h.HELIUM, f"--user-data-dir={udd}", "--headless=new", "--no-first-run", "--no-default-browser-check",
                          "--disable-gpu", "--remote-debugging-port=0", f"--load-extension={ext_dir}",
                          f"--disable-extensions-except={ext_dir}", "about:blank"],
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env={**os.environ, "SYNCHROME_DATA_DIR": DATA})
    try:
        c = CDP(udd)
        ids = [t["url"].split("/")[2] for t in c.call("Target.getTargets")["targetInfos"] if t["url"].startswith("chrome-extension://")]
        for _ in range(40):
            if ext_id in ids:
                break
            time.sleep(0.5)
            ids = [t["url"].split("/")[2] for t in c.call("Target.getTargets")["targetInfos"] if t["url"].startswith("chrome-extension://")]
        st = worker_status(c, ext_id) if ext_id in ids else None
        return sorted(set(ids)), st
    finally:
        p.terminate()
        try:
            p.wait(20)
        except subprocess.TimeoutExpired:
            p.kill()


def main():
    print("=== companion identity (store id + pre-store id) ===")
    h.prepare_binary()
    shutil.rmtree(WORK, ignore_errors=True)
    os.makedirs(DATA)
    man = json.load(open(os.path.join(h.EXT, "manifest.json")))
    check("0 the development manifest carries the store key", ec.__name__ and man.get("key", "").startswith("MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA8mpk"), "")
    # no account is used: the disposable profiles carry no mapping token, so the host answers
    # token_missing before any cloud lookup (a signed-in session of the user is never touched)

    # 1 Helium, repo companion
    udd = os.path.join(WORK, "helium-store")
    os.makedirs(udd)
    m = dev("--dev-install-host", udd)
    origins = json.load(open(m["manifest"]))["allowed_origins"] if m.get("manifest") else []
    check("4 host manifest allows exactly the store id and the pre-store id",
          origins == [f"chrome-extension://{STORE_ID}/", f"chrome-extension://{LEGACY_ID}/"], json.dumps(origins))
    ids, st = helium_with(udd, h.EXT, STORE_ID)
    check("1 Helium: the repo companion has the STORE id and reaches the host (no token → unmapped)",
          STORE_ID in ids and (st or {}).get("status") == "token_missing" and (st or {}).get("ui") == "unmapped", json.dumps({"ids": ids, "status": (st or {}).get("status")}))

    # 3 Helium, pre-store copy (old key)
    old_key = json.loads(subprocess.run(["git", "show", "7b975e0:apps/extension/manifest.json"], capture_output=True, text=True,
                                        cwd=os.path.dirname(h.EXT)).stdout)["key"]
    legacy_dir = os.path.join(WORK, "companion-pre-store")
    shutil.copytree(h.EXT, legacy_dir)
    lm = dict(man, key=old_key)
    json.dump(lm, open(os.path.join(legacy_dir, "manifest.json"), "w"), indent=2)
    udd2 = os.path.join(WORK, "helium-legacy")
    os.makedirs(udd2)
    dev("--dev-install-host", udd2)
    ids, st = helium_with(udd2, legacy_dir, LEGACY_ID)
    check("3 Helium: a companion with the OLD key gets the pre-store id and is still accepted by the host (unmapped)",
          LEGACY_ID in ids and (st or {}).get("status") == "token_missing", json.dumps({"ids": ids, "status": (st or {}).get("status")}))

    # 2 Chrome stable, repo companion over the DevTools pipe
    udd3 = os.path.join(WORK, "chrome-store")
    os.makedirs(udd3)
    dev("--dev-install-host", udd3)
    c = ec.Chrome(udd3, env={"SYNCHROME_DATA_DIR": DATA}, extensions=[h.EXT])
    try:
        st = c.wait(lambda s: bool(s.get("status")), 40)
        check("2 Chrome stable: the repo companion gets the STORE id and reaches the host (unmapped)",
              c.loaded == [STORE_ID] and (st or {}).get("status") == "token_missing", json.dumps({"loaded": c.loaded, "status": (st or {}).get("status")}))
    finally:
        c.stop()

    # 4 foreign origin
    p = subprocess.run([h.APP, "chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/"], input=b"", capture_output=True, timeout=30,
                       env={**os.environ, "SYNCHROME_DATA_DIR": DATA})
    check("4b a foreign extension origin is still rejected (exit 2, no reply)", p.returncode == 2 and not p.stdout, f"rc={p.returncode}")
    log = open(os.path.join(DATA, "logs", "native-host.log")).read() if os.path.exists(os.path.join(DATA, "logs", "native-host.log")) else ""
    check("S host log has no tokens", h.secret_free(log), "")
    shutil.rmtree(WORK, ignore_errors=True)


if __name__ == "__main__":
    try:
        main()
    except Exception as ex:  # noqa: BLE001
        import traceback
        traceback.print_exc()
        check("harness completed without exception", False, type(ex).__name__ + ": " + str(ex)[:200])
    failed = [n for n, ok in h.results if not ok]
    print(f"\n=== store id: {len(h.results) - len(failed)} pass, {len(failed)} fail ===")
    sys.exit(1 if failed else 0)
