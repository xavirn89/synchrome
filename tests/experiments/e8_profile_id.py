"""E8 — profile identification by pre-seeding mappingToken into Local Extension Settings.

Hypothesis: the app can write a `mappingToken` into `Local Extension Settings/<ext-id>/`
(a LevelDB store) BEFORE the browser first launches, and the extension reads it back via
chrome.storage.local, so the extension instance learns which Synchrome profile it maps to.

Procedure (Helium, disposable profile):
  1. compute the deterministic ext id from the manifest "key"
  2. create Default/Local Extension Settings/<id>/ and seed mappingToken with classic-level
     (external LevelDB writer, standing in for the Rust app)
  3. verify our own reader sees the seed
  4. launch the probe (same id); it reads chrome.storage.local.get('mappingToken') and records it
  5. read the probe's results off disk and confirm it read the seeded token

Usage: python3 e8_profile_id.py "<helium binary>" <label>
"""
import json, os, shutil, signal, struct, subprocess, sys, time

bin_, name = sys.argv[1], sys.argv[2]
base = os.path.dirname(os.path.abspath(__file__))
PROBE = os.path.join(base, "e678-ext")
udd = os.path.join(base, f"udd-e8-{name}")
shutil.rmtree(udd, ignore_errors=True)
EID = open("/tmp/e678_id.txt").read().strip()

TOKEN = {"token": "SEED-" + str(int(time.time())), "profileId": "11111111-2222-3333-4444-555555555555",
         "deviceId": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"}


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


def launch(secs):
    args = [bin_, f"--user-data-dir={udd}", "--headless=new", "--no-first-run",
            "--no-default-browser-check", "--disable-gpu", "--profile-directory=Default",
            f"--load-extension={PROBE}", f"--disable-extensions-except={PROBE}", "about:blank"]
    p = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(secs)
    p.send_signal(signal.SIGTERM); p.wait(25)


print(f"=== E8 {name}  (ext id {EID}) ===")
store_dir = os.path.join(udd, "Default", "Local Extension Settings", EID)
os.makedirs(store_dir, exist_ok=True)

# 2. seed with classic-level (value is the JSON of the JS value, as chrome.storage stores it)
value_json = json.dumps(TOKEN)
r = subprocess.run(["node", os.path.join(base, "e8_seed.js"), store_dir, value_json],
                   capture_output=True, text=True)
print("   seeder:", (r.stdout or r.stderr).strip())
if r.returncode != 0:
    print("   FAIL: seeding failed"); shutil.rmtree(udd, ignore_errors=True); sys.exit(1)

# 3. our reader sees the seed?
pre = read_store(store_dir)
seed_ok = pre.get("mappingToken") == value_json
print(f"   own reader sees seeded mappingToken: {seed_ok}  ({pre.get('mappingToken')})")

# 4. launch probe
launch(14)

# 5. did the extension read it?
post = read_store(store_dir)
res = json.loads(post.get("e678", "{}"))
read_val = res.get("e8_mappingToken")
got = read_val == TOKEN
print(f"   extension chrome.storage.local.get('mappingToken') -> {json.dumps(read_val)}")
print(f"   matches seeded token: {got}")
verdict = "PASS" if (seed_ok and got) else "FAIL"
print(f"   [{verdict}] app can pre-seed a token that the extension reads via chrome.storage.local")

shutil.rmtree(udd, ignore_errors=True)
print("(cleaned)")
