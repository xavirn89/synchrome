"""E5 — Chromium history: format, closed-profile read, copy/restore, cross-profile import.

On disposable profiles only. Steps:
  1. Generate a real visit, inspect the DB: is it SQLite? journal mode? schema version? tables?
  2. Read with the browser closed; probe the lock while the browser is running.
  3. Insert a visit into a closed profile (urls+visits+visit_source, counters maintained).
  4. Copy the History DB into a SECOND disposable profile (closed) and relaunch → the imported
     rows must be present and intact (integrity_check).
No real profile is touched.

Usage: python3 e5_history.py "<browser binary>" <label>
"""
import os, shutil, signal, sqlite3, subprocess, sys, time

bin_, name = sys.argv[1], sys.argv[2]
base = os.path.dirname(os.path.abspath(__file__))
src = os.path.join(base, f"udd-e5-{name}-src")
dst = os.path.join(base, f"udd-e5-{name}-dst")
for d in (src, dst):
    shutil.rmtree(d, ignore_errors=True)


def launch(udd, secs, url="about:blank"):
    p = subprocess.Popen([bin_, f"--user-data-dir={udd}", "--headless=new", "--no-first-run",
                          "--no-default-browser-check", "--disable-gpu", "--profile-directory=Default", url],
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(secs)
    p.send_signal(signal.SIGTERM)
    p.wait(25)
    return p


def webkit_now():
    return int((time.time() + 11644473600) * 1_000_000)


def hist(udd):
    return os.path.join(udd, "Default", "History")


print(f"=== E5 {name} ===")
launch(src, 14, "https://example.com/")
db = hist(src)

# 1. Format
print("\n-- format --")
is_sqlite = open(db, "rb").read(16).startswith(b"SQLite format 3")
print(f"   magic 'SQLite format 3': {is_sqlite}")
con = sqlite3.connect(db); cur = con.cursor()
ver = cur.execute("select value from meta where key='version'").fetchone()
jmode = cur.execute("pragma journal_mode").fetchone()[0]
tables = [r[0] for r in cur.execute("select name from sqlite_master where type='table' order by name")]
print(f"   meta.version: {ver[0] if ver else '?'} | journal_mode (ro connection): {jmode}")
print(f"   relevant tables: {[t for t in tables if t in ('urls','visits','visit_source','segments','segment_usage','keyword_search_terms','meta','downloads')]}")
print(f"   -wal present: {os.path.exists(db + '-wal')} | -journal present: {os.path.exists(db + '-journal')}")
u0 = cur.execute("select count(*) from urls").fetchone()[0]
v0 = cur.execute("select count(*) from visits").fetchone()[0]
print(f"   rows: urls={u0} visits={v0}")
con.close()

# 2. Lock while running
print("\n-- lock while browser running --")
p = subprocess.Popen([bin_, f"--user-data-dir={src}", "--headless=new", "--no-first-run",
                      "--no-default-browser-check", "--disable-gpu", "--profile-directory=Default", "about:blank"],
                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(6)
try:
    c = sqlite3.connect(f"file:{db}?mode=ro", uri=True, timeout=2)
    c.execute("select count(*) from urls").fetchone()
    locked = False
    c.close()
except sqlite3.OperationalError as e:
    locked = "locked" in str(e).lower()
# copy-while-running integrity
shutil.copy(db, db + ".livecopy")
try:
    cc = sqlite3.connect(db + ".livecopy")
    integ_live = cc.execute("pragma integrity_check").fetchone()[0]
    cc.close()
except Exception as e:
    integ_live = f"error: {e}"
os.remove(db + ".livecopy")
p.send_signal(signal.SIGTERM); p.wait(25)
print(f"   direct read-only open while running: {'LOCKED (database is locked)' if locked else 'succeeded'}")
print(f"   integrity of a copy taken while running: {integ_live}")

# 3. Insert a visit into the closed profile, counters maintained
print("\n-- insert visit (browser closed) --")
con = sqlite3.connect(db); cur = con.cursor()
t = webkit_now() - 3600 * 1_000_000
url = "https://synchrome-e5.invalid/imported"
cur.execute("insert into urls(url,title,visit_count,typed_count,last_visit_time,hidden) values(?,?,1,0,?,0)",
            (url, "E5 imported page", t))
uid = cur.lastrowid
cur.execute("insert into visits(url,visit_time,from_visit,transition,segment_id,visit_duration,is_known_to_sync) "
            "values(?,?,0,?,0,0,0)", (uid, t, 0x30000000))  # CHAIN_START|CHAIN_END|LINK
vid = cur.lastrowid
cur.execute("insert into visit_source(id,source) values(?,2)", (vid,))  # SOURCE_EXTENSION
con.commit()
print(f"   inserted url id {uid}, visit id {vid}, integrity: {cur.execute('pragma integrity_check').fetchone()[0]}")
con.close()
launch(src, 12)  # relaunch to confirm Chromium accepts it
con = sqlite3.connect(db); cur = con.cursor()
present = cur.execute("select title,visit_count from urls where url=?", (url,)).fetchone()
print(f"   after relaunch: inserted row present = {present is not None} {present}")
print(f"   integrity after relaunch: {cur.execute('pragma integrity_check').fetchone()[0]}")
con.close()

# 4. Copy History into a second profile (both closed), import
print("\n-- import History into a second profile (closed) --")
launch(dst, 12)  # create profile with its own (near-empty) history
ddb = hist(dst)
d_before = sqlite3.connect(ddb).execute("select count(*) from urls").fetchone()[0]
# wholesale replace while closed (back up first)
shutil.copy(ddb, ddb + ".bak")
for ext in ("", "-wal", "-journal"):
    s = db + ext
    if os.path.exists(s):
        shutil.copy(s, ddb + ext)
launch(dst, 12)
con = sqlite3.connect(ddb); cur = con.cursor()
imported = cur.execute("select title from urls where url=?", (url,)).fetchone()
d_after = cur.execute("select count(*) from urls").fetchone()[0]
integ = cur.execute("pragma integrity_check").fetchone()[0]
con.close()
ok = imported is not None and integ == "ok"
print(f"   dst urls before import: {d_before}, after: {d_after}")
print(f"   imported row visible in dst: {imported is not None} {imported}")
print(f"   dst integrity after import + relaunch: {integ}")
print(f"   [{'PASS' if ok else 'FAIL'}] history copy/restore + cross-profile import")

for d in (src, dst):
    shutil.rmtree(d, ignore_errors=True)
print("(cleaned throwaway profiles)")
