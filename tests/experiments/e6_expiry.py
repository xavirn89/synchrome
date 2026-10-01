"""E6 — does Chromium's 90-day history expiry fire history.onVisitRemoved?

Hypothesis: expiry deletes rows but does NOT surface as onVisitRemoved (so Synchrome must
not treat the disappearance of old visits as a user deletion to propagate).

Procedure (disposable profiles):
  1. launch probe once -> profile + empty History created
  2. closed-insert: 2 recent visits (keep) + 3 visits dated ~120 days ago (should expire)
  3. relaunch with the probe (listening to onVisitRemoved); wait for the startup expirer
  4. read the probe's results from storage.local (off-disk) + inspect the History DB
Helium runs the extension; Chrome validates DB-level expiry only (no --load-extension).

Usage: python3 e6_expiry.py "<browser>" <label> [--with-ext]
"""
import json, os, shutil, signal, sqlite3, struct, subprocess, sys, time

bin_, name = sys.argv[1], sys.argv[2]
WITH_EXT = "--with-ext" in sys.argv
base = os.path.dirname(os.path.abspath(__file__))
EXT = os.path.join(base, "e678-ext")
udd = os.path.join(base, f"udd-e6-{name}")
shutil.rmtree(udd, ignore_errors=True)
EID = open("/tmp/e678_id.txt").read().strip()


def launch(secs):
    args = [bin_, f"--user-data-dir={udd}", "--headless=new", "--no-first-run",
            "--no-default-browser-check", "--disable-gpu", "--profile-directory=Default"]
    if WITH_EXT:
        args += [f"--load-extension={EXT}", f"--disable-extensions-except={EXT}"]
    args.append("about:blank")
    p = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(secs)
    p.send_signal(signal.SIGTERM); p.wait(25)


def webkit(days_ago):
    return int((time.time() - days_ago * 86400 + 11644473600) * 1_000_000)


def hist():
    return os.path.join(udd, "Default", "History")


# --- compact LevelDB .log reader (keys plain, values JSON) ---
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


print(f"=== E6 {name} (with-ext={WITH_EXT}) ===")
launch(12)
con = sqlite3.connect(hist()); cur = con.cursor()


def add_visit(url, title, days_ago):
    t = webkit(days_ago)
    cur.execute("insert into urls(url,title,visit_count,typed_count,last_visit_time,hidden) values(?,?,1,0,?,0)",
                (url, title, t))
    uid = cur.lastrowid
    cur.execute("insert into visits(url,visit_time,from_visit,transition,segment_id,visit_duration,is_known_to_sync) "
                "values(?,?,0,?,0,0,0)", (uid, t, 0x30000000))
    cur.execute("insert into visit_source(id,source) values(?,2)", (cur.lastrowid,))


add_visit("https://synchrome-e6.invalid/recent1", "recent1", 1)
add_visit("https://synchrome-e6.invalid/recent2", "recent2", 2)
for i in range(3):
    add_visit(f"https://synchrome-e6.invalid/old{i}", f"old{i}", 120)
con.commit()
total_before = cur.execute("select count(*) from urls").fetchone()[0]
old_before = cur.execute("select count(*) from urls where title like 'old%'").fetchone()[0]
con.close()
print(f"   inserted: {total_before} urls total, {old_before} dated ~120 days ago")

launch(40)   # give the startup expirer time to run
con = sqlite3.connect(hist()); cur = con.cursor()
old_after = cur.execute("select count(*) from urls where title like 'old%'").fetchone()[0]
recent_after = cur.execute("select count(*) from urls where title like 'recent%'").fetchone()[0]
print(f"   after relaunch: old rows remaining = {old_after} (were {old_before}); recent remaining = {recent_after}")
expired = old_after < old_before
print(f"   DB-level expiry occurred: {expired}")
con.close()

if WITH_EXT:
    store = read_store(os.path.join(udd, "Default", "Local Extension Settings", EID))
    res = json.loads(store.get("e678", "{}"))
    evs = res.get("e6_removed_events", [])
    print(f"   probe e6_removed_events: {evs}")
    print(f"   probe history_count seen: {res.get('e6_history_count')}")
    fired = len(evs) > 0
    print(f"   onVisitRemoved fired during run: {fired}")
    verdict = "PASS" if (expired and not fired) else ("PARTIAL" if expired else "FAIL")
    print(f"   [{verdict}] expiry {'deletes without firing onVisitRemoved' if (expired and not fired) else 'behaviour: see above'}")

shutil.rmtree(udd, ignore_errors=True)
print("(cleaned)")
