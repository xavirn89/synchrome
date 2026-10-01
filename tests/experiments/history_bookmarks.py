import json, os, signal, sqlite3, subprocess, sys, time, uuid
bin_, name = sys.argv[1], sys.argv[2]
base = os.path.dirname(os.path.abspath(__file__)); udd = os.path.join(base, f"udd-{name}"); prof = os.path.join(udd,"Default")
def run(secs, url="about:blank"):
    p = subprocess.Popen([bin_, f"--user-data-dir={udd}", "--headless=new", "--no-first-run", "--no-default-browser-check", "--disable-gpu", url], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(secs); p.send_signal(signal.SIGTERM); p.wait(20)
def webkit_now(): return int((time.time() + 11644473600) * 1_000_000)
print(f"=== {name}: history + bookmarks external edit ===")
run(12, "https://example.com/")   # generate one real visit so tables are populated
db = os.path.join(prof, "History")
con = sqlite3.connect(db); cur = con.cursor()
print("meta version:", cur.execute("select value from meta where key='version'").fetchone(), "| urls:", cur.execute("select count(*) from urls").fetchone()[0], "visits:", cur.execute("select count(*) from visits").fetchone()[0])
print("real visit row:", cur.execute("select transition, visit_duration, segment_id, incremented_omnibox_typed_score, visited_link_id, is_known_to_sync from visits limit 1").fetchone())
print("real url row:", cur.execute("select visit_count, typed_count, hidden from urls limit 1").fetchone())
t = webkit_now() - 3600*1_000_000
cur.execute("insert into urls(url,title,visit_count,typed_count,last_visit_time,hidden) values(?,?,?,?,?,0)", ("https://synchrome-test.invalid/page", "Synchrome inserted", 1, 0, t))
uid = cur.lastrowid
cur.execute("insert into visits(url,visit_time,from_visit,transition,segment_id,visit_duration) values(?,?,0,?,0,0)", (uid, t, 0x30000000 | 0))  # LINK | CHAIN_START|CHAIN_END
vid = cur.lastrowid
cur.execute("insert into visit_source(id,source) values(?,2)", (vid,))  # SOURCE_EXTENSION
con.commit(); con.close()
print("inserted url id", uid, "visit id", vid)
# Bookmarks: append a node without fixing checksum
bp=os.path.join(prof,"Bookmarks")
if not os.path.exists(bp):
    def root(i,g,n): return {"children":[],"date_added":str(webkit_now()),"date_modified":"0","guid":g,"id":str(i),"name":n,"type":"folder"}
    json.dump({"checksum":"deadbeef","version":1,"roots":{"bookmark_bar":root(1,"0bc5d13f-2cba-5d74-951f-3f233fe6c908","Bookmarks bar"),"other":root(2,"82b081ec-3dd3-529c-8475-ab6c344590dd","Other bookmarks"),"synced":root(3,"4cf2e351-0e85-532b-bb37-df045d8f8d0f","Mobile bookmarks")}},open(bp,"w"))
    print("  (synthesized Bookmarks file with bogus checksum)")
bm = json.load(open(bp))
old_checksum = bm["checksum"]
bm["roots"]["bookmark_bar"].setdefault("children", []).append({"date_added": str(webkit_now()), "date_last_used": "0", "guid": str(uuid.uuid4()), "id": "9999", "name": "Synchrome inserted bookmark", "type": "url", "url": "https://synchrome-test.invalid/bm"})
json.dump(bm, open(os.path.join(prof,"Bookmarks"),"w"))
run(15)
con = sqlite3.connect(db); cur = con.cursor()
print("AFTER relaunch: urls:", cur.execute("select count(*) from urls").fetchone()[0], "visits:", cur.execute("select count(*) from visits").fetchone()[0])
print("  inserted row still there:", cur.execute("select id,title,visit_count,last_visit_time from urls where url like 'https://synchrome-test%'").fetchall())
print("  integrity:", cur.execute("pragma integrity_check").fetchone()[0]); con.close()
bm2 = json.load(open(os.path.join(prof,"Bookmarks")))
kids = bm2["roots"]["bookmark_bar"].get("children", [])
print("  bookmarks bar children after relaunch:", [(c.get("name"), c.get("id"), c.get("guid")[:8]) for c in kids])
print("  checksum changed:", old_checksum != bm2["checksum"], "| Bookmarks.bak exists:", os.path.exists(os.path.join(prof,"Bookmarks.bak")))
