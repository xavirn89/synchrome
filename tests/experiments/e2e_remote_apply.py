"""Two-device E2E of cloud -> browser apply (M1 slice 8). Disposable profiles only.

Device A and device B are two simulated installations on this machine, both signed in as
test user A: each has its own SYNCHROME_DATA_DIR (device.json => own device id,
state.sqlite, token registry, native-host.log, backups) and its own disposable Helium
--user-data-dir. Test user B is used only for the RLS probe. Edits are made through CDP
inside each extension's own worker. Prints ids, counts, field names, plan op codes and the
titles of the test fixture only — never URLs (hosts at most), tokens or passwords.

  EXPECTED_A_ID=… EXPECTED_B_ID=… python3 e2e_remote_apply.py <creds-env-file>
"""
import json, os, re, shutil, subprocess, sys, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import e2e_handshake as h  # noqa: E402
import e2e_bookmarks as eb  # noqa: E402

check = h.check
WORK = "/tmp/synchrome-e2e"
STATE = {"A": None, "B": None, "C": None}
M = {"latency_ms": {}, "suppressed_echo_events": 0, "plans": [], "applied_results": []}


class Device:
    def __init__(self, name):
        self.name = name
        self.data = os.path.join(WORK, f"dev{name}-data")
        self.udd = os.path.join(WORK, f"dev{name}-helium")
        shutil.rmtree(self.data, ignore_errors=True)
        shutil.rmtree(self.udd, ignore_errors=True)
        os.makedirs(self.udd)
        self.env = {"SYNCHROME_DATA_DIR": self.data}
        self.hl = None
        self.mid = None
        self.device_id = None
        STATE[name] = self

    def dev(self, *args, offline=False):
        e = dict(os.environ)
        e.update(self.env)
        if offline:
            e["SUPABASE_URL"] = eb.OFFLINE_URL
        p = subprocess.run([h.APP, *args], capture_output=True, text=True, timeout=120, env=e)
        line = (p.stdout.strip().splitlines() or ["{}"])[-1]
        try:
            return json.loads(line)
        except json.JSONDecodeError:
            return {"error": f"non-json output (rc={p.returncode})"}

    def start(self, offline=False):
        env = dict(self.env)
        if offline:
            env["SUPABASE_URL"] = eb.OFFLINE_URL
        self.hl = eb.Helium(self.udd, env=env)
        # deterministic harness: no background polling, only the pulls this script triggers
        # (the worker start itself still runs one cycle, as in real life)
        self.hl.js("chrome.alarms.clearAll().then(() => true)")
        return self.hl

    def stop(self):
        if self.hl:
            try:
                M["suppressed_echo_events"] += int(self.hl.js("suppressedEvents") or 0) if self.name == "B" else 0
            except Exception:  # noqa: BLE001
                pass
            self.hl.stop()
            self.hl = None

    def log(self):
        p = os.path.join(self.data, "logs", "native-host.log")
        return open(p, "rb").read().decode("utf-8", "replace") if os.path.exists(p) else ""

    def tree(self):
        return self.hl.js("""(async () => {
          const [root] = await chrome.bookmarks.getTree();
          const bar = root.children.find(c => c.folderType === "bookmarks-bar" || c.id === "1");
          const f = (n) => ({t: n.title, u: n.url ?? null, c: (n.children || []).map(f)});
          return (bar.children || []).map(f);
        })()""")

    def status(self):
        return self.dev("--dev-bookmarks-status", self.mid).get("status") or {}

    def idle(self):
        return bool(self.hl.js("queue.length === 0 && running === null && flushTimer === null"))


def wait(pred, timeout=90, step=0.5):
    end = time.time() + timeout
    while time.time() < end:
        r = pred()
        if r:
            return r
        time.sleep(step)
    return None


def flat(tree, d=0):
    out = []
    for n in tree:
        out.append((d, n["t"], n["u"]))
        out += flat(n["c"], d + 1)
    return out


def show(tree):
    """Fixture titles + URL host only."""
    return [(d, t, (u.split("/")[2] if u else None)) for d, t, u in flat(tree)]


def find(title):
    return eb.find(title)


def edit_and_publish(a, js):
    t = eb.now_ms()
    a.hl.js(f"(async () => {{ {js}; return true; }})()")
    st = a.hl.wait(lambda s: eb.up_to_date(s) and s.get("bookmarks_at", 0) >= t, 60, after=t)
    wait(a.idle, 30)
    return st


def parse(log):
    return {
        "pulls": re.findall(r"bookmarks (?:bootstrap )?pull -> (.*)", log),
        "plans": [(int(n), k, o) for n, k, o in re.findall(r"apply plan ops=(\d+) (\{[^}]*\}) more=\w+ order=(\S*)", log)],
        "applied": re.findall(r"bookmarks applied results=(\d+) -> (.*)", log),
        "snapshots": re.findall(r"bookmarks snapshot parts=\d+ nodes=\d+ -> (.*)", log),
        "events": re.findall(r"bookmarks events n=\d+ -> (.*)", log),
    }


def pull_b(b, js_after=None):
    """Trigger B's extension run (pull + apply + reconcile); return the parsed log slice."""
    mark = len(b.log().encode())
    t = time.time()
    expr = "(async () => { await run(true); %s return true; })()" % (js_after or "")
    b.hl.js(expr, timeout=120)
    wait(b.idle, 60)
    ms = int((time.time() - t) * 1000)
    sl = parse(open(os.path.join(b.data, "logs", "native-host.log"), "rb").read()[mark:].decode("utf-8", "replace"))
    for n, k, o in sl["plans"]:
        if n:
            M["plans"].append((n, o))
    M["applied_results"] += sl["applied"]
    return sl, ms


def journal(a, pid, device_id):
    c = a.dev("--dev-cloud-tree", pid)
    chunks = [x for x in c.get("chunks", []) if f"/journal/{device_id}/" in x["path"]]
    events = [e for e in c.get("store_event_ranges", []) if f"/journal/{device_id}/" in e[2]]
    return c, chunks, events


def same_identity_tree(a, b, pid, mask=None):
    """A's and B's materialised trees (entity ids, kinds, titles, hosts, order) are equal."""
    ta = a.dev("--dev-local-tree", pid).get("tree")
    tb = b.dev("--dev-local-tree", pid).get("tree")
    if mask:
        ta = [r if r[6] not in mask else r[:3] + ["<masked>"] + r[4:] for r in ta]
        tb = [r if r[6] not in mask else r[:3] + ["<masked>"] + r[4:] for r in tb]
    return ta == tb


def main(creds):
    h.prepare_binary()
    os.makedirs(WORK, exist_ok=True)
    a, b = Device("A"), Device("B")
    print("=== two-device remote apply E2E ===")
    check("pre: signed out, no Keychain session", a.dev("--dev-account").get("signed_in") is False and not h.keychain_has_session())

    # 1–3
    sa = a.dev("--dev-sign-in", creds, "A")
    check("1 login user A for device A (exact id)", sa.get("user_id") == h.EXPECTED["A"], f"user_id={sa.get('user_id')}")
    if not sa.get("signed_in"):
        return
    sb = b.dev("--dev-sign-in", creds, "A")
    check("2 login user A for device B (exact id)", sb.get("user_id") == h.EXPECTED["A"], f"user_id={sb.get('user_id')}")
    a.device_id = a.dev("--dev-register-device").get("device_id")
    b.device_id = b.dev("--dev-register-device").get("device_id")
    ja = json.load(open(os.path.join(a.data, "device.json")))
    jb = json.load(open(os.path.join(b.data, "device.json")))
    check("3 same account, distinct device ids (device.json), distinct data dirs",
          a.dev("--dev-account").get("user_id") == b.dev("--dev-account").get("user_id") == h.EXPECTED["A"]
          and a.device_id != b.device_id and ja.get("device_id") == a.device_id and jb.get("device_id") == b.device_id and a.data != b.data,
          f"A={a.device_id} B={b.device_id}")

    # 4–6
    m = a.dev("--dev-create-mapping", f"E2E RA {os.urandom(3).hex()}", "helium", "Default")
    a.mid, pid = m.get("mapping_id"), m.get("profile_id")
    a.dev("--dev-write-token", a.mid, a.udd)
    a.dev("--dev-install-host", a.udd)
    t = eb.now_ms()
    a.start()
    sta = a.hl.wait(lambda s: s.get("status") == "connected" and s.get("token_state") == "confirmed", 60, after=t)
    wait(a.idle, 30)
    check("4 device A: cloud profile + mapping confirmed", sta is not None, f"profile={pid} mapping={a.mid}")
    mb = b.dev("--dev-map-existing", pid, "helium", "Default")
    b.mid = mb.get("mapping_id")
    b.dev("--dev-write-token", b.mid, b.udd)
    b.dev("--dev-install-host", b.udd)
    t = eb.now_ms()
    b.start()
    stb = b.hl.wait(lambda s: s.get("status") == "connected" and s.get("token_state") == "confirmed", 60, after=t)
    wait(b.idle, 30)
    check("5 device B: the SAME cloud profile mapped and confirmed", stb is not None and mb.get("profile_id") == pid, f"mapping={b.mid}")
    files = lambda d: all(os.path.exists(os.path.join(d.data, f)) for f in ("device.json", "local_tokens.json", "state.sqlite", "logs/native-host.log"))
    check("6 same account + profile; distinct devices, mappings, state.sqlite, registries, logs",
          a.mid != b.mid and files(a) and files(b) and os.path.samefile(a.data, a.data) and a.data != b.data, "")

    # 7–9
    st = edit_and_publish(a, """
      const t = await chrome.bookmarks.create({parentId: "1", title: "Synchrome Remote Test"});
      const fa = await chrome.bookmarks.create({parentId: t.id, title: "Folder A"});
      await chrome.bookmarks.create({parentId: fa.id, title: "Bookmark 1", url: "https://example.com/1"});
      await chrome.bookmarks.create({parentId: fa.id, title: "Bookmark 2", url: "https://example.com/2"});
      const fb = await chrome.bookmarks.create({parentId: t.id, title: "Folder B"});
      await chrome.bookmarks.create({parentId: fb.id, title: "Bookmark 3", url: "https://example.com/3"})""")
    c, ca, ea = journal(a, pid, a.device_id)
    _, cb, eb_ = journal(a, pid, b.device_id)
    check("7-8 A created the structure and published it completely", eb.up_to_date(st) and c.get("ops") == 6, f"ops={c.get('ops')}")
    check("9 cloud: A's journal + store_events present; B published nothing", len(ca) == 1 and len(ea) == 1 and not cb and not eb_, f"A chunks={len(ca)} events={len(ea)}")

    # 10–15
    sl, ms = pull_b(b)
    M["latency_ms"]["first_apply"] = ms
    cur = b.dev("--dev-remote-cursor", pid, a.device_id).get("applied_through")
    plan = next((p for p in sl["plans"] if p[0]), (0, "", ""))
    check("10-11 B pulled: listed the event, downloaded + verified A's chunk, cursor A=6, materialised, planned",
          any("chunks: 1, ops: 6" in p for p in sl["pulls"]) and cur == 6 and plan[0] > 0, f"pull={sl['pulls']} plan={plan}")
    check("12 B's extension applied creates (folders first) and an exact order",
          plan[2].startswith("cf") and "cb" in plan[2] and sl["applied"] and "failed: 0" in sl["applied"][0][1], f"order={plan[2]} applied={sl['applied']}")
    ta, tb = a.tree(), b.tree()
    print("     tree A after first apply:", json.dumps(show(ta)))
    print("     tree B after first apply:", json.dumps(show(tb)))
    check("13 tree B == tree A (hierarchy, titles, URLs, order) and same entity ids", tb == ta and same_identity_tree(a, b, pid), "")
    _, cb, eb_ = journal(a, pid, b.device_id)
    s = b.status()
    check("14 echo suppression: apply events produced no ops, no B journal, no B store_events",
          s.get("pending") == 0 and not cb and not eb_ and int(b.hl.js("suppressedEvents")) >= 6, f"suppressed={b.hl.js('suppressedEvents')}")
    check("15 B's reconcile snapshot after the apply = 0 ops (0 chunks, 0 store_events)",
          sl["snapshots"][-1:] == ["Ok(Applied { ops: 0, pending: 0 })"], json.dumps(sl["snapshots"]))

    # 16–27
    def phase(key, js, fields, codes):
        before, ca0, _ = journal(a, pid, a.device_id)
        st = edit_and_publish(a, js)
        after, ca1, ea1 = journal(a, pid, a.device_id)
        new = [x for x in ca1 if x["path"] not in {y["path"] for y in ca0}]
        ops = [o for x in new for o in x["ops"]]
        sl, ms = pull_b(b)
        M["latency_ms"][key] = ms
        plans = [p for p in sl["plans"] if p[0]]
        _, cb, _ = journal(a, pid, b.device_id)
        ok_delta = len(new) == 1 and [o["fields"] for o in ops] == fields
        ok_plan = bool(plans) and all(cd in plans[0][2].split(",") for cd in codes)
        same = b.tree() == a.tree() and same_identity_tree(a, b, pid)
        return ok_delta, ok_plan, same, not cb and b.status().get("pending") == 0, ops, plans

    d, p, s_, e, ops, plans = phase("rename", f"await chrome.bookmarks.update(({find('Bookmark 1')}).id, {{title: 'Bookmark 1 renamed'}})", [["title"]], ["rn"])
    check("16-17 rename on A: exactly 1 op [title] published", d, json.dumps(ops))
    check("18-19 B pulled, applied a rename, mirrors it exactly; no echo", p and s_ and e, json.dumps(plans))
    d, p, s_, e, ops, plans = phase("url", f"await chrome.bookmarks.update(({find('Bookmark 2')}).id, {{url: 'https://example.org/2b'}})", [["url"]], ["url"])
    check("20-21 URL change on A -> B applied exactly that change (set_url); no echo", d and p and s_ and e, json.dumps({"ops": ops, "plans": plans}))
    d, p, s_, e, ops, plans = phase("move", f"await chrome.bookmarks.move(({find('Bookmark 2')}).id, {{parentId: ({find('Folder B')}).id}})", [["parentId", "pos"]], ["mv"])
    if not d:  # the old key may already sort correctly in the new folder: [parentId] alone is the minimal delta
        d = len(ops) == 1 and ops[0]["fields"] in (["parentId"], ["parentId", "pos"])
    check("22-23 move Bookmark 2 into Folder B: B applied move (+reorder), same parent and pos (same materialised tree)", d and p and s_ and e, json.dumps({"ops": ops, "plans": plans}))
    d, p, s_, e, ops, plans = phase("reorder", f"await chrome.bookmarks.move(({find('Bookmark 2')}).id, {{parentId: ({find('Folder B')}).id, index: 0}})", [["pos"]], ["ro"])
    check("24-25 reorder on A: B has exactly the same order", d and p and s_ and e, json.dumps({"ops": ops, "plans": plans}))
    d, p, s_, e, ops, plans = phase("delete", f"await chrome.bookmarks.remove(({find('Bookmark 1 renamed')}).id)", [None], ["d"])
    check("26-27 delete a bookmark on A: B removed it", d and p and s_ and e, json.dumps({"ops": ops, "plans": plans}))

    # 29–32 restart Helium B + restart app/host B
    b.stop()
    cur0 = b.dev("--dev-remote-cursor", pid, a.device_id).get("applied_through")
    app = b.dev("--dev-sync", b.mid)  # a fresh app process
    t = eb.now_ms()
    mark = len(b.log().encode())
    b.start()
    wait(lambda: b.idle() and b.status().get("download", {}).get("state") == "up_to_date", 60)
    sl = parse(open(os.path.join(b.data, "logs", "native-host.log"), "rb").read()[mark:].decode("utf-8", "replace"))
    cur1 = b.dev("--dev-remote-cursor", pid, a.device_id).get("applied_through")
    _, cb, _ = journal(a, pid, b.device_id)
    check("29-32 restart Helium B and app B: nothing reprocessed, cursor unchanged, no duplicates, no echoes",
          "chunks: 0, ops: 0" in app.get("pull", "") and cur0 == cur1 and all("chunks: 0" in p for p in sl["pulls"])
          and not any(p[0] for p in sl["plans"]) and sl["snapshots"] and all("ops: 0" in x for x in sl["snapshots"])
          and b.tree() == a.tree() and not cb, f"cursor {cur0}->{cur1} pulls={sl['pulls']}")

    # 33–38 offline B
    b.stop()
    t = eb.now_ms()
    b.start(offline=True)
    b.hl.wait(lambda s: s.get("status") == "connected_offline", 60, after=t)
    tree_off = b.tree()
    edit_and_publish(a, f"await chrome.bookmarks.create({{parentId: ({find('Folder A')}).id, title: 'Offline 1', url: 'https://example.net/off'}})")
    edit_and_publish(a, f"await chrome.bookmarks.update(({find('Offline 1')}).id, {{title: 'Offline 1 v2'}})")
    b.hl.js("run(true)", timeout=90)
    wait(b.idle, 60)
    offl = b.dev("--dev-sync", b.mid, offline=True)
    cur_off = b.dev("--dev-remote-cursor", pid, a.device_id).get("applied_through")
    check("33-35 B offline: cursor kept, browser untouched, state Offline",
          cur_off == cur1 and b.tree() == tree_off and "offline" in offl.get("pull", "")
          and (b.status().get("download") or {}).get("state") == "offline", f"pull={offl.get('pull')}")
    b.stop()
    c_all, ca, _ = journal(a, pid, a.device_id)
    a_max = max(x["seq_to"] for x in ca)
    seqs = sorted(o["seq"] for x in ca for o in x["ops"])
    sl, ms = (None, None)
    t = time.time()
    b.start()
    wait(lambda: b.idle() and (b.status().get("download") or {}).get("state") == "up_to_date"
         and b.dev("--dev-remote-cursor", pid, a.device_id).get("applied_through") == a_max, 90)
    M["latency_ms"]["offline_recovery"] = int((time.time() - t) * 1000)
    cur2 = b.dev("--dev-remote-cursor", pid, a.device_id).get("applied_through")
    titles = [r[1] for r in flat(b.tree())]
    check("36-38 cloud restored: both changes applied in order, A's seq continuous, cursor at the end",
          cur2 == a_max and seqs == list(range(1, a_max + 1)) and "Offline 1 v2" in titles and "Offline 1" not in titles and b.tree() == a.tree(),
          f"cursor={cur2} a_max={a_max}")

    # 39–45 conflict on X (+ a non-conflicting Y in the same publication)
    b.stop()
    t = eb.now_ms()
    b.start(offline=True)
    b.hl.wait(lambda s: s.get("status") == "connected_offline", 60, after=t)
    t = eb.now_ms()
    b.hl.js(f"(async () => {{ await chrome.bookmarks.update(({find('Bookmark 3')}).id, {{title: 'X edited on B'}}); return true; }})()")
    b.hl.wait(lambda s: eb.bm(s).get("pending", 0) >= 1, 60, after=t)
    b.stop()
    x_sid = next(r[6] for r in b.dev("--dev-local-tree", pid)["tree"] if r[3] == "X edited on B")
    check("39-40 B edited X locally while offline; the op is pending (unpublished) in B's outbox", b.status().get("pending") == 1, f"X={x_sid}")
    edit_and_publish(a, f"await chrome.bookmarks.update(({find('Bookmark 3')}).id, {{title: 'X edited on A'}}); await chrome.bookmarks.update(({find('Offline 1 v2')}).id, {{title: 'Y edited on A'}})")
    _, ca, _ = journal(a, pid, a.device_id)
    x_remote_seq = max(o["seq"] for x in ca for o in x["ops"] if o["id"] == x_sid)
    b.start()
    wait(lambda: b.idle() and (b.status().get("download") or {}).get("state") in ("conflict",), 90)
    s = b.status()
    conf = b.dev("--dev-conflicts", pid).get("conflicts", [])
    titles = [r[1] for r in flat(b.tree())]
    check("41-44 A changed X; B did NOT overwrite: local value intact, local_pending conflict holding A's op, state Conflict",
          "X edited on B" in titles and "X edited on A" not in titles
          and any(c_["id"] == x_sid and c_["kind"] == "local_pending" and c_["remote_seq"] == x_remote_seq for c_ in conf)
          and (s.get("download") or {}).get("state") == "conflict", json.dumps({"download": s.get("download"), "conflicts": conf}))
    check("45 the non-conflicting entity Y of the same chunk WAS applied (profile not blocked)", "Y edited on A" in titles, "")
    check("44b B's own local change on X was still published (nothing lost)", s.get("pending") == 0, "")

    # 46–48 legit local edit immediately after a remote apply
    edit_and_publish(a, f"await chrome.bookmarks.update(({find('Folder A')}).id, {{title: 'Folder A (from A)'}})")
    _, cb0, _ = journal(a, pid, b.device_id)
    sl, ms = pull_b(b, js_after=f"await chrome.bookmarks.update(({find('Bookmark 2')}).id, {{title: 'B legit edit'}});")
    newb = wait(lambda: [x for x in journal(a, pid, b.device_id)[1] if x["path"] not in {y["path"] for y in cb0}], 60) or []
    wait(b.idle, 30)
    titles = [r[1] for r in flat(b.tree())]
    check("46-48 a legit local edit made right after the apply (same task, no delay) became 1 op and was published",
          any(p[0] for p in sl["plans"]) and "Folder A (from A)" in titles and len(newb) == 1 and [o["fields"] for o in newb[0]["ops"]] == [["title"]],
          f"B new chunks={len(newb)}")
    sl_a, _ = pull_b(a)
    check("47b A received B's legit edit", "B legit edit" in [r[1] for r in flat(a.tree())], "")

    # 49–50 gap
    edit_and_publish(a, f"await chrome.bookmarks.update(({find('Y edited on A')}).id, {{title: 'G1'}})")
    edit_and_publish(a, f"await chrome.bookmarks.update(({find('G1')}).id, {{title: 'G2'}})")
    _, ca, _ = journal(a, pid, a.device_id)
    s1, s2 = sorted(x["seq_to"] for x in ca)[-2:]
    removed = a.dev("--dev-delete-event", pid, a.device_id, str(s1))
    cur_before = b.dev("--dev-remote-cursor", pid, a.device_id).get("applied_through")
    tree_before = b.tree()
    sl, _ = pull_b(b)
    cur_gap = b.dev("--dev-remote-cursor", pid, a.device_id).get("applied_through")
    check("49-50 gap (event of seq %d hidden): B stops device A at the gap, cursor not advanced past it, nothing applied" % s1,
          removed.get("deleted") and cur_gap == cur_before == s1 - 1 and any(f"gaps: [(\"{a.device_id}\", {s1})]" in p for p in sl["pulls"]) and b.tree() == tree_before,
          f"cursor={cur_gap} pulls={sl['pulls']}")
    a.dev("--dev-insert-event", json.dumps(removed["event"]))
    sl, _ = pull_b(b)
    cur_fill = b.dev("--dev-remote-cursor", pid, a.device_id).get("applied_through")
    # after step 41-44 A and B intentionally differ in X's title (the held conflict): mask it
    xmask = lambda tr: [(d_, "<X>" if t_ in ("X edited on A", "X edited on B") else t_, u_) for d_, t_, u_ in flat(tr)]
    check("50b gap filled: both chunks applied in order, cursor at the end (trees equal except the conflicted X)",
          cur_fill == s2 and "G2" in [r[1] for r in flat(b.tree())] and "G1" not in [r[1] for r in flat(b.tree())] and xmask(b.tree()) == xmask(a.tree()),
          f"cursor={cur_fill}")

    # 51–52 duplicates
    dup = b.dev("--dev-pull-duplicates", b.mid)
    cur_dup = b.dev("--dev-remote-cursor", pid, a.device_id).get("applied_through")
    n_before = len(flat(b.tree()))
    sl, _ = pull_b(b)
    check("51-52 every event presented twice: no new apply, no duplicates, cursor unchanged",
          "chunks: 0, ops: 0" in dup.get("pull", "") and dup.get("remote_pending_after") == dup.get("remote_pending_before") == 0
          and cur_dup == cur_fill and not any(p[0] for p in sl["plans"]) and len(flat(b.tree())) == n_before, json.dumps(dup))

    # 28 delete a folder with children while B holds an unsynced local child in it
    b.stop()
    t = eb.now_ms()
    b.start(offline=True)
    b.hl.wait(lambda s: s.get("status") == "connected_offline", 60, after=t)
    b.hl.js(f"(async () => {{ await chrome.bookmarks.create({{parentId: ({find('Folder B')}).id, title: 'B unsynced', url: 'https://example.net/b'}}); return true; }})()")
    b.hl.wait(lambda s: eb.bm(s).get("pending", 0) >= 1, 60)
    b.stop()
    edit_and_publish(a, f"await chrome.bookmarks.removeTree(({find('Folder B')}).id)")
    b.start()
    wait(lambda: b.idle() and b.status().get("pending") == 0, 90)
    lg = parse(b.log())
    last_plan = [p for p in lg["plans"] if p[0]][-1]
    titles = [r[1] for r in flat(b.tree())]
    conf = b.dev("--dev-conflicts", pid).get("conflicts", [])
    order = last_plan[2].split(",")
    check("28 folder delete: children deleted first (d…D, non-recursive), folder kept because B's unsynced child is inside, explicit apply_failed conflict, local child intact",
          "D" in order and order.index("D") > max(i for i, k in enumerate(order) if k == "d")
          and "Folder B" in titles and "B unsynced" in titles and "B legit edit" not in titles
          and "X edited on B" in titles  # X is in conflict: its remote delete is held too
          and any(c_["kind"] == "apply_failed" for c_ in conf), f"order={last_plan[2]} conflicts={[c_['kind'] for c_ in conf]}")
    sl_a, _ = pull_b(a)
    conf_a = a.dev("--dev-conflicts", pid).get("conflicts", [])
    check("28b A got B's child of a folder A deleted: explicit orphan conflict (not stuck as pending)",
          any(c_["kind"] == "orphan" for c_ in conf_a) and (a.status().get("download") or {}).get("remote_pending") == 0, json.dumps(conf_a))

    # 53–54 RLS with test user B
    _, ca, _ = journal(a, pid, a.device_id)  # read as user A, BEFORE the shared Keychain switches to B
    first_path = ca[0]["path"]
    cdev = Device("C")
    u = cdev.dev("--dev-sign-in", creds, "B")
    probe = cdev.dev("--dev-cloud-probe-foreign", h.EXPECTED["A"], pid, a.device_id, first_path) if u.get("signed_in") else {}
    inv = cdev.dev("--dev-inventory")
    ref = lambda v: isinstance(v, dict) and "Err" in v
    check("53 login of test user B (exact id)", u.get("user_id") == h.EXPECTED["B"], f"user_id={u.get('user_id')}")
    check("54 user B cannot list/download A's journals, read/insert A's store_events, write under A's prefix, see A's mappings/profile",
          probe.get("list_sees") == {"Ok": 0} and probe.get("list_foreign_prefix_sees") == 0 and probe.get("get_first_chunk") == {"Ok": False}
          and probe.get("store_events_sees") == {"Ok": 0} and ref(probe.get("store_event_insert")) and ref(probe.get("put_under_foreign_prefix"))
          and ref(probe.get("overwrite_foreign_chunk")) and probe.get("foreign_mappings_visible") == {"Ok": 0}
          and probe.get("foreign_profile_visible") == {"Ok": False} and inv.get("profiles") == 0 and inv.get("mappings") == 0,
          json.dumps({k: v for k, v in probe.items() if k != "as_user"}))
    a.dev("--dev-sign-in", creds, "A")

    # 55 security
    env = open(os.path.join(h.ROOT, ".env")).read()
    anon = next((l.split("=", 1)[1].strip().strip('"') for l in env.splitlines() if l.startswith("SUPABASE_ANON_KEY=")), "")
    ext = "".join(open(os.path.join(h.EXT, f), errors="replace").read() for f in os.listdir(h.EXT) if os.path.isfile(os.path.join(h.EXT, f)))
    check("55a extension: no anon key, no JWT, no Supabase URL, no network API (fetch/XHR/WebSocket)",
          anon and anon not in ext and not h.JWT.search(ext) and "supabase" not in ext.lower()
          and not re.search(r"\bfetch\(|XMLHttpRequest|WebSocket|EventSource", ext), "")
    for dv in (a, b):
        lg = dv.log()
        leaks = [w for w in ("example.", "Bookmark", "Synchrome Remote", "Folder", "Offline 1", "edited on", "legit", "unsynced", "G1", "G2") if w in lg]
        check(f"55b device {dv.name} log: no URLs, titles, mapping tokens, access/refresh tokens", not leaks and h.secret_free(lg), f"leaks={leaks}")
    os.environ["SYNCHROME_DATA_DIR"] = a.data
    import random
    rnd = "".join(random.choice("0123456789abcdef") for _ in range(64))
    rc, msgs = h.host_session(h.frame({"type": "hello", "ext_version": "e2e", "mapping_token": rnd}) + h.frame({"type": "bookmarks_pull", "pull_id": "p1"}))
    del os.environ["SYNCHROME_DATA_DIR"]
    check("55c remote apply requires a confirmed mapping (unknown token -> no plan, not_confirmed)",
          [m_.get("type") for m_ in msgs] == ["hello_result", "error", "bookmarks_ack"] and msgs[1].get("code") == "not_confirmed", f"{[m_.get('type') for m_ in msgs]}")

    # 56 metrics
    c_all = a.dev("--dev-cloud-tree", pid)
    _, ca, ea = journal(a, pid, a.device_id)
    _, cbj, ebj = journal(a, pid, b.device_id)
    b.stop()
    M.update({
        "ops_published_by_A": sum(len(x["ops"]) for x in ca),
        "legit_ops_published_by_B": sum(len(x["ops"]) for x in cbj),
        "chunks_A": len(ca), "chunks_B": len(cbj), "store_events_A": len(ea), "store_events_B": len(ebj),
        "cursor_B_for_A": b.dev("--dev-remote-cursor", pid, a.device_id).get("applied_through"),
        "cursor_A_for_B": a.dev("--dev-remote-cursor", pid, b.device_id).get("applied_through"),
        "apply_plans_B": len(M["plans"]),
        "ops_applied_total_B": sum(n for n, _ in M["plans"]),
        "conflicts_B": [c_["kind"] for c_ in b.dev("--dev-conflicts", pid).get("conflicts", [])],
        "conflicts_A": [c_["kind"] for c_ in a.dev("--dev-conflicts", pid).get("conflicts", [])],
    })
    print("METRICS " + json.dumps({k: v for k, v in M.items() if k != "applied_results"}))
    check("56 metrics collected", M["chunks_A"] == M["store_events_A"] and M["chunks_B"] == M["store_events_B"], "")

    # 57 cleanup
    a.stop()
    rb = b.dev("--dev-cleanup", b.mid)
    rbd = b.dev("--dev-delete-this-device")
    ra = a.dev("--dev-cleanup", a.mid)
    gone = a.dev("--dev-cloud-tree", pid)
    inv = a.dev("--dev-inventory")
    check("57a cloud: Storage objects, store_events, profile, both mappings, both devices removed",
          rb.get("unmapped") and rbd.get("deleted") and ra.get("unmapped") and ra.get("deleted_profile") and ra.get("device_deleted")
          and gone.get("objects") == 0 and gone.get("store_events") == 0 and inv.get("profiles") == 0 and inv.get("mappings") == 0 and inv.get("devices") == [],
          json.dumps({"B": rb, "B_device": rbd, "A": ra}))
    so = a.dev("--dev-sign-out")
    check("57b Keychain session removed (shared by A and B)", so.get("signed_out") and not h.keychain_has_session(), "")
    for dv in (a, b, cdev):
        shutil.rmtree(dv.udd, ignore_errors=True)
        shutil.rmtree(dv.data, ignore_errors=True)
    check("57c per-device data dirs (device.json, state.sqlite, registry, logs, backups), Helium profiles and host manifests removed",
          not any(os.path.exists(p_) for dv in (a, b, cdev) for p_ in (dv.udd, dv.data)), "")


def emergency():
    print("!! aborted: emergency cleanup")
    for dv in (STATE["A"], STATE["B"]):
        if dv and dv.hl and dv.hl.p.poll() is None:
            dv.hl.p.kill()
    creds = sys.argv[1]
    if STATE["B"] and STATE["B"].mid:
        STATE["B"].dev("--dev-sign-in", creds, "A")
        print("   B:", STATE["B"].dev("--dev-cleanup", STATE["B"].mid), STATE["B"].dev("--dev-delete-this-device"))
    if STATE["A"] and STATE["A"].mid:
        print("   A:", STATE["A"].dev("--dev-cleanup", STATE["A"].mid))
    if STATE["A"]:
        STATE["A"].dev("--dev-sign-out")


if __name__ == "__main__":
    try:
        main(sys.argv[1])
    except Exception as e:  # noqa: BLE001
        import traceback
        traceback.print_exc()
        check("harness completed without exception", False, type(e).__name__ + ": " + str(e)[:200])
        emergency()
    failed = [n for n, ok in h.results if not ok]
    print(f"\n=== remote apply: {len(h.results) - len(failed)} pass, {len(failed)} fail ===")
    if failed:
        print("  failures: " + ", ".join(failed))
    sys.exit(1 if failed else 0)
