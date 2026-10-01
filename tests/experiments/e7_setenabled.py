"""E7 — can chrome.management.setEnabled change another extension from the service worker
without a user gesture, and does a dialog appear?

Hypothesis: setEnabled from the SW without a gesture is blocked (needs a user gesture /
shows a native confirmation), so Synchrome must route enable/disable through a popup click.

Procedure (Helium, disposable profile; Chrome 154 ignores --load-extension):
  load the probe + a dummy extension (both enabled); the probe tries to disable then
  re-enable the dummy from its service worker with no user gesture, recording each result
  and the dummy's enabled state. Results read off-disk from storage.local. Headless = no UI,
  so any dialog requirement manifests as a failure/timeout.

Usage: python3 e7_setenabled.py "<helium binary>" <label>
"""
import json, os, shutil, signal, struct, subprocess, sys, time

bin_, name = sys.argv[1], sys.argv[2]
base = os.path.dirname(os.path.abspath(__file__))
PROBE = os.path.join(base, "e678-ext")
DUMMY = os.path.join(base, "e678-dummy")
udd = os.path.join(base, f"udd-e7-{name}")
shutil.rmtree(udd, ignore_errors=True)
EID = open("/tmp/e678_id.txt").read().strip()


def launch(secs):
    args = [bin_, f"--user-data-dir={udd}", "--headless=new", "--no-first-run",
            "--no-default-browser-check", "--disable-gpu", "--profile-directory=Default",
            f"--load-extension={PROBE},{DUMMY}", f"--disable-extensions-except={PROBE},{DUMMY}",
            "about:blank"]
    p = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(secs)
    p.send_signal(signal.SIGTERM); p.wait(25)


def _vi(b, i):
    r = s = 0
    while True:
        c = b[i]; i += 1; r |= (c & 0x7f) << s; s += 7
        if not c & 0x80:
            return r, i


def read_store(path):
    kv = {}
    if not os.path.isdir(path):
        return kv
    for f in sorted(os.listdir(path)):
        if not f.endswith(".log"):
            continue
        data = open(os.path.join(path, f), "rb").read()
        off = 0; buf = b""; recs = []
        while off + 7 <= len(data):
            _c, ln, typ = struct.unpack("<IHB", data[off:off + 7]); off += 7
            if ln == 0 and typ == 0:
                off = (off // 32768 + 1) * 32768; continue
            frag = data[off:off + ln]; off += ln
            if typ == 1:
                recs.append(frag)
            elif typ == 2:
                buf = frag
            elif typ == 3:
                buf += frag
            elif typ == 4:
                buf += frag; recs.append(buf); buf = b""
            if off % 32768 > 32768 - 7:
                off = (off // 32768 + 1) * 32768
        for rec in recs:
            if len(rec) < 12:
                continue
            cnt = struct.unpack("<I", rec[8:12])[0]; i = 12
            try:
                for _ in range(cnt):
                    t = rec[i]; i += 1
                    kl, i = _vi(rec, i); k = rec[i:i + kl]; i += kl
                    if t == 1:
                        vl, i = _vi(rec, i); v = rec[i:i + vl]; i += vl
                        kv[k.decode("utf-8", "replace")] = v.decode("utf-8", "replace")
                    else:
                        kv.pop(k.decode("utf-8", "replace"), None)
            except IndexError:
                break
    return kv


print(f"=== E7 {name} ===")
launch(16)
res = json.loads(read_store(os.path.join(udd, "Default", "Local Extension Settings", EID)).get("e678", "{}"))
print("   dummy found:", res.get("e7_dummy_found"), "id:", res.get("e7_dummy_id"))
print("   enabled initial:", res.get("e7_enabled_initial"), "mayDisable:", res.get("e7_mayDisable"))
print("   disable: error =", res.get("e7_disable_error"), "| enabled after disable =", res.get("e7_enabled_after_disable"))
print("   enable:  error =", res.get("e7_enable_error"), "| enabled after enable  =", res.get("e7_enabled_after_enable"))

disable_worked = res.get("e7_enabled_after_disable") is False
enable_worked = res.get("e7_enabled_after_enable") is True
print(f"   SW disable without gesture worked: {disable_worked}")
print(f"   SW re-enable without gesture worked: {enable_worked}")
verdict = "PASS" if res.get("e7_dummy_found") else "FAIL (dummy not loaded)"
print(f"   [{verdict}] (interpretation in RESULTS.md)")

shutil.rmtree(udd, ignore_errors=True)
print("(cleaned)")
