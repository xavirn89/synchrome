"""E4 — Extension storage in LevelDB: read, copy, restore across disposable profiles.

Loads a write-once probe extension into a throwaway profile, lets it write each storage
area, quits, reads the on-disk LevelDB with a pure-Python reader, then copies the storage
dirs into a SECOND throwaway profile (both browsers closed) and verifies the extension
reads the restored values back (the probe does not overwrite an existing store).

Requires a browser that honours --load-extension (Helium/Chromium; Google Chrome stable
154 disables CLI extension loading — see RESULTS). No real profile is touched.

Usage: python3 e4_leveldb.py "<browser binary>" <label>
"""
import json, os, shutil, signal, struct, subprocess, sys, time

bin_, name = sys.argv[1], sys.argv[2]
base = os.path.dirname(os.path.abspath(__file__))
EXT = os.path.join(base, "e4-ext")
src = os.path.join(base, f"udd-e4-{name}-src")
dst = os.path.join(base, f"udd-e4-{name}-dst")
for d in (src, dst):
    shutil.rmtree(d, ignore_errors=True)


def launch(udd, secs):
    args = [bin_, f"--user-data-dir={udd}", "--headless=new", "--no-first-run",
            "--no-default-browser-check", "--disable-gpu",
            f"--load-extension={EXT}", f"--disable-extensions-except={EXT}",
            "--profile-directory=Default", "about:blank"]
    p = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(secs)
    p.send_signal(signal.SIGTERM)
    p.wait(25)


def _varint(b, i):
    r = s = 0
    while True:
        c = b[i]; i += 1; r |= (c & 0x7f) << s; s += 7
        if not c & 0x80:
            return r, i


def read_log(path):
    data = open(path, "rb").read()
    off = 0; buf = b""; recs = []
    while off + 7 <= len(data):
        _crc, ln, typ = struct.unpack("<IHB", data[off:off + 7]); off += 7
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
    out = []
    for rec in recs:
        if len(rec) < 12:
            continue
        cnt = struct.unpack("<I", rec[8:12])[0]; i = 12
        try:
            for _ in range(cnt):
                t = rec[i]; i += 1
                kl, i = _varint(rec, i); k = rec[i:i + kl]; i += kl
                if t == 1:
                    vl, i = _varint(rec, i); v = rec[i:i + vl]; i += vl
                    out.append(("put", k, v))
                else:
                    out.append(("del", k, None))
        except IndexError:
            break
    return out


def dump_store(path):
    kv = {}
    if not os.path.isdir(path):
        return kv
    for f in sorted(os.listdir(path)):
        if f.endswith(".log"):
            for op, k, v in read_log(os.path.join(path, f)):
                key = k.decode("utf-8", "replace")
                kv[key] = "<del>" if op == "del" else v.decode("utf-8", "replace")
    return kv


def settings_dir(udd, area):
    return os.path.join(udd, "Default", area)


def find_probe_id(udd):
    """Return the ext id whose Local Extension Settings store contains our e4_local_key."""
    les = settings_dir(udd, "Local Extension Settings")
    if not os.path.isdir(les):
        return None
    for eid in sorted(os.listdir(les)):
        if "e4_local_key" in dump_store(os.path.join(les, eid)):
            return eid
    return None


print(f"=== E4 {name} ===")
launch(src, 18)
EID = find_probe_id(src)
print("probe extension id:", EID)
if not EID:
    print("FAIL: probe did not load/write (does this browser honour --load-extension?)")
    for d in (src, dst):
        shutil.rmtree(d, ignore_errors=True)
    sys.exit(1)

print("\n-- storage areas on disk (src) --")
areas = {
    "storage.local  (Local Extension Settings)": os.path.join(settings_dir(src, "Local Extension Settings"), EID),
    "storage.sync   (Sync Extension Settings)": os.path.join(settings_dir(src, "Sync Extension Settings"), EID),
}
for label, p in areas.items():
    d = dump_store(p)
    print(f"   {label}: {'present' if os.path.isdir(p) else 'ABSENT'}")
    for k, v in d.items():
        print(f"       {k} = {v[:100]}")

idb = settings_dir(src, "IndexedDB")
idb_ours = [d for d in (os.listdir(idb) if os.path.isdir(idb) else []) if EID in d]
print(f"   IndexedDB (extension origin): {idb_ours}")
ls = settings_dir(src, "Local Storage")
print(f"   Local Storage/leveldb present: {os.path.isdir(os.path.join(ls, 'leveldb'))} (shared, binary keys)")
print(f"   storage.session: in-memory only — no on-disk dir (by design)")

# ---- copy/restore into a fresh profile, both closed ----
print("\n-- restore into a second profile (browsers closed) --")
launch(dst, 15)                      # fresh install; writes its OWN values (different stamp)
EID_DST = find_probe_id(dst)
src_stamp = json.loads(dump_store(os.path.join(settings_dir(src, "Local Extension Settings"), EID))["e4_local_key"])["stamp"]
dst_stamp_before = json.loads(dump_store(os.path.join(settings_dir(dst, "Local Extension Settings"), EID_DST))["e4_local_key"])["stamp"]
print(f"   src stamp {src_stamp}  |  dst stamp before restore {dst_stamp_before}  (same id: {EID == EID_DST})")

restored = []
for area in ("Local Extension Settings", "Sync Extension Settings"):
    s = os.path.join(settings_dir(src, area), EID)
    d = os.path.join(settings_dir(dst, area), EID_DST)
    if os.path.isdir(s):
        shutil.rmtree(d, ignore_errors=True)
        shutil.copytree(s, d)
        restored.append(area)
# IndexedDB for the extension origin
for name_ in idb_ours:
    s = os.path.join(idb, name_)
    d = os.path.join(settings_dir(dst, "IndexedDB"), name_.replace(EID, EID_DST))
    if os.path.isdir(s):
        shutil.rmtree(d, ignore_errors=True)
        shutil.copytree(s, d)
        restored.append("IndexedDB")
print("   restored:", restored)

launch(dst, 12)                      # probe is write-once → should NOT overwrite; records readback
dst_after = dump_store(os.path.join(settings_dir(dst, "Local Extension Settings"), EID_DST))
readback = json.loads(dst_after.get("e4_readback", "{}")) if "e4_readback" in dst_after else {}
dst_stamp_after = json.loads(dst_after["e4_local_key"])["stamp"] if "e4_local_key" in dst_after else None
ok = dst_stamp_after == src_stamp and readback.get("readStamp") == src_stamp
print(f"   dst storage.local stamp after restore: {dst_stamp_after}")
print(f"   extension readback stamp: {readback.get('readStamp')} (sync: {readback.get('syncStamp')})")
print(f"   [{'PASS' if ok else 'FAIL'}] restored store adopted and read by the extension in the new profile")

for d in (src, dst):
    shutil.rmtree(d, ignore_errors=True)
print("(cleaned throwaway profiles)")
