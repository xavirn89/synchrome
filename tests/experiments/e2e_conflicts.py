"""Two-device E2E of bookmark conflict resolution (M1 slice 9). Disposable profiles only.

Same rig as e2e_remote_apply.py: devices A and B are two simulated installations on this
machine (own SYNCHROME_DATA_DIR each: device.json, state.sqlite, registry, logs), both signed
in as test user A, each with its own disposable Helium --user-data-dir. Test user B is used
only for the isolation probes. A change is kept *unpublished* by making it while that device
runs offline (`connected_offline`: captured, not uploaded). Conflicts are resolved through
`--dev-resolve` (the app's Rust path) and, in case 15, by clicking the REAL app window
(embedded frontend, debug-only UI driver). Prints ids, counts, field/group names, plan op
codes and fixture titles only — never URLs (hosts at most), tokens or passwords. Every
process it starts is tracked by PID and only those PIDs are stopped.

  EXPECTED_A_ID=… EXPECTED_B_ID=… python3 e2e_conflicts.py <creds-env-file>
"""
import json, os, re, shutil, subprocess, sys, time, uuid

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import e2e_handshake as h  # noqa: E402
import e2e_bookmarks as eb  # noqa: E402
import e2e_remote_apply as ra  # noqa: E402

check = h.check
find = eb.find
flat = ra.flat
UUID = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
M = {
    "created": {},
    "resolutions": {"keep_local": 0, "keep_remote": 0, "merge": 0, "retry": 0, "relocate": 0},
    "resolution_ops": 0,
    "conv_ms": {},
}
UI_PROCS = []


# ------------------------------------------------------------------ rig
def _start(self, offline=False):
    """Like ra.Device.start, but tolerant to a worker that is not ready on the first eval."""
    env = dict(self.env)
    if offline:
        env["SUPABASE_URL"] = eb.OFFLINE_URL
    self.hl = eb.Helium(self.udd, env=env)
    for i in range(20):
        try:
            # deterministic harness: no background polling, only the pulls this script triggers
            self.hl.js("chrome.alarms.clearAll().then(() => true)")
            break
        except RuntimeError:
            if i == 19:
                raise
            time.sleep(0.5)
    return self.hl


ra.Device.start = _start


# ------------------------------------------------------------------ helpers
def titles(d):
    return [r[1] for r in flat(d.tree())]


def node(d, title):
    return next(((t_, u_) for _, t_, u_ in flat(d.tree()) if t_ == title), (None, None))


def host(u):
    return u.split("/")[2] if u else None


def views(d, closed=False):
    return d.dev("--dev-conflict-views", d.mid, *(["all"] if closed else [])).get("conflicts") or []


def open_views(d):
    return [c for c in views(d) if c["status"] in ("unresolved", "resolving")]


def by_name(d, name):
    return next((c for c in views(d, True) if c["name"] == name or (c.get("remote") or {}).get("title") == name), {})


def note(d):
    for c in views(d, True):
        M["created"].setdefault(c["conflict_id"], f"{c['kind']}/{c['subtype']}")


def resolve(d, cid, res):
    r = d.dev("--dev-resolve", d.mid, cid, res)
    out = (r.get("results") or [{}])[0]
    if out.get("outcome") in ("resolved", "resolving"):
        M["resolutions"][res] += 1
        M["resolution_ops"] += out.get("ops", 0)
    return out


def stop(d):
    if d.hl:
        try:
            M.setdefault("suppressed", {}).setdefault(d.name, 0)
            M["suppressed"][d.name] += int(d.hl.js("suppressedEvents") or 0)
        except Exception:  # noqa: BLE001
            pass
        d.hl.stop()
        d.hl = None


def offline_edit(d, js):
    """A local change on `d` that stays unpublished (device offline); `d` is stopped after."""
    stop(d)
    t = eb.now_ms()
    d.start(offline=True)
    d.hl.wait(lambda s: s.get("status") == "connected_offline", 60, after=t)
    ra.wait(d.idle, 30)
    before = d.status().get("pending", 0)
    d.hl.js(f"(async () => {{ {js}; return true; }})()")
    ok = ra.wait(lambda: d.status().get("pending", 0) > before and d.idle(), 60)
    stop(d)
    return bool(ok)


def online(d, until=None, timeout=90):
    """Start `d` online (its worker-start cycle: snapshot, events, pull, apply). Returns the
    parsed host-log slice of that start."""
    stop(d)
    mark = len(d.log().encode())
    t = eb.now_ms()
    d.start()
    d.hl.wait(lambda s: s.get("status") == "connected", 60, after=t)
    ra.wait(lambda: d.idle() and (until is None or until()), timeout)
    return ra.parse(open(os.path.join(d.data, "logs", "native-host.log"), "rb").read()[mark:].decode("utf-8", "replace"))


def dl(d):
    return d.status().get("download") or {}


def converged(a, b, pid):
    return a.tree() == b.tree() and ra.same_identity_tree(a, b, pid)


def sync_both(a, b, rounds=2):
    for _ in range(rounds):
        ra.pull_b(b)
        ra.pull_b(a)


def jops(a, pid, dev):
    return [o for x in ra.journal(a, pid, dev)[1] for o in x["ops"]]


def conflict_pair(a, b, x, b_js, a_js, pid, until_n=None):
    """B changes X offline (unpublished), A changes X and publishes, B comes online."""
    n_before = len(jops(a, pid, b.device_id))
    ok_pending = offline_edit(b, b_js)
    unpublished = len(jops(a, pid, b.device_id)) == n_before
    ra.edit_and_publish(a, a_js)
    want = until_n or (len(open_views(b)) + 1)
    sl = online(b, until=lambda: dl(b).get("conflicts", 0) >= want)
    note(b)
    return ok_pending and unpublished, sl, time.time()


# ------------------------------------------------------------------ real UI driver
class UI:
    """The Synchrome app (embedded frontend) on device B's data dir, driven through the
    debug-only SYNCHROME_DEV_UI_DRIVER directory."""

    def __init__(self, dev):
        self.dir = os.path.join(ra.WORK, "ui-driver")
        shutil.rmtree(self.dir, ignore_errors=True)
        os.makedirs(os.path.join(self.dir, "in"))
        self.log = open(os.path.join(ra.WORK, "ui-app.log"), "w")
        e = dict(os.environ)
        e.update({"SYNCHROME_DATA_DIR": dev.data, "SYNCHROME_DEV_UI_DRIVER": self.dir})
        self.p = subprocess.Popen([h.APP], env=e, stdout=self.log, stderr=self.log)
        UI_PROCS.append(self.p)
        self.n = 0
        self.ready = ra.wait(lambda: self.js("return document.readyState === 'complete' && !!document.querySelector('main')", 3), 60, 1)

    def js(self, body, timeout=20):
        self.n += 1
        rid = uuid.uuid4().hex
        wrapper = ("(async () => { let r; try { r = {ok: true, v: await (async () => { %s })()}; }"
                   " catch (e) { r = {ok: false, e: String(e)}; }"
                   " window.__TAURI_INTERNALS__.invoke('dev_ui_report', {id: '%s', value: r}); })();") % (body, rid)
        with open(os.path.join(self.dir, "in", f"{self.n:06d}.js"), "w") as f:
            f.write(wrapper)
        out = os.path.join(self.dir, "out", f"{rid}.json")
        end = time.time() + timeout
        while time.time() < end:
            if os.path.exists(out):
                r = json.load(open(out))
                os.remove(out)
                return r.get("v") if r.get("ok") else {"__error": r.get("e")}
            time.sleep(0.2)
        return None

    def until(self, body, timeout=30):
        return ra.wait(lambda: self.js(body, 5), timeout, 0.5)

    def stop(self):
        if self.p.poll() is None:
            self.p.terminate()
            try:
                self.p.wait(10)
            except subprocess.TimeoutExpired:
                self.p.kill()
        self.log.close()


UI_JS = r"""
const all = (sel, root = document) => [...root.querySelectorAll(sel)];
const btn = (root, prefix) => all('button', root).find(b => b.textContent.trim().startsWith(prefix));
const card = (name) => all('.conflict').find(c => (c.querySelector('strong')?.textContent || '').includes('“' + name + '”'));
const badge = () => (all('.badge').find(b => b.textContent.trim().startsWith('Conflict ·'))?.textContent || '').trim();
"""


def restart_app(dev):
    ui = UI(dev)
    ok = bool(ui.ready)
    ui.stop()
    return ok


# ------------------------------------------------------------------ main
def main(creds):
    h.prepare_binary()
    os.makedirs(ra.WORK, exist_ok=True)
    a, b = ra.Device("A"), ra.Device("B")
    print("=== two-device conflict resolution E2E ===")
    check("pre: signed out, no Keychain session", a.dev("--dev-account").get("signed_in") is False and not h.keychain_has_session())
    sa = a.dev("--dev-sign-in", creds, "A")
    check("pre: login user A for device A (exact id)", sa.get("user_id") == h.EXPECTED["A"], f"user_id={sa.get('user_id')}")
    if not sa.get("signed_in"):
        return
    sb = b.dev("--dev-sign-in", creds, "A")
    check("pre: login user A for device B (exact id)", sb.get("user_id") == h.EXPECTED["A"], f"user_id={sb.get('user_id')}")
    a.device_id = a.dev("--dev-register-device").get("device_id")
    b.device_id = b.dev("--dev-register-device").get("device_id")
    check("pre: distinct device ids, data dirs (state.sqlite, registry, logs)", a.device_id and b.device_id and a.device_id != b.device_id and a.data != b.data,
          f"A={a.device_id} B={b.device_id}")

    # 1 mappings
    m = a.dev("--dev-create-mapping", f"E2E CF {os.urandom(3).hex()}", "helium", "Default")
    a.mid, pid = m.get("mapping_id"), m.get("profile_id")
    a.dev("--dev-write-token", a.mid, a.udd)
    a.dev("--dev-install-host", a.udd)
    t = eb.now_ms()
    a.start()
    sta = a.hl.wait(lambda s: s.get("status") == "connected" and s.get("token_state") == "confirmed", 60, after=t)
    ra.wait(a.idle, 30)
    mb = b.dev("--dev-map-existing", pid, "helium", "Default")
    b.mid = mb.get("mapping_id")
    b.dev("--dev-write-token", b.mid, b.udd)
    b.dev("--dev-install-host", b.udd)
    t = eb.now_ms()
    b.start()
    stb = b.hl.wait(lambda s: s.get("status") == "connected" and s.get("token_state") == "confirmed", 60, after=t)
    ra.wait(b.idle, 30)
    check("1 same cloud profile; mappings of A and B confirmed", sta is not None and stb is not None and mb.get("profile_id") == pid, f"profile={pid}")

    # 2 fixture
    ra.edit_and_publish(a, """
      const T = await chrome.bookmarks.create({parentId: "1", title: "Synchrome Conflict Test"});
      const mk = async (p, t, u) => chrome.bookmarks.create(u ? {parentId: p, title: t, url: u} : {parentId: p, title: t});
      const fa = await mk(T.id, "Folder A");
      for (const i of [1, 2, 3, 4]) await mk(fa.id, "K" + i, "https://example.com/k" + i);
      const fd = await mk(T.id, "Folder D");
      for (const n of ["E1", "E2", "R1", "R2"]) await mk(fd.id, n, "https://example.com/" + n.toLowerCase());
      const ff = await mk(T.id, "Folder F"); await mk(ff.id, "F synced", "https://example.com/f");
      const fp = await mk(T.id, "Folder P"); await mk(fp.id, "P child C", "https://example.com/c");
      const fh = await mk(T.id, "Folder H");
      for (const i of [1, 2, 3, 4]) await mk(fh.id, "H" + i, "https://example.com/h" + i);
      const fu = await mk(T.id, "Folder U");
      for (const i of [1, 2, 3, 4]) await mk(fu.id, "U" + i, "https://example.com/u" + i);
      await mk(T.id, "Y", "https://example.com/y")""")
    ra.pull_b(b)
    check("2 bookmark fixture created on A, synced to B (same tree, same identities)", converged(a, b, pid), f"{len(titles(b))} nodes")
    sid = lambda title: next((r[6] for r in a.dev("--dev-local-tree", pid)["tree"] if r[3] == title), None)

    # ---------------------------------------------------------------- CASE 1 + CASE 9
    x = sid("K1")
    b_before = len(jops(a, pid, b.device_id))
    ok_off = offline_edit(b, f"await chrome.bookmarks.update(({find('K1')}).id, {{title: 'K1 by B'}})")
    check("3-5 B offline changed X's title; the change is pending locally and NOT in the cloud",
          ok_off and b.status().get("pending", 0) >= 1 and len(jops(a, pid, b.device_id)) == b_before, f"pending={b.status().get('pending')}")
    ra.edit_and_publish(a, f"await chrome.bookmarks.update(({find('K1')}).id, {{title: 'K1 by A'}})")
    a_x_op = [o for o in jops(a, pid, a.device_id) if o["id"] == x][-1]
    check("6 A changed X's title too and published", a_x_op["fields"] == ["title"], "")
    online(b, until=lambda: dl(b).get("state") == "conflict")
    t1 = time.time()
    note(b)
    c1 = by_name(b, "K1 by B")
    check("7-8 B online + pull: conflict local_pending/edit_edit, state Conflict",
          c1.get("kind") == "local_pending" and c1.get("subtype") == "edit_edit" and c1.get("status") == "unresolved" and dl(b).get("state") == "conflict",
          json.dumps({k: c1.get(k) for k in ("kind", "subtype", "status", "held")}))
    check("9 snapshots: Local 'K1 by B', Remote 'K1 by A' (device A, seq), Base 'K1'",
          c1["local"]["title"] == "K1 by B" and c1["remote"]["title"] == "K1 by A" and c1.get("remote_device") == a.device_id
          and c1.get("remote_seq") == a_x_op["seq"] and (c1.get("base") or {}).get("title") == "K1", "")
    b_x_op = [o for o in jops(a, pid, b.device_id) if o["id"] == x][-1]  # B's own op, published by the sync
    # CASE 9: Y keeps syncing while X is in conflict
    ra.edit_and_publish(a, f"await chrome.bookmarks.update(({find('Y')}).id, {{title: 'Y v2'}})")
    ra.pull_b(b)
    check("66-68 with X in conflict, A changed Y: B applied Y normally", "Y v2" in titles(b), "")
    check("69-70 X stays frozen (local value), the profile is not blocked (no remote changes pending)",
          "K1 by B" in titles(b) and "K1 by A" not in titles(b) and len(open_views(b)) == 1 and dl(b).get("remote_pending") == 0, "")
    n0 = len(jops(a, pid, b.device_id))
    r = resolve(b, c1["conflict_id"], "keep_local")
    new = jops(a, pid, b.device_id)[n0:]
    check("10-11 Keep local: resolved; exactly one new op from B on X",
          r.get("outcome") == "resolved" and r.get("ops") == 1 and len(new) == 1 and new[0]["id"] == x and new[0]["fields"] == ["title"], json.dumps(r))
    check("12 its HLC is later than both the local (B) and the remote (A) versions",
          bool(new) and new[0]["hlc"] > b_x_op["hlc"] and new[0]["hlc"] > a_x_op["hlc"],
          f"res={new[0]['hlc'] if new else None} local={b_x_op['hlc']} remote={a_x_op['hlc']}")
    check("13 published (in the cloud journal + store_events of B)", bool(new), "")
    ra.pull_b(a)
    ra.pull_b(b)
    check("14-15 A received it; A and B both show B's value; converged", node(a, "K1 by B")[0] and converged(a, b, pid), "")
    h1 = by_name(b, "K1 by B")
    check("16-17 conflict resolved; history records Keep local", h1.get("status") == "resolved" and h1.get("resolution") == "keep_local" and h1.get("resolved_at"), "")
    M["conv_ms"]["case1_keep_local"] = int((time.time() - t1) * 1000)

    # ---------------------------------------------------------------- CASE 2
    _, _, t2 = conflict_pair(a, b, sid("K2"), f"await chrome.bookmarks.update(({find('K2')}).id, {{title: 'K2 by B'}})",
                             f"await chrome.bookmarks.update(({find('K2')}).id, {{title: 'K2 by A'}})", pid)
    c2 = by_name(b, "K2 by B")
    check("18 new edit_edit conflict", c2.get("subtype") == "edit_edit" and c2.get("status") == "unresolved", "")
    r = resolve(b, c2["conflict_id"], "keep_remote")
    n1 = len(jops(a, pid, b.device_id))
    st_between = by_name(b, "K2 by B").get("status")
    sup0 = int(b.hl.js("suppressedEvents") or 0)
    sl, _ = ra.pull_b(b)
    plans = [p_ for p_ in sl["plans"] if p_[0]]
    n2 = len(jops(a, pid, b.device_id))
    check("19-20 Keep remote: B applied A's version through chrome.bookmarks (rename)",
          r.get("outcome") == "resolving" and plans and "rn" in plans[0][2].split(",") and "K2 by A" in titles(b), json.dumps(plans))
    check("21-22 no echo: no op after the apply; the post-apply snapshot = 0 ops; apply events suppressed",
          n2 == n1 and sl["snapshots"][-1:] == ["Ok(Applied { ops: 0, pending: 0 })"] and int(b.hl.js("suppressedEvents") or 0) > sup0,
          json.dumps({"ops_after_apply": n2 - n1, "snapshots": sl["snapshots"]}))
    ra.pull_b(a)
    check("23 A and B converge", converged(a, b, pid), "")
    check("24 'resolving' until the apply, 'resolved' only after it", st_between == "resolving" and by_name(b, "K2 by A").get("status") == "resolved",
          f"between={st_between}")
    M["conv_ms"]["case2_keep_remote"] = int((time.time() - t2) * 1000)

    # ---------------------------------------------------------------- CASE 3
    check("25 stable bookmark K3 (identical on A and B)", node(a, "K3") == node(b, "K3") and node(b, "K3")[0] == "K3", "")
    _, _, t3 = conflict_pair(a, b, sid("K3"), f"await chrome.bookmarks.update(({find('K3')}).id, {{title: 'K3 by B'}})",
                             f"await chrome.bookmarks.update(({find('K3')}).id, {{url: 'https://example.org/k3-by-a'}})", pid)
    c3 = by_name(b, "K3 by B")
    check("26-28 B offline title, A URL: B receives the conflict (local=[title], remote=[url])",
          c3.get("subtype") == "edit_edit" and c3["local"]["changed"] == ["title"] and c3["remote"]["changed"] == ["url"], "")
    check("29 Merge available (offered first)", [o["resolution"] for o in c3.get("options", [])] == ["merge", "keep_local", "keep_remote"], "")
    n0 = len(jops(a, pid, b.device_id))
    r = resolve(b, c3["conflict_id"], "merge")
    new = jops(a, pid, b.device_id)[n0:]
    sync_both(a, b)
    nb, na = node(b, "K3 by B"), node(a, "K3 by B")
    check("30-31 Merge: B's title + A's URL on both devices",
          r.get("outcome") in ("resolving", "resolved") and nb[0] == na[0] == "K3 by B" and nb[1] == na[1] and host(nb[1]) == "example.org" and nb[1].endswith("k3-by-a"),
          json.dumps({"A": host(na[1]), "B": host(nb[1])}))
    check("32 resolution published as one normal op [title, url]", len(new) == 1 and new[0]["fields"] == ["title", "url"], json.dumps(new))
    check("33-34 converged; conflict resolved", converged(a, b, pid) and by_name(b, "K3 by B").get("status") == "resolved", "")
    M["conv_ms"]["case3_merge"] = int((time.time() - t3) * 1000)

    # ---------------------------------------------------------------- CASE 4
    _, _, t4 = conflict_pair(a, b, sid("K4"), f"await chrome.bookmarks.update(({find('K4')}).id, {{title: 'K4 by B'}})",
                             f"await chrome.bookmarks.update(({find('K4')}).id, {{title: 'K4 by A'}})", pid)
    c4 = by_name(b, "K4 by B")
    bad = resolve(b, c4["conflict_id"], "merge")
    check("35-37 both changed the title: Merge NOT offered (only Keep local / Keep remote) and refused if requested",
          [o["resolution"] for o in c4.get("options", [])] == ["keep_local", "keep_remote"] and bad.get("outcome") == "error"
          and by_name(b, "K4 by B").get("status") == "unresolved", json.dumps(bad))
    resolve(b, c4["conflict_id"], "keep_local")
    sync_both(a, b, 1)
    M["conv_ms"]["case4_keep_local"] = int((time.time() - t4) * 1000)

    # ---------------------------------------------------------------- CASES 5 + 6 (one offline session, 4 conflicts)
    e1_sid = sid("E1")
    offline_edit(b, f"""
      await chrome.bookmarks.update(({find('E1')}).id, {{title: 'E1 by B'}});
      await chrome.bookmarks.update(({find('E2')}).id, {{title: 'E2 by B'}});
      await chrome.bookmarks.remove(({find('R1')}).id);
      await chrome.bookmarks.remove(({find('R2')}).id)""")
    ra.edit_and_publish(a, f"""
      await chrome.bookmarks.remove(({find('E1')}).id);
      await chrome.bookmarks.remove(({find('E2')}).id);
      await chrome.bookmarks.update(({find('R1')}).id, {{title: 'R1 by A'}});
      await chrome.bookmarks.update(({find('R2')}).id, {{title: 'R2 by A', url: 'https://example.org/r2-by-a'}})""")
    online(b, until=lambda: dl(b).get("conflicts", 0) >= 4)
    t5 = time.time()
    note(b)
    e1, e2, r1, r2 = by_name(b, "E1 by B"), by_name(b, "E2 by B"), by_name(b, "R1 by A"), by_name(b, "R2 by A")
    destr = lambda c, r_: next((o["destructive"] for o in c.get("options", []) if o["resolution"] == r_), None)
    check("38-40 B edited X offline, A deleted X: B receives edit_delete (x2); nothing deleted yet",
          e1.get("subtype") == e2.get("subtype") == "edit_delete" and "E1 by B" in titles(b) and "E2 by B" in titles(b), "")
    check("44-46 B deleted X offline, A edited X: delete_edit (x2); nothing restored yet",
          r1.get("subtype") == r2.get("subtype") == "delete_edit" and "R1 by A" not in titles(b) and "R2 by A" not in titles(b), "")
    check("delete choices that remove data still present on one side are flagged for confirmation",
          destr(e1, "keep_remote") is True and destr(r1, "keep_local") is True and destr(e1, "keep_local") is False and destr(r1, "keep_remote") is False, "")
    n0 = len(jops(a, pid, b.device_id))
    re1 = resolve(b, e1["conflict_id"], "keep_local")
    new = [o for o in jops(a, pid, b.device_id)[n0:] if o["id"] == e1_sid]
    ra.pull_b(a)
    check("41 edit vs delete, Keep local: X stays on B, one COMPLETE op (all fields), A gets X back",
          re1.get("outcome") == "resolved" and len(new) == 1 and set(new[0]["fields"] or []) >= {"parentId", "pos", "title", "kind", "root", "url"}
          and "E1 by B" in titles(b) and "E1 by B" in titles(a), json.dumps(new[0]["fields"] if new else None))
    n1 = len(jops(a, pid, b.device_id))
    re2 = resolve(b, e2["conflict_id"], "keep_remote")
    n2 = len(jops(a, pid, b.device_id))
    sl, _ = ra.pull_b(b)
    check("42-43 edit vs delete again, Keep remote: X deleted on B, echo 0 (no op after the apply, snapshot 0 ops)",
          re2.get("outcome") == "resolving" and "E2 by B" not in titles(b) and len(jops(a, pid, b.device_id)) == n2 and n2 - n1 == re2.get("ops")
          and sl["snapshots"][-1:] == ["Ok(Applied { ops: 0, pending: 0 })"], json.dumps(sl["snapshots"]))
    rr1 = resolve(b, r1["conflict_id"], "keep_local")
    rr2 = resolve(b, r2["conflict_id"], "keep_remote")
    sync_both(a, b)
    n_r2 = node(b, "R2 by A")
    check("47 delete vs edit, Keep local: the delete wins, A ends without X", rr1.get("outcome") == "resolved" and "R1 by A" not in titles(a) and "R1 by A" not in titles(b), "")
    check("48-49 delete vs edit again, Keep remote: X restored on B with A's title and URL, in Folder D",
          rr2.get("outcome") in ("resolving", "resolved") and n_r2[0] == "R2 by A" and host(n_r2[1]) == "example.org"
          and any(n["t"] == "R2 by A" for f in b.tree()[0]["c"] if f["t"] == "Folder D" for n in f["c"]), "")
    check("43/49 converged after the four delete conflicts (same identities); all resolved",
          converged(a, b, pid) and all(by_name(b, n_).get("status") == "resolved" for n_ in ("E1 by B", "E2 by B", "R1 by A", "R2 by A")),
          json.dumps([by_name(b, n_).get("status") for n_ in ("E1 by B", "E2 by B", "R1 by A", "R2 by A")]))
    M["conv_ms"]["case5_6_deletes_x4"] = int((time.time() - t5) * 1000)

    # ---------------------------------------------------------------- CASE 7
    offline_edit(b, f"await chrome.bookmarks.create({{parentId: ({find('Folder F')}).id, title: 'F local child', url: 'https://example.net/f-local'}})")
    ra.edit_and_publish(a, f"await chrome.bookmarks.removeTree(({find('Folder F')}).id)")
    online(b, until=lambda: dl(b).get("conflicts", 0) >= 1)
    t7 = time.time()
    note(b)
    c7 = by_name(b, "Folder F")
    check("50-53 folder F with B's unsynced child; A deleted F: delete_not_empty -> apply_failed, F + child kept, synced child deleted",
          c7.get("kind") == "apply_failed" and c7.get("subtype") == "delete_not_empty" and c7.get("local_items") == 1
          and "Folder F" in titles(b) and "F local child" in titles(b) and "F synced" not in titles(b),
          json.dumps({k: c7.get(k) for k in ("kind", "subtype", "attempts", "local_items", "detail")}))
    r = resolve(b, c7["conflict_id"], "retry")
    ra.pull_b(b)
    c7b = by_name(b, "Folder F")
    check("54 Retry with the child present: fails again, child intact, conflict still open (attempts 2)",
          r.get("outcome") == "resolving" and c7b.get("status") == "unresolved" and c7b.get("attempts") == 2 and "F local child" in titles(b), "")
    r = resolve(b, c7["conflict_id"], "relocate")
    sync_both(a, b)
    root_b = [n["t"] for n in b.tree()[0]["c"]]
    check("55 Relocate: the child moved to the valid parent ('Synchrome Conflict Test'), then F deleted; nothing lost",
          "F local child" in root_b and "Folder F" not in titles(b) and by_name(b, "Folder F").get("status") == "resolved", json.dumps(r))
    check("56 A and B converge (A has the child, no F)", "F local child" in [n["t"] for n in a.tree()[0]["c"]] and converged(a, b, pid) and not open_views(a), "")
    M["conv_ms"]["case7_apply_failed"] = int((time.time() - t7) * 1000)

    # ---------------------------------------------------------------- CASE 8
    check("57 folder P with child C exists on both", "P child C" in titles(a) and "P child C" in titles(b), "")
    offline_edit(b, f"await chrome.bookmarks.removeTree(({find('Folder P')}).id)")
    check("58 B deleted P locally (unpublished)", "Folder P" in titles(a), "")
    ra.edit_and_publish(a, f"await chrome.bookmarks.create({{parentId: ({find('Folder P')}).id, title: 'C2', url: 'https://example.net/c2'}})")
    online(b, until=lambda: dl(b).get("conflicts", 0) >= 1)
    t8 = time.time()
    note(b)
    c8 = by_name(b, "C2")
    check("59-60 A created C2 inside P: B receives an orphan (C2 neither dropped nor created yet)",
          c8.get("kind") == "orphan" and "C2" not in titles(b), json.dumps({k: c8.get(k) for k in ("kind", "status")}))
    check("61 options: relocate / restore folder (keep_remote) / accept deletion (keep_local, destructive)",
          [o["resolution"] for o in c8.get("options", [])] == ["relocate", "keep_remote", "keep_local"] and destr(c8, "keep_local") is True, "")
    r = resolve(b, c8["conflict_id"], "relocate")
    ra.pull_b(b)
    root_b = [n["t"] for n in b.tree()[0]["c"]]
    check("62-63 C2 kept in the nearest valid folder ('Synchrome Conflict Test'); not lost", "C2" in root_b, json.dumps(r))
    check("64 orphan resolved", by_name(b, "C2").get("status") == "resolved", by_name(b, "C2").get("status"))
    sync_both(a, b)
    check("65 no 'Recovered by Synchrome' folder anywhere; A and B converge (P deleted, C2 kept)",
          not any("Recovered" in t_ for t_ in titles(a) + titles(b)) and "Folder P" not in titles(a) and "C2" in titles(a)
          and converged(a, b, pid) and not open_views(a) and not open_views(b), "")
    M["conv_ms"]["case8_orphan"] = int((time.time() - t8) * 1000)

    # ---------------------------------------------------------------- CASE 10 retained ops
    x10 = sid("H1")
    _, _, t10 = conflict_pair(a, b, x10, f"await chrome.bookmarks.update(({find('H1')}).id, {{title: 'H1 by B'}})",
                              f"await chrome.bookmarks.update(({find('H1')}).id, {{title: 'H1 a1'}})", pid)
    ra.edit_and_publish(a, f"await chrome.bookmarks.update(({find('H1 a1')}).id, {{title: 'H1 a2'}})")
    ra.edit_and_publish(a, f"await chrome.bookmarks.update(({find('H1 a2')}).id, {{url: 'https://example.org/h1-a3'}})")
    ra.pull_b(b)
    c10 = by_name(b, "H1 by B")
    held = b.dev("--dev-conflict-held", c10.get("conflict_id", "")).get("held", [])
    a_ops = [o for o in jops(a, pid, a.device_id) if o["id"] == x10][-3:]
    check("71-72 two more remote ops on X while in conflict: all three held, in order (device, seq, HLC)",
          len(held) == 3 and [hh["seq"] for hh in held] == [o["seq"] for o in a_ops] and [hh["hlc"] for hh in held] == sorted(hh["hlc"] for hh in held)
          and all(hh["device"] == a.device_id for hh in held) and c10.get("held") == 3 and "H1 by B" in titles(b),
          json.dumps([(hh["seq"], hh["device"][:8]) for hh in held]))
    cur = b.dev("--dev-remote-cursor", pid, a.device_id).get("applied_through")
    r = resolve(b, c10["conflict_id"], "keep_remote")
    ra.pull_b(b)
    n10 = node(b, "H1 a2")
    check("73-75 Keep remote: every held op is applied in order (last title a2 + URL a3; nothing skipped)",
          n10[0] == "H1 a2" and n10[1] and n10[1].endswith("h1-a3") and "H1 a1" not in titles(b), json.dumps({"host": host(n10[1])}))
    ra.edit_and_publish(a, f"await chrome.bookmarks.update(({find('H1 a2')}).id, {{title: 'H1 a4'}})")
    ra.pull_b(b)
    ra.pull_b(a)
    check("76 a later remote op flows normally after the resolution (no new conflict, cursor moves on, in order)",
          "H1 a4" in titles(b) and not open_views(b) and b.dev("--dev-remote-cursor", pid, a.device_id).get("applied_through") > cur
          and converged(a, b, pid), "")
    check("76b no spurious conflict on A: B's resolution only re-asserted A's own previous value (three-way BASE rule)",
          not open_views(a), json.dumps([(c_["kind"], c_["subtype"], c_["status"]) for c_ in open_views(a)]))
    M["conv_ms"]["case10_retained"] = int((time.time() - t10) * 1000)

    # ---------------------------------------------------------------- CASE 11 persistence
    _, _, t11 = conflict_pair(a, b, sid("H2"), f"await chrome.bookmarks.update(({find('H2')}).id, {{title: 'H2 by B'}})",
                              f"await chrome.bookmarks.update(({find('H2')}).id, {{title: 'H2 by A'}})", pid)
    c11 = by_name(b, "H2 by B")
    check("77 conflict created", c11.get("status") == "unresolved", "")
    app1 = restart_app(b)
    stop(b)
    online(b)
    c11b = by_name(b, "H2 by B")
    keep = lambda c: {k: c.get(k) for k in ("conflict_id", "kind", "subtype", "status", "local", "remote", "base", "created_at")}
    check("78-81 after restarting app B and Helium B: same conflict (id, status) with its Local/Remote/Base snapshots",
          app1 and keep(c11b) == keep(c11) and c11b["base"] and c11b["local"]["title"] == "H2 by B" and c11b["remote"]["title"] == "H2 by A", "")
    r = resolve(b, c11["conflict_id"], "keep_local")
    app2 = restart_app(b)
    stop(b)
    online(b)
    c11c = by_name(b, "H2 by B")
    check("82-85 resolved, restarted again: history present (Keep local), never back to unresolved",
          r.get("outcome") == "resolved" and app2 and c11c.get("status") == "resolved" and c11c.get("resolution") == "keep_local" and not open_views(b), "")
    sync_both(a, b, 1)
    M["conv_ms"]["case11_persistence"] = int((time.time() - t11) * 1000)

    # ---------------------------------------------------------------- CASE 12 idempotency
    _, _, t12 = conflict_pair(a, b, sid("H3"), f"await chrome.bookmarks.update(({find('H3')}).id, {{title: 'H3 by B'}})",
                              f"await chrome.bookmarks.update(({find('H3')}).id, {{title: 'H3 by A'}})", pid)
    c12 = by_name(b, "H3 by B")
    r1_ = resolve(b, c12["conflict_id"], "keep_local")
    n0, hist0, st0 = len(jops(a, pid, b.device_id)), len(views(b, True)), by_name(b, "H3 by B")
    r2_ = resolve(b, c12["conflict_id"], "keep_local")
    n1, hist1, st1 = len(jops(a, pid, b.device_id)), len(views(b, True)), by_name(b, "H3 by B")
    check("86-90 resolve, then the same request again: already_resolved, no second op, no duplicate history, state unchanged",
          r1_.get("outcome") == "resolved" and r2_.get("outcome") == "already_resolved" and n1 == n0 and hist1 == hist0
          and st1 == st0, json.dumps({"first": r1_, "second": r2_}))
    sync_both(a, b, 1)
    M["conv_ms"]["case12_idempotent"] = int((time.time() - t12) * 1000)

    # ---------------------------------------------------------------- CASE 13 superseded
    offline_edit(b, f"await chrome.bookmarks.update(({find('H4')}).id, {{title: 'H4 same'}})")
    ra.edit_and_publish(a, f"await chrome.bookmarks.update(({find('H4')}).id, {{title: 'H4 same'}})")
    sl = online(b, until=lambda: dl(b).get("state") == "up_to_date")
    note(b)
    c13 = by_name(b, "H4 same")
    check("91-92 both sides converged on the same value: the conflict is superseded (no decision asked)",
          c13.get("status") == "superseded" and not open_views(b), json.dumps({"status": c13.get("status"), "detail": c13.get("detail")}))
    check("94 no unnecessary apply (the start cycle planned 0 browser ops)", not any(p_[0] for p_ in sl["plans"]), json.dumps(sl["plans"]))
    n0 = len(jops(a, pid, b.device_id))
    ra.edit_and_publish(b, f"await chrome.bookmarks.update(({find('H4 same')}).id, {{title: 'H4 after'}})")
    check("93 the entity is not frozen: a later local edit is captured and published", len(jops(a, pid, b.device_id)) == n0 + 1, "")
    sync_both(a, b, 1)
    check("13b converged", converged(a, b, pid), "")

    # ---------------------------------------------------------------- CASE 15 real UI
    offline_edit(b, f"""
      await chrome.bookmarks.update(({find('U1')}).id, {{title: 'U1 by B'}});
      await chrome.bookmarks.update(({find('U2')}).id, {{title: 'U2 by B'}});
      await chrome.bookmarks.update(({find('U3')}).id, {{title: 'U3 by B'}});
      await chrome.bookmarks.update(({find('U4')}).id, {{title: 'U4 by B'}})""")
    ra.edit_and_publish(a, f"""
      await chrome.bookmarks.update(({find('U1')}).id, {{title: 'U1 by A'}});
      await chrome.bookmarks.update(({find('U2')}).id, {{title: 'U2 by A'}});
      await chrome.bookmarks.update(({find('U3')}).id, {{url: 'https://example.org/u3-by-a'}});
      await chrome.bookmarks.remove(({find('U4')}).id)""")
    online(b, until=lambda: dl(b).get("conflicts", 0) >= 4)
    t15 = time.time()
    note(b)
    ui = UI(b)
    check("98 Synchrome app (embedded frontend, B's installation) opened and signed in", bool(ui.ready) and bool(ui.until(UI_JS + "return document.body.innerText.includes('Signed in')")), "")
    badge = lambda: ui.js(UI_JS + "return badge();")
    check("99 Conflict counter visible: 'Conflict · 4'", ui.until(UI_JS + "return badge() === 'Conflict · 4';", 40), str(badge()))
    ui.js(UI_JS + "btn(document, 'Review conflicts').click(); return true;")
    shown = ui.until(UI_JS + "return all('.conflict').length === 4;")
    check("100 Review conflicts opens the panel with the 4 conflicts", bool(shown), "")
    info = ui.js(UI_JS + """
      const c = card('U1 by B'); const th = all('th', c).map(x => x.textContent.trim());
      return {th, diff: all('tr.diff', c).map(r => r.cells[0].textContent), meta: c.querySelector('.pdir').nextElementSibling?.textContent || '',
              text: c.innerText};""") or {}
    check("101 comparison columns: This device | <remote device name> | Before",
          isinstance(info, dict) and info.get("th", [None])[1:2] == ["This device"] and len(info.get("th", [])) == 4 and info["th"][3] == "Before"
          and info["th"][2] not in ("", "another device"), json.dumps(info.get("th")))
    check("102 differences highlighted (Title row differs)", "Title" in (info.get("diff") or []), json.dumps(info.get("diff")))
    check("103 remote device and date visible", isinstance(info, dict) and "Since " in info.get("text", "") and "other device:" in info.get("text", "")
          and "other device: another device" not in info.get("text", ""), "")
    merge_btns = ui.js(UI_JS + "return ['U1 by B','U2 by B','U3 by B','U4 by B'].map(n => !!btn(card(n), 'Merge'));")
    check("106 Merge offered only where valid (U3: title vs URL), absent for same-field and delete conflicts", merge_btns == [False, False, True, False], json.dumps(merge_btns))
    ui.js(UI_JS + "btn(card('U1 by B'), \"Keep this device's version\").click(); return true;")
    ok104 = ui.until(UI_JS + "const s = document.querySelector('.conflicts [role=status]'); return !!s && s.textContent.includes('U1 by B') && s.textContent.includes('Resolved \u2014 kept this device') && badge() === 'Conflict \u00b7 3';")
    check("104/109 Keep local from the UI: confirmation shown, badge 4 -> 3", bool(ok104), str(badge()))
    ui.js(UI_JS + "btn(card('U2 by B'), \"Take the other device's version\").click(); return true;")
    ok105 = ui.until(UI_JS + "return document.body.innerText.includes('Decision saved') && badge() === 'Conflict · 2';")
    ui.js(UI_JS + "btn(card('U3 by B'), 'Merge').click(); return true;")
    ok106 = ui.until(UI_JS + "return badge() === 'Conflict · 1';")
    ui.js(UI_JS + "btn(card('U4 by B'), 'Delete it').click(); return true;")
    conf = ui.until(UI_JS + "const b = btn(card('U4 by B'), 'Confirm: Delete it'); return !!b && card('U4 by B').innerText.includes('Click again to confirm');")
    still = ui.js(UI_JS + "return badge();")
    ui.js(UI_JS + "btn(card('U4 by B'), 'Confirm: Delete it').click(); return true;")
    ok107 = ui.until(UI_JS + "return !badge();")
    check("105 Keep remote from the UI: decision saved (resolving), badge 3 -> 2", bool(ok105), "")
    check("106b Merge from the UI, badge 2 -> 1", bool(ok106), "")
    check("107 destructive delete needs an explicit second click (first click only asks), then badge 1 -> 0",
          bool(conf) and still == "Conflict · 1" and bool(ok107), f"before confirm={still}")
    sync_both(a, b)
    ui.js(UI_JS + "btn(document, 'Refresh').click(); return true;")
    ui.until(UI_JS + "return !!btn(document, 'History');")
    ui.js(UI_JS + "btn(document, 'History').click(); return true;")
    hist = ui.until(UI_JS + """
      const items = all('.conflicts li.row-item').map(li => li.innerText);
      const want = ['U1 by B', 'U2 by B', 'U3 by B', 'U4 by B'];
      return want.every(w => items.some(t => t.includes(w) && t.includes('Resolved'))) ? items.length : null;""", 60)
    check("108 history after resolving: the four UI resolutions listed as Resolved", bool(hist), str(hist))
    panel = ui.js(UI_JS + "return all('.conflicts').map(x => x.innerText).join('\\n');") or ""
    check("111 conflicts UI shows no internal ids (no UUIDs, no conflict ids)", isinstance(panel, str) and panel and not UUID.search(panel), "")
    ui.stop()
    nu1, nu3 = node(a, "U1 by B"), node(a, "U3 by B")
    check("104-107 effects converged on A and B: U1=B, U2=A, U3=B title+A URL, U4 deleted",
          nu1[0] and node(a, "U2 by A")[0] and nu3[0] and host(nu3[1]) == "example.org" and "U4 by B" not in titles(a)
          and converged(a, b, pid) and not open_views(b), "")
    M["conv_ms"]["case15_ui_x4"] = int((time.time() - t15) * 1000)
    for k_ in ("U1 by B", "U2 by A", "U3 by B", "U4 by B"):
        c_ = by_name(b, k_)
        if c_.get("resolution"):
            M["resolutions"][c_["resolution"]] += 1

    # ---------------------------------------------------------------- CASE 14 isolation (shared Keychain: last)
    stop(a)
    stop(b)
    a_first = ra.journal(a, pid, a.device_id)[1]
    first_path = a_first[0]["path"] if a_first else ""
    a_cid = by_name(b, "K1 by B").get("conflict_id", "")
    cdev = ra.Device("C")
    u = cdev.dev("--dev-sign-in", creds, "B")
    check("95 login of USER_B (exact id)", u.get("user_id") == h.EXPECTED["B"], f"user_id={u.get('user_id')}")
    probe = cdev.dev("--dev-cloud-probe-foreign", h.EXPECTED["A"], pid, a.device_id, first_path) if u.get("signed_in") else {}
    inv = cdev.dev("--dev-inventory")
    refd = lambda v_: isinstance(v_, dict) and "Err" in v_
    check("96a USER_B cannot see A's profile or mappings", probe.get("foreign_profile_visible") == {"Ok": False}
          and probe.get("foreign_mappings_visible") == {"Ok": 0} and inv.get("profiles") == 0 and inv.get("mappings") == 0, "")
    check("96b USER_B cannot list/read A's journals or read/insert A's store_events, nor write under A's prefix",
          probe.get("list_sees") == {"Ok": 0} and probe.get("get_first_chunk") == {"Ok": False} and probe.get("store_events_sees") == {"Ok": 0}
          and refd(probe.get("store_event_insert")) and refd(probe.get("put_under_foreign_prefix")), json.dumps({k: v_ for k, v_ in probe.items() if k != "as_user"}))
    lst = b.dev("--dev-conflict-views", b.mid, "all")
    res_b = b.dev("--dev-resolve", b.mid, a_cid, "keep_remote")
    res_c = cdev.dev("--dev-resolve", b.mid, a_cid, "keep_remote")
    leak = json.dumps([lst, res_b, res_c])
    refused = lambda r_: "error" in r_ or all(x_.get("outcome") == "error" for x_ in r_.get("results", [{}]))
    check("96c-97 USER_B can neither list nor resolve A's conflict_id (A's installation or its own); answers are 'not found/no access' without data",
          refused(lst) and refused(res_b) and refused(res_c) and "results" not in res_b and not any(w_ in leak for w_ in ("K1", "by B", "by A", "example.")),
          json.dumps({"list": lst.get("error"), "resolve_on_A_install": res_b.get("error"), "resolve_own_install": res_c.get("error")}))
    a.dev("--dev-sign-in", creds, "A")
    check("97b A's conflict untouched by those attempts", by_name(b, "K1 by B").get("status") == "resolved", "")

    # ---------------------------------------------------------------- security
    for dv in (a, b):
        lg = dv.log()
        leaks = [w_ for w_ in ("example.", "Synchrome Conflict", "Folder", "by B", "by A", "child", "K1", "U1", "H1", "C2", "Y v2") if w_ in lg]
        check(f"110 device {dv.name} host log: no titles, URLs, JWT, refresh or mapping tokens", not leaks and h.secret_free(lg), f"leaks={leaks}")
    ul = open(os.path.join(ra.WORK, "ui-app.log"), errors="replace").read() if os.path.exists(os.path.join(ra.WORK, "ui-app.log")) else ""
    check("110b app (UI) output: no titles, URLs or tokens", not any(w_ in ul for w_ in ("example.", "by B", "by A")) and h.secret_free(ul), "")
    ext = "".join(open(os.path.join(h.EXT, f), errors="replace").read() for f in os.listdir(h.EXT) if os.path.isfile(os.path.join(h.EXT, f)))
    check("112 extension has no conflict-decision logic (only executes apply plans; popup only reports counts)",
          not re.search(r"keep_local|keep_remote|resolve_conflict|merge", ext), "")
    check("113 extension has no direct Supabase connection (no URL, key, JWT, fetch/XHR/WebSocket)",
          "supabase" not in ext.lower() and not h.JWT.search(ext) and not re.search(r"\bfetch\(|XMLHttpRequest|WebSocket|EventSource", ext), "")

    # ---------------------------------------------------------------- metrics
    by_type = {}
    for k_ in M["created"].values():
        by_type[k_] = by_type.get(k_, 0) + 1
    final = {}
    for dv in (a, b):
        for c_ in views(dv, True):
            final[c_["status"]] = final.get(c_["status"], 0) + 1
    conv = list(M["conv_ms"].values())
    out = {
        "conflicts_by_type": by_type, "keep_local": M["resolutions"]["keep_local"], "keep_remote": M["resolutions"]["keep_remote"],
        "merges": M["resolutions"]["merge"], "retries": M["resolutions"]["retry"], "relocates": M["resolutions"]["relocate"],
        "superseded": final.get("superseded", 0), "resolution_ops_cli": M["resolution_ops"],
        "resolution_ops_total": sum(c_.get("resolution_ops", 0) for dv in (a, b) for c_ in views(dv, True)),
        "suppressed_echo_events": M.get("suppressed", {}),
        "unresolved_end": final.get("unresolved", 0) + final.get("resolving", 0), "resolved_end": final.get("resolved", 0),
        "conflict_to_convergence_ms": M["conv_ms"], "mean_ms": int(sum(conv) / len(conv)) if conv else None, "max_ms": max(conv) if conv else None,
        "ops_published": {"A": len(jops(a, pid, a.device_id)), "B": len(jops(a, pid, b.device_id))},
    }
    out["open_at_end"] = [{"device": dv.name, "kind": c_["kind"], "subtype": c_["subtype"], "status": c_["status"], "name": c_["name"],
                           "detail": c_.get("detail"), "attempts": c_.get("attempts")} for dv in (a, b) for c_ in views(dv, True)
                          if c_["status"] in ("unresolved", "resolving")]
    print("METRICS " + json.dumps(out))
    check("metrics collected; no unresolved conflicts at the end", out["unresolved_end"] == 0, "")

    # ---------------------------------------------------------------- cleanup
    rb = b.dev("--dev-cleanup", b.mid)
    rbd = b.dev("--dev-delete-this-device")
    rA = a.dev("--dev-cleanup", a.mid)
    gone = a.dev("--dev-cloud-tree", pid)
    inv = a.dev("--dev-inventory")
    check("cleanup cloud: Storage objects, store_events, profile, both mappings, both devices removed",
          rb.get("unmapped") and rbd.get("deleted") and rA.get("unmapped") and rA.get("deleted_profile") and rA.get("device_deleted")
          and gone.get("objects") == 0 and gone.get("store_events") == 0 and inv.get("profiles") == 0 and inv.get("mappings") == 0
          and inv.get("devices") == [], json.dumps({"B": rb, "A": rA}))
    so = a.dev("--dev-sign-out")
    check("cleanup Keychain session removed", so.get("signed_out") and not h.keychain_has_session(), "")
    for dv in (a, b, cdev):
        shutil.rmtree(dv.udd, ignore_errors=True)
        shutil.rmtree(dv.data, ignore_errors=True)
    shutil.rmtree(os.path.join(ra.WORK, "ui-driver"), ignore_errors=True)
    for f in ("ui-app.log",):
        try:
            os.remove(os.path.join(ra.WORK, f))
        except FileNotFoundError:
            pass
    check("cleanup local: data dirs (state.sqlite + conflicts, registry, logs, backups), Helium profiles + manifests, UI driver files",
          not any(os.path.exists(p_) for dv in (a, b, cdev) for p_ in (dv.udd, dv.data)), "")


if __name__ == "__main__":
    try:
        main(sys.argv[1])
    except Exception as e:  # noqa: BLE001
        import traceback
        traceback.print_exc()
        check("harness completed without exception", False, type(e).__name__ + ": " + str(e)[:200])
        for p_ in UI_PROCS:
            if p_.poll() is None:
                p_.kill()
        ra.emergency()
    for p_ in UI_PROCS:
        if p_.poll() is None:
            p_.kill()
    failed = [n for n, ok in h.results if not ok]
    print(f"\n=== conflicts: {len(h.results) - len(failed)} pass, {len(failed)} fail ===")
    if failed:
        print("  failures: " + ", ".join(failed))
    sys.exit(1 if failed else 0)
