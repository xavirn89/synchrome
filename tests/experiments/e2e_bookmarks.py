"""End-to-end validation of bookmarks capture + upload-only publication (M1 slice 7).

Requires migration 0004. Real debug binary (byte-identical copy outside ~/Documents, see
e2e_handshake.py), real extension in a DISPOSABLE Helium --user-data-dir, live edits through
CDP inside the extension's own service worker (chrome.bookmarks), Synchrome Cloud through
A's / B's normal sessions (anon key + own JWT; no service role). Prints ids, counts, field
NAMES, titles of the known test fixture, URL hosts and 8-hex URL hashes only — never
tokens, JWTs, passwords or full URLs.

  EXPECTED_A_ID=… EXPECTED_B_ID=… python3 e2e_bookmarks.py <creds-env-file>
"""
import json, os, signal, sqlite3, struct, subprocess, sys, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import e2e_handshake as h  # noqa: E402
from cdp import CDP  # noqa: E402

check, dev = h.check, h.dev
EXT_ID = h.EXT_ID
OFFLINE_URL = "https://127.0.0.1:9"  # nothing listens: connection refused immediately (config requires https)
APPDATA = os.path.expanduser("~/Library/Application Support/Synchrome")
STATE_DB = os.path.join(APPDATA, "state.sqlite")
latencies = {}
RUN = {"helium": None, "mid": None, "creds": None, "udd": None}  # for emergency cleanup


class Helium:
    """A running disposable Helium with the extension, attached over CDP to its worker."""

    def __init__(self, udd, env=None):
        self.udd = udd
        try:
            os.remove(os.path.join(udd, "DevToolsActivePort"))
        except FileNotFoundError:
            pass
        e = dict(os.environ)
        e.update(env or {})
        self.p = subprocess.Popen(
            [h.HELIUM, f"--user-data-dir={udd}", "--headless=new", "--no-first-run",
             "--no-default-browser-check", "--disable-gpu", "--remote-debugging-port=0",
             f"--load-extension={h.EXT}", f"--disable-extensions-except={h.EXT}", "about:blank"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=e)
        RUN["helium"] = self
        self.c = CDP(udd)
        self.c.attach_extension_worker(EXT_ID)

    def js(self, expr, timeout=60):
        return self.c.eval(expr, timeout)

    def status(self):
        return self.js("chrome.storage.local.get('synchromeStatus').then(r => r.synchromeStatus ?? null)")

    def wait(self, pred, timeout=90, after=0):
        end = time.time() + timeout
        st = None
        while time.time() < end:
            st = self.status()
            if st and st.get("at", 0) >= after and pred(st):
                return st
            time.sleep(0.5)
        return st

    def stop(self):
        # Chromium commits the Bookmarks file ~2.5 s after the last change (ImportantFileWriter);
        # give it time before terminating, as a real browser session would.
        time.sleep(4)
        self.p.send_signal(signal.SIGTERM)
        try:
            self.p.wait(25)
        except subprocess.TimeoutExpired:
            self.p.kill()
        for _ in range(40):
            if not os.path.lexists(os.path.join(self.udd, "SingletonLock")):
                break
            time.sleep(0.25)


def RUNNING_JS(hl):
    """True while the extension still has queued events or a flush in flight."""
    return bool(hl.js("queue.length > 0 || running !== null || flushTimer !== null"))


def bm(st):
    return (st or {}).get("bookmarks") or {}


def up_to_date(st):
    b = bm(st)
    return b.get("state") == "up_to_date" and b.get("pending") == 0


def now_ms():
    return int(time.time() * 1000)


def rows(tree):
    """bar subtree as (depth, kind, title, host)."""
    return [(r[1], r[2], r[3], r[4]) for r in tree.get("tree", []) if r[0] == "bar"]


def sid_of(tree, title):
    return next((r[6] for r in tree.get("tree", []) if r[3] == title), None)


def parent_title(tree, title):
    """Title of the folder directly above `title` in the rendered (DFS) tree."""
    t = tree.get("tree", [])
    i = next(k for k, r in enumerate(t) if r[3] == title)
    return next((r[3] for r in reversed(t[:i]) if r[1] == t[i][1] - 1), None)


def urlhash(tree, title):
    return next((r[5] for r in tree.get("tree", []) if r[3] == title), None)


def new_chunks(before, after):
    old = {c["path"] for c in before.get("chunks", [])}
    return [c for c in after.get("chunks", []) if c["path"] not in old]


def find(title):
    return f"(await chrome.bookmarks.search({{title: {json.dumps(title)}}}))[0]"


KNOWN = [
    (0, "folder", "Synchrome Test", None),
    (1, "folder", "Folder A", None),
    (2, "url", "Bookmark 1", "example.com"),
    (2, "url", "Bookmark 2", "example.com"),
    (1, "folder", "Folder B", None),
    (2, "url", "Bookmark 3", "example.com"),
]
CREATE_KNOWN = """(async () => {
  const t = await chrome.bookmarks.create({parentId: "1", title: "Synchrome Test"});
  const a = await chrome.bookmarks.create({parentId: t.id, title: "Folder A"});
  await chrome.bookmarks.create({parentId: a.id, title: "Bookmark 1", url: "https://example.com/1?q=private-query"});
  await chrome.bookmarks.create({parentId: a.id, title: "Bookmark 2", url: "https://example.com/2"});
  const b = await chrome.bookmarks.create({parentId: t.id, title: "Folder B"});
  await chrome.bookmarks.create({parentId: b.id, title: "Bookmark 3", url: "https://example.com/3"});
  return true;
})()"""
CREATION_FIELDS = ["createdAt", "kind", "parentId", "pos", "root", "title", "url"]


def main(creds):
    h.prepare_binary()
    log_start = os.path.getsize(h.LOG) if os.path.exists(h.LOG) else 0
    print("=== bookmarks e2e (upload only) ===")
    check("precondition: app signed out, no Keychain session",
          dev("--dev-account").get("signed_in") is False and not h.keychain_has_session())

    # 1–2
    a = dev("--dev-sign-in", creds, "A")
    check("1 login A (exact user id)", a.get("signed_in") is True and a.get("user_id") == h.EXPECTED["A"], f"user_id={a.get('user_id')}")
    if not a.get("signed_in"):
        return
    a_id = a["user_id"]
    d = dev("--dev-register-device")
    me = d.get("device_id")
    inv = dev("--dev-inventory")
    check("2 device registered", d.get("is_current") is True and me in (inv.get("devices") or []), f"device_id={me}")

    udd = h.fresh_udd("bookmarks")
    RUN["udd"] = udd
    dev("--dev-install-host", udd)

    # 8 (executed first so that 9 is a genuine initial snapshot of an existing tree)
    hl = Helium(udd)
    hl.js(CREATE_KNOWN)
    st = hl.wait(lambda s: s.get("status") == "token_missing", 30)
    check("8 known structure created in the disposable Helium profile (extension still unlinked)",
          (st or {}).get("status") == "token_missing", "")
    hl.stop()

    # 3–5
    m = dev("--dev-create-mapping", f"E2E BM {os.urandom(3).hex()}", "helium", "Default")
    mid, pid = m.get("mapping_id"), m.get("profile_id")
    RUN["mid"] = mid
    inv = dev("--dev-inventory")
    check("3 cloud profile created", pid and inv.get("profiles") == 1, f"profile_id={pid}")
    check("4 cloud mapping created (pending_local_write)", m.get("token_state") == "pending_local_write", f"mapping_id={mid}")
    w = dev("--dev-write-token", mid, udd)
    check("5 mappingToken written into the disposable profile (browser closed)", w.get("display_status") == "token_written", "")

    # 6–7, 9: start -> handshake -> initial snapshot -> state.sqlite -> cloud
    t0 = now_ms()
    hl = Helium(udd)
    persisted = hl.js("chrome.bookmarks.search({title: 'Synchrome Test'}).then(r => r.length)")
    check("8b the known structure persisted across a browser restart", persisted == 1, f"found={persisted}")
    st = hl.wait(lambda s: up_to_date(s) and bm(s).get("bookmarks") == 3, 120, after=t0)
    latencies["initial_snapshot_ms"] = now_ms() - t0
    check("6 extension started and confirmed the mapping end-to-end",
          (st or {}).get("status") == "connected" and (st or {}).get("token_state") == "confirmed", f"status={(st or {}).get('status')}")
    v = next((x for x in dev("--dev-list-mappings") if x.get("id") == mid), {})
    check("7 mapping confirmed in the cloud", v.get("status") == "token_confirmed" and v.get("token_confirmed_at"),
          f"token_confirmed_at={v.get('token_confirmed_at')}")
    local = dev("--dev-local-tree", pid)
    c1 = dev("--dev-cloud-tree", pid)
    ch = c1.get("chunks", [])
    check("9a snapshot reached state.sqlite complete and normalised", rows(local) == KNOWN and local.get("bookmarks") == 3 and local.get("folders") == 3,
          json.dumps(rows(local)))
    check("9b published: 6 creation ops in 1 chunk (seq 1–6), 1 store_event",
          c1.get("ops") == 6 and len(ch) == 1 and ch[0]["seq_from"] == 1 and ch[0]["seq_to"] == 6 and c1.get("store_events") == 1
          and all(o["op"] == "set" and o["fields"] in (CREATION_FIELDS, [f for f in CREATION_FIELDS if f != "url"]) for o in ch[0]["ops"])
          and sum(1 for o in ch[0]["ops"] if "url" in o["fields"]) == 3,
          f"ops={c1.get('ops')} chunks={len(ch)} events={c1.get('store_events')}")
    check("9c structure and order rebuilt from the cloud match exactly (incl. URL hashes)",
          rows(c1) == KNOWN and [r[:6] for r in c1["tree"]] == [r[:6] for r in local["tree"]] and c1.get("seq_contiguous"),
          json.dumps(rows(c1)))

    # 10: repeated snapshot (forced, same browser session) must travel on its own and give 0 ops
    mark = os.path.getsize(h.LOG)
    hl.js("chrome.storage.session.clear().then(() => { needSnapshot = true; return run(true); })", timeout=90)
    seen = open(h.LOG, "rb").read()[mark:].decode("utf-8", "replace")
    check("10a forced repeated snapshot reached the app on its own and produced 0 ops",
          "bookmarks snapshot parts=1 nodes=8 -> Ok(Applied { ops: 0, pending: 0 })" in seen, "")
    c = dev("--dev-cloud-tree", pid)
    s = dev("--dev-bookmarks-status", mid).get("status") or {}
    check("10 repeated snapshot: 0 new ops, 0 new chunks, 0 new store_events",
          c.get("ops") == 6 and len(c.get("chunks", [])) == 1 and c.get("store_events") == 1 and s.get("pending") == 0,
          f"ops={c.get('ops')} chunks={len(c.get('chunks', []))} events={c.get('store_events')}")

    # 11–16: live edits, one delta each
    prev = c

    def edit(key, js):
        nonlocal prev
        t = now_ms()
        hl.js(f"(async () => {{ {js}; return true; }})()")
        # a sync_status received after this edit (hello alone does not count) and nothing pending
        st = hl.wait(lambda s: up_to_date(s) and s.get("bookmarks_at", 0) >= t and not RUNNING_JS(hl), 90, after=t)
        latencies[key] = now_ms() - t
        cur = dev("--dev-cloud-tree", pid)
        delta = new_chunks(prev, cur)
        before, prev = prev, cur
        return st, before, cur, delta

    st, before, cur, delta = edit("create_ms", f"const a = {find('Folder A')}; await chrome.bookmarks.create({{parentId: a.id, title: 'Bookmark 4', url: 'https://example.org/4'}})")
    ops = [o for c in delta for o in c["ops"]]
    check("11 create Bookmark 4: exactly 1 creation op in 1 new chunk + 1 store_event",
          up_to_date(st) and len(delta) == 1 and len(ops) == 1 and ops[0]["fields"] == CREATION_FIELDS
          and ops[0]["id"] == sid_of(cur, "Bookmark 4") and rows(cur)[4] == (2, "url", "Bookmark 4", "example.org")
          and cur.get("store_events") == before.get("store_events") + 1, json.dumps(ops))

    st, before, cur, delta = edit("rename_ms", f"await chrome.bookmarks.update(({find('Bookmark 1')}).id, {{title: 'Bookmark 1 renamed'}})")
    ops = [o for c in delta for o in c["ops"]]
    check("12 rename Bookmark 1: 1 op, fields [title], same entity id",
          len(ops) == 1 and ops[0]["fields"] == ["title"] and ops[0]["id"] == sid_of(before, "Bookmark 1")
          and sid_of(cur, "Bookmark 1 renamed") == sid_of(before, "Bookmark 1"), json.dumps(ops))

    st, before, cur, delta = edit("url_ms", f"await chrome.bookmarks.update(({find('Bookmark 4')}).id, {{url: 'https://example.net/4b'}})")
    ops = [o for c in delta for o in c["ops"]]
    check("13 URL change of Bookmark 4: 1 op, fields [url], URL hash changed, host example.net",
          len(ops) == 1 and ops[0]["fields"] == ["url"] and urlhash(cur, "Bookmark 4") != urlhash(before, "Bookmark 4")
          and (2, "url", "Bookmark 4", "example.net") in rows(cur), json.dumps(ops))

    st, before, cur, delta = edit("move_ms", f"await chrome.bookmarks.move(({find('Bookmark 2')}).id, {{parentId: ({find('Folder B')}).id, index: 1}})")
    ops = [o for c in delta for o in c["ops"]]
    fb = [r for r in rows(cur)][rows(cur).index((1, "folder", "Folder B", None)) + 1:]
    # pos is emitted only if the old key does not already sort correctly in the new folder
    # (here B2's key "k" > B3's "V", so the minimal delta is parentId alone).
    check("14 move Bookmark 2 into Folder B (after Bookmark 3): 1 op with parentId (+pos only if needed), parent = Folder B, order correct",
          len(ops) == 1 and ops[0]["fields"] in (["parentId"], ["parentId", "pos"]) and ops[0]["id"] == sid_of(before, "Bookmark 2")
          and parent_title(cur, "Bookmark 2") == "Folder B"
          and fb[:2] == [(2, "url", "Bookmark 3", "example.com"), (2, "url", "Bookmark 2", "example.com")],
          json.dumps({"ops": ops, "folder_b": fb[:2]}))

    st, before, cur, delta = edit("reorder_ms", f"await chrome.bookmarks.move(({find('Bookmark 2')}).id, {{parentId: ({find('Folder B')}).id, index: 0}})")
    ops = [o for c in delta for o in c["ops"]]
    fb = rows(cur)[rows(cur).index((1, "folder", "Folder B", None)) + 1:]
    check("15 reorder Folder B (Bookmark 2 first): 1 op [pos] on Bookmark 2 only; Bookmark 3 keeps its pos",
          len(ops) == 1 and ops[0]["fields"] == ["pos"] and ops[0]["id"] == sid_of(before, "Bookmark 2")
          and fb[:2] == [(2, "url", "Bookmark 2", "example.com"), (2, "url", "Bookmark 3", "example.com")], json.dumps(ops))

    doomed = {sid_of(cur, "Folder B"), sid_of(cur, "Bookmark 2"), sid_of(cur, "Bookmark 3")}
    st, before, cur, delta = edit("delete_ms", f"await chrome.bookmarks.removeTree(({find('Folder B')}).id)")
    ops = [o for c in delta for o in c["ops"]]
    check("16 delete Folder B with 2 children: 3 tombstones (folder + both descendants), nothing else",
          len(ops) == 3 and all(o["op"] == "delete" and o["fields"] is None for o in ops) and {o["id"] for o in ops} == doomed
          and not any(r[2] in ("Folder B", "Bookmark 2", "Bookmark 3") for r in rows(cur)), json.dumps(rows(cur)))
    after_edits = cur
    hl.stop()

    # 17: restart
    t = now_ms()
    hl = Helium(udd)
    st = hl.wait(up_to_date, 90, after=t)
    c = dev("--dev-cloud-tree", pid)
    check("17 Helium restart: equivalent snapshot -> 0 ops, no duplicates",
          up_to_date(st) and c.get("ops") == after_edits.get("ops") and len(c["chunks"]) == len(after_edits["chunks"])
          and c.get("store_events") == after_edits.get("store_events") and rows(c) == rows(after_edits), f"ops={c.get('ops')}")
    hl.stop()

    # 18: offline
    t = now_ms()
    hl = Helium(udd, env={"SUPABASE_URL": OFFLINE_URL})
    st = hl.wait(lambda s: s.get("status") == "connected_offline", 60, after=t)
    check("18a cloud unreachable: handshake connected_offline (token confirmed earlier, same account)",
          (st or {}).get("status") == "connected_offline", f"status={(st or {}).get('status')}")
    t = now_ms()
    hl.js(f"(async () => {{ const a = {find('Folder A')}; await chrome.bookmarks.create({{parentId: a.id, title: 'Offline 1', url: 'https://offline.example/1'}}); await chrome.bookmarks.update(a.id, {{title: 'Folder A offline'}}); return true; }})()")
    st = hl.wait(lambda s: bm(s).get("state") == "offline" and bm(s).get("pending", 0) >= 2, 90, after=t)
    hl.stop()
    s = dev("--dev-bookmarks-status", mid).get("status") or {}
    local = dev("--dev-local-tree", pid)
    c = dev("--dev-cloud-tree", pid)
    offline_pending = s.get("pending")
    check("18 cloud offline: connected_offline, state Offline, outbox keeps both changes, cloud untouched",
          bm(st).get("state") == "offline" and s.get("state") == "offline" and s.get("pending") == 2
          and (2, "url", "Offline 1", "offline.example") in rows(local) and (1, "folder", "Folder A offline", None) in rows(local)
          and c.get("ops") == after_edits.get("ops"), f"pending={s.get('pending')} cloud_ops={c.get('ops')}")

    # 20a: fault between upload and checkpoint
    f = dev("--dev-publish-fault", mid)
    c_fault = dev("--dev-cloud-tree", pid)
    s = dev("--dev-bookmarks-status", mid).get("status") or {}
    fault_chunk = new_chunks(after_edits, c_fault)
    check("20a fault after upload: chunk uploaded, store_event NOT recorded, checkpoint NOT advanced (pending 2)",
          "Failed" in str(f.get("outcome")) and f.get("uploads_before_fault") == 1 and len(fault_chunk) == 1
          and len(fault_chunk[0]["ops"]) == 2 and c_fault.get("store_events") == after_edits.get("store_events") and s.get("pending") == 2,
          f"outcome={f.get('outcome')} pending={s.get('pending')}")

    # 19 + 20b: cloud restored, the host retries the same sealed chunk
    t = now_ms()
    hl = Helium(udd)
    st = hl.wait(up_to_date, 120, after=t)
    latencies["recovery_ms"] = now_ms() - t
    c = dev("--dev-cloud-tree", pid)
    retried = [x for x in c["chunks"] if x["path"] == fault_chunk[0]["path"]] if fault_chunk else []
    check("19 cloud restored: pending published, seq contiguous, checkpoint advanced only now",
          up_to_date(st) and c.get("ops") == after_edits.get("ops") + 2 and c.get("seq_contiguous")
          and c.get("store_events") == after_edits.get("store_events") + 1, f"ops={c.get('ops')} events={c.get('store_events')}")
    check("20b retry re-sent the SAME chunk (same path, same bytes hash), no duplicate object or store_event",
          len(retried) == 1 and retried[0]["sha256_12"] == fault_chunk[0]["sha256_12"]
          and len(c["chunks"]) == len(c_fault["chunks"]) and len({e[2] for e in c["store_event_ranges"]}) == len(c["store_event_ranges"]),
          f"chunks={len(c['chunks'])} events={c.get('store_events')}")
    hl.stop()

    # 21: 401 -> forced refresh -> retry
    t = now_ms()
    hl = Helium(udd, env={"SUPABASE_URL": OFFLINE_URL})
    hl.wait(lambda s: s.get("status") == "connected_offline", 60, after=t)
    t = now_ms()
    hl.js(f"(async () => {{ const a = {find('Folder A offline')}; await chrome.bookmarks.create({{parentId: a.id, title: 'Refresh 1', url: 'https://refresh.example/1'}}); return true; }})()")
    hl.wait(lambda s: bm(s).get("pending", 0) >= 1, 60, after=t)
    hl.stop()
    before401 = dev("--dev-cloud-tree", pid)
    r = dev("--dev-publish-401", mid)
    c401 = dev("--dev-cloud-tree", pid)
    check("21 session: first call 401 with a rejected access token -> automatic refresh -> second attempt publishes",
          "Published" in str(r.get("outcome")) and r.get("forced_refreshes") == 1 and r.get("refresh_token_rotated") is True
          and c401.get("ops") == before401.get("ops") + 1 and (r.get("status") or {}).get("pending") == 0,
          f"outcome={r.get('outcome')} refreshes={r.get('forced_refreshes')} rotated={r.get('refresh_token_rotated')}")
    final = c401

    # 23 (part): mapping confirmation is mandatory, even with a valid session
    import random
    rnd = "".join(random.choice("0123456789abcdef") for _ in range(64))
    batch = {"type": "bookmarks_events", "epoch": "e2e", "batch_id": "b1", "events": [{"n": 1, "kind": "removed", "id": "1"}]}
    rc, msgs = h.host_session(h.frame({"type": "hello", "ext_version": "e2e", "mapping_token": rnd}) + h.frame(batch))
    rc2, msgs2 = h.host_session(h.frame({"type": "hello", "ext_version": "e2e"}) + h.frame(batch))
    after_probe = dev("--dev-cloud-tree", pid)
    check("23a mapping confirmation mandatory: unknown token / no token + valid session -> not_confirmed, nothing captured",
          [m.get("type") for m in msgs] == ["hello_result", "error", "bookmarks_ack"] and msgs[0].get("status") == "token_unknown"
          and msgs[1].get("code") == "not_confirmed" and msgs2[1].get("code") == "not_confirmed"
          and after_probe.get("ops") == final.get("ops"), f"{msgs[0].get('status')}/{msgs[1].get('code')}")

    # 22: account B
    b = dev("--dev-sign-in", creds, "B")
    first_path = final["chunks"][0]["path"]
    probe = dev("--dev-cloud-probe-foreign", a_id, pid, me, first_path)
    inv = dev("--dev-inventory")
    refused = lambda v: isinstance(v, dict) and "Err" in v
    check("22a login B (exact user id)", b.get("signed_in") and b.get("user_id") == h.EXPECTED["B"], f"user_id={b.get('user_id')}")
    check("22b B cannot list A's objects (A's literal prefix, and A's ids under its own prefix)",
          probe.get("list_sees") == {"Ok": 0} and probe.get("list_foreign_prefix_sees") == 0,
          json.dumps({k: probe.get(k) for k in ("list_sees", "list_foreign_prefix_sees", "list_foreign_prefix_error")}))
    check("22c B cannot download A's objects (not found)", probe.get("get_first_chunk") == {"Ok": False}, json.dumps(probe.get("get_first_chunk")))
    check("22d B cannot write under A's prefix (new name or overwrite) nor into A's profile under its own prefix",
          refused(probe.get("put_under_foreign_prefix")) and refused(probe.get("overwrite_foreign_chunk")) and refused(probe.get("put_into_foreign_profile")),
          json.dumps({k: probe.get(k) for k in ("put_under_foreign_prefix", "overwrite_foreign_chunk", "put_into_foreign_profile")}))
    check("22e B cannot insert store_events for A's profile", refused(probe.get("store_event_insert")), json.dumps(probe.get("store_event_insert")))
    check("22f B sees no metadata of A (profile, mappings, devices, store_events)",
          probe.get("foreign_profile_visible") == {"Ok": False} and probe.get("foreign_mappings_visible") == {"Ok": 0}
          and probe.get("store_events_sees") == {"Ok": 0} and inv.get("profiles") == 0 and inv.get("mappings") == 0 and me not in (inv.get("devices") or []), "")
    dev("--dev-sign-in", creds, "A")
    still = dev("--dev-cloud-tree", pid)
    check("22g A's chunks unchanged after B's attempts", [x["sha256_12"] for x in still["chunks"]] == [x["sha256_12"] for x in final["chunks"]], "")

    # 23: security
    env = open(os.path.join(h.ROOT, ".env")).read()
    anon = next((l.split("=", 1)[1].strip().strip('"') for l in env.splitlines() if l.startswith("SUPABASE_ANON_KEY=")), "")
    ext_text = "".join(open(os.path.join(h.EXT, f), errors="replace").read() for f in os.listdir(h.EXT) if os.path.isfile(os.path.join(h.EXT, f)))
    check("23b extension contains no anon key, no JWT, no Supabase URL", anon and anon not in ext_text and not h.JWT.search(ext_text)
          and "supabase" not in ext_text.lower(), "")
    log = open(h.LOG, "rb").read()[log_start:].decode("utf-8", "replace")
    leaks = [w for w in ("example.com", "example.org", "example.net", "offline.example", "refresh.example", "private-query",
                         "Bookmark", "Synchrome Test", "Folder A", "Offline 1") if w in log]
    check("23c native-host.log: no URLs, no titles, no full mapping tokens, no access/refresh tokens",
          not leaks and h.secret_free(log), f"{len(log)} bytes this run; leaks={leaks}")
    views = json.dumps(dev("--dev-list-mappings"))
    dist = "".join(open(os.path.join(h.ROOT, "apps/desktop/dist/assets", f)).read() for f in os.listdir(os.path.join(h.ROOT, "apps/desktop/dist/assets")))
    check("23d frontend: views carry counts only (no URLs/titles of bookmarks, no token); bundle has no diagnostics URLs",
          "example" not in views and "Bookmark" not in views and '"mapping_token"' not in views and h.secret_free(views)
          and '"bookmarks"' in views and "example.com" not in dist, "")

    # 24: metrics
    metrics = {
        "bookmarks_normalised": final.get("bookmarks"), "folders": final.get("folders"),
        "ops_total": final.get("ops"), "deletes": final.get("deletes"), "chunks": len(final["chunks"]),
        "store_events": final.get("store_events"),
        "chunk_seq_ranges": [(x["seq_from"], x["seq_to"], len(x["ops"]), x["bytes"]) for x in final["chunks"]],
        "store_event_ranges": [(e[0], e[1]) for e in final["store_event_ranges"]],
        "pending_during_offline": offline_pending, "latency_ms": latencies,
        "final_cloud_tree": rows(final),
    }
    print("METRICS " + json.dumps(metrics))
    check("24 metrics collected", metrics["chunks"] and metrics["store_events"] == metrics["chunks"], "")

    # 25: cleanup
    cl = dev("--dev-cleanup", mid)
    gone = dev("--dev-cloud-tree", pid)
    inv = dev("--dev-inventory")
    local = dev("--dev-bookmarks-status", mid)
    check("25a Storage objects purged, store_events / profile / mapping / device rows gone",
          cl.get("purged_objects") == len(final["chunks"]) and cl.get("deleted_profile") and cl.get("device_deleted")
          and gone.get("objects") == 0 and gone.get("store_events") == 0
          and inv.get("profiles") == 0 and inv.get("mappings") == 0 and inv.get("devices") == [], json.dumps(cl))
    so = dev("--dev-sign-out")
    check("25b Keychain session removed", so.get("signed_out") and not h.keychain_has_session(), "")
    shutil_rm(udd)
    check("25c disposable Helium profile and its host manifest removed", not os.path.exists(udd), udd)
    # state.sqlite: only this harness wrote into it; verify it is empty, then delete it
    con = sqlite3.connect(STATE_DB)
    counts = {t: con.execute(f"select count(*) from {t}").fetchone()[0] for t in ("profiles", "entities", "chromium_index", "ops", "chunks", "ext_cursor", "lease")}
    con.close()
    empty = local.get("profile_id") is None and all(v == 0 for v in counts.values())
    if empty:
        for sfx in ("", "-wal", "-shm"):
            try:
                os.remove(STATE_DB + sfx)
            except FileNotFoundError:
                pass
    check("25d local bookmarks state forgotten; test state.sqlite empty and deleted", empty and not os.path.exists(STATE_DB), json.dumps(counts))
    bdir = os.path.join(APPDATA, "backups", "ext-storage")
    removed = 0
    for n in os.listdir(bdir) if os.path.isdir(bdir) else []:
        if n.endswith(EXT_ID):
            shutil_rm(os.path.join(bdir, n))
            removed += 1
    check("25e extension-store backups of the disposable profile removed", not any(n.endswith(EXT_ID) for n in os.listdir(bdir)) if os.path.isdir(bdir) else True,
          f"removed={removed}")


def shutil_rm(p):
    import shutil
    shutil.rmtree(p, ignore_errors=True)


def emergency_cleanup():
    """Best effort after an abort: stop Helium, remove cloud rows/objects, sign out."""
    print("!! aborted: emergency cleanup")
    hl = RUN.get("helium")
    if hl and hl.p.poll() is None:
        hl.p.kill()
    if RUN.get("mid"):
        dev("--dev-sign-in", RUN["creds"], "A")
        print("   cleanup:", json.dumps(dev("--dev-cleanup", RUN["mid"])))
    dev("--dev-sign-out")
    if RUN.get("udd"):
        shutil_rm(RUN["udd"])


if __name__ == "__main__":
    RUN["creds"] = sys.argv[1]
    try:
        main(sys.argv[1])
    except Exception as e:  # noqa: BLE001
        check("harness completed without exception", False, type(e).__name__ + ": " + str(e)[:200])
        emergency_cleanup()
    failed = [n for n, ok in h.results if not ok]
    print(f"\n=== bookmarks: {len(h.results) - len(failed)} pass, {len(failed)} fail ===")
    if failed:
        print("  failures: " + ", ".join(failed))
    sys.exit(1 if failed else 0)
