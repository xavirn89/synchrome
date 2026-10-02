"""Two-device E2E of bookmark conflict resolution (M1 slice 9). Disposable profiles only.

Same rig as e2e_remote_apply.py: devices A and B are two simulated installations on this
machine (own SYNCHROME_DATA_DIR each: device.json, state.sqlite, registry, logs), both signed
in as test user A, each with its own disposable Helium --user-data-dir. Test user B is used
only for the isolation probes. A device's change is kept *unpublished* by making it while
that device runs offline (`connected_offline`: captured, not uploaded). Conflicts are
resolved through `--dev-resolve`, the same Rust path as the app's buttons. Prints ids,
counts, field/group names, plan op codes and fixture titles only — never URLs (hosts at
most), tokens or passwords.

  EXPECTED_A_ID=… EXPECTED_B_ID=… python3 e2e_conflicts.py <creds-env-file>
"""
import json, os, re, shutil, sys, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import e2e_handshake as h  # noqa: E402
import e2e_bookmarks as eb  # noqa: E402
import e2e_remote_apply as ra  # noqa: E402

check = h.check
find = eb.find
flat = ra.flat
M = {
    "conflicts_created": {},
    "resolutions": {"keep_local": 0, "keep_remote": 0, "merge": 0, "retry": 0, "relocate": 0},
    "resolution_ops": 0,
    "conflict_to_convergence_ms": {},
    "suppressed_echo_events": {"A": 0, "B": 0},
}


def titles(d):
    return [r[1] for r in flat(d.tree())]


def views(d, closed=False):
    return d.dev("--dev-conflict-views", d.mid, *(["all"] if closed else [])).get("conflicts") or []


def open_views(d):
    return [c for c in views(d) if c["status"] in ("unresolved", "resolving")]


def count_created(d):
    for c in views(d, True):
        M["conflicts_created"].setdefault(c["conflict_id"], f"{c['kind']}/{c['subtype']}")


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
            M["suppressed_echo_events"][d.name] += int(d.hl.js("suppressedEvents") or 0)
        except Exception:  # noqa: BLE001
            pass
        d.hl.stop()
        d.hl = None


def offline_edit(d, js):
    """Make a local change on `d` that stays unpublished (device offline), then stop it."""
    stop(d)
    t = eb.now_ms()
    d.start(offline=True)
    d.hl.wait(lambda s: s.get("status") == "connected_offline", 60, after=t)
    ra.wait(d.idle, 30)
    before = d.status().get("pending", 0)
    d.hl.js(f"(async () => {{ {js}; return true; }})()")
    ok = ra.wait(lambda: d.status().get("pending", 0) > before and d.idle(), 60)
    stop(d)
    return ok


def online(d, until=None, timeout=90):
    """Start `d` online: its worker-start cycle runs (snapshot, events, pull, apply)."""
    stop(d)
    t = eb.now_ms()
    d.start()
    d.hl.wait(lambda s: s.get("status") == "connected", 60, after=t)
    ra.wait(lambda: d.idle() and (until is None or until()), timeout)


def dl(d):
    return d.status().get("download") or {}


def converged(a, b, pid):
    return a.tree() == b.tree() and ra.same_identity_tree(a, b, pid)


def sync_both(a, b, rounds=2):
    for _ in range(rounds):
        ra.pull_b(b)
        ra.pull_b(a)


def main(creds):
    h.prepare_binary()
    os.makedirs(ra.WORK, exist_ok=True)
    a, b = ra.Device("A"), ra.Device("B")
    print("=== two-device conflict resolution E2E ===")
    check("pre: signed out, no Keychain session", a.dev("--dev-account").get("signed_in") is False and not h.keychain_has_session())
    sa = a.dev("--dev-sign-in", creds, "A")
    check("1 login user A for device A (exact id)", sa.get("user_id") == h.EXPECTED["A"], f"user_id={sa.get('user_id')}")
    if not sa.get("signed_in"):
        return
    sb = b.dev("--dev-sign-in", creds, "A")
    check("2 login user A for device B (exact id)", sb.get("user_id") == h.EXPECTED["A"], f"user_id={sb.get('user_id')}")
    a.device_id = a.dev("--dev-register-device").get("device_id")
    b.device_id = b.dev("--dev-register-device").get("device_id")
    check("3 same account, distinct device ids and data dirs", a.device_id and b.device_id and a.device_id != b.device_id and a.data != b.data,
          f"A={a.device_id} B={b.device_id}")
    m = a.dev("--dev-create-mapping", f"E2E CF {os.urandom(3).hex()}", "helium", "Default")
    a.mid, pid = m.get("mapping_id"), m.get("profile_id")
    a.dev("--dev-write-token", a.mid, a.udd)
    a.dev("--dev-install-host", a.udd)
    t = eb.now_ms()
    a.start()
    a.hl.wait(lambda s: s.get("status") == "connected" and s.get("token_state") == "confirmed", 60, after=t)
    ra.wait(a.idle, 30)
    mb = b.dev("--dev-map-existing", pid, "helium", "Default")
    b.mid = mb.get("mapping_id")
    b.dev("--dev-write-token", b.mid, b.udd)
    b.dev("--dev-install-host", b.udd)
    t = eb.now_ms()
    b.start()
    stb = b.hl.wait(lambda s: s.get("status") == "connected" and s.get("token_state") == "confirmed", 60, after=t)
    ra.wait(b.idle, 30)
    check("4 both devices mapped to the same cloud profile and confirmed", stb is not None and mb.get("profile_id") == pid, f"profile={pid}")

    ra.edit_and_publish(a, """
      const t = await chrome.bookmarks.create({parentId: "1", title: "Synchrome Conflict Test"});
      const fa = await chrome.bookmarks.create({parentId: t.id, title: "Folder A"});
      for (const i of [1, 2, 3]) await chrome.bookmarks.create({parentId: fa.id, title: "Bookmark " + i, url: "https://example.com/" + i});
      const fd = await chrome.bookmarks.create({parentId: t.id, title: "Folder D"});
      for (const i of [1, 2, 3, 4]) await chrome.bookmarks.create({parentId: fd.id, title: "D" + i, url: "https://example.com/d" + i});
      const f5 = await chrome.bookmarks.create({parentId: t.id, title: "Folder F5"});
      await chrome.bookmarks.create({parentId: f5.id, title: "F5 synced", url: "https://example.com/f5"});
      await chrome.bookmarks.create({parentId: t.id, title: "Folder F6"});
      await chrome.bookmarks.create({parentId: t.id, title: "Bookmark Y", url: "https://example.com/y"})""")
    ra.pull_b(b)
    check("5 fixture created on A and applied on B (same tree, same identities)", converged(a, b, pid), json.dumps(ra.show(b.tree())))

    # ---------------------------------------------------------------- case 1 + case 7
    offline_edit(b, f"await chrome.bookmarks.update(({find('Bookmark 1')}).id, {{title: 'X1 on B'}})")
    ra.edit_and_publish(a, f"await chrome.bookmarks.update(({find('Bookmark 1')}).id, {{title: 'X1 on A'}})")
    online(b, until=lambda: dl(b).get("state") == "conflict")
    t_conf = time.time()
    count_created(b)
    v = open_views(b)
    c1 = v[0] if v else {}
    check("C1.1 same bookmark: B's unpublished change + A's change -> B in Conflict (local_pending/edit_edit)",
          len(v) == 1 and c1.get("kind") == "local_pending" and c1.get("subtype") == "edit_edit" and dl(b).get("state") == "conflict",
          json.dumps({k: c1.get(k) for k in ("kind", "subtype", "status", "held", "remote_seq")}))
    check("C1.2 view: LOCAL 'X1 on B' vs REMOTE 'X1 on A' from device A, BASE 'Bookmark 1', options keep_local/keep_remote (no merge)",
          c1.get("local", {}).get("title") == "X1 on B" and c1.get("remote", {}).get("title") == "X1 on A"
          and c1.get("remote_device") == a.device_id and (c1.get("base") or {}).get("title") == "Bookmark 1"
          and [o["resolution"] for o in c1.get("options", [])] == ["keep_local", "keep_remote"], "")
    check("C1.3 B's own change was published anyway; B's browser still shows it (nothing overwritten)",
          b.status().get("pending") == 0 and "X1 on B" in titles(b), "")
    # case 7: while X is in conflict, Y keeps syncing
    ra.edit_and_publish(a, f"await chrome.bookmarks.update(({find('Bookmark Y')}).id, {{title: 'Y v2'}})")
    ra.pull_b(b)
    check("C7 while X is in conflict, another entity (Y) is downloaded and applied on B; X stays frozen",
          "Y v2" in titles(b) and "X1 on B" in titles(b) and len(open_views(b)) == 1 and dl(b).get("remote_pending") == 0, "")
    _, cb0, _ = ra.journal(a, pid, b.device_id)
    r = resolve(b, c1.get("conflict_id", ""), "keep_local")
    _, cb1, _ = ra.journal(a, pid, b.device_id)
    new = [x for x in cb1 if x["path"] not in {y["path"] for y in cb0}]
    ops = [o for x in new for o in x["ops"]]
    remote_hlc = max((o["hlc"] for x in ra.journal(a, pid, a.device_id)[1] for o in x["ops"]), default="")
    check("C1.4 Keep local: resolved, 1 new op [title] published by B with an HLC later than both versions",
          r.get("outcome") == "resolved" and r.get("ops") == 1 and len(ops) == 1 and ops[0]["fields"] == ["title"]
          and ops[0].get("hlc", "") > remote_hlc, json.dumps({"result": r, "ops": ops}))
    ra.pull_b(a)
    ra.pull_b(b)
    check("C1.5 A ends with B's value; A and B converge; the conflict is resolved (kept as history)",
          "X1 on B" in titles(a) and converged(a, b, pid)
          and any(c["conflict_id"] == c1.get("conflict_id") and c["status"] == "resolved" and c["resolution"] == "keep_local" for c in views(b, True)),
          "")
    M["conflict_to_convergence_ms"]["case1_keep_local"] = int((time.time() - t_conf) * 1000)

    # ---------------------------------------------------------------- case 2
    offline_edit(b, f"await chrome.bookmarks.update(({find('Bookmark 2')}).id, {{title: 'X2 on B'}})")
    ra.edit_and_publish(a, f"await chrome.bookmarks.update(({find('Bookmark 2')}).id, {{title: 'X2 on A'}})")
    online(b, until=lambda: dl(b).get("state") == "conflict")
    t_conf = time.time()
    count_created(b)
    c2 = (open_views(b) or [{}])[0]
    check("C2.1 conflict again on another bookmark", c2.get("subtype") == "edit_edit" and c2.get("local", {}).get("title") == "X2 on B", "")
    _, cb0, _ = ra.journal(a, pid, b.device_id)
    sup0 = int(b.hl.js("suppressedEvents") or 0)
    r = resolve(b, c2.get("conflict_id", ""), "keep_remote")
    check("C2.2 Keep remote: decision persisted, status resolving until the browser is updated",
          r.get("outcome") == "resolving" and any(c["status"] == "resolving" for c in views(b)), json.dumps(r))
    sl, _ = ra.pull_b(b)
    plans = [p for p in sl["plans"] if p[0]]
    _, cb1, _ = ra.journal(a, pid, b.device_id)
    new_ops = [o for x in cb1 if x["path"] not in {y["path"] for y in cb0} for o in x["ops"]]
    check("C2.3 B applied A's version (rename), echo = 0 (reconcile 0 ops; only the resolution op was published)",
          "X2 on A" in titles(b) and plans and "rn" in plans[0][2].split(",")
          and sl["snapshots"][-1:] == ["Ok(Applied { ops: 0, pending: 0 })"] and len(new_ops) == r.get("ops")
          and int(b.hl.js("suppressedEvents") or 0) > sup0,
          json.dumps({"plans": plans, "snapshots": sl["snapshots"], "new_ops": len(new_ops)}))
    ra.pull_b(a)
    check("C2.4 both converge; conflict resolved after the apply",
          converged(a, b, pid) and any(c["conflict_id"] == c2.get("conflict_id") and c["status"] == "resolved" for c in views(b, True)), "")
    M["conflict_to_convergence_ms"]["case2_keep_remote"] = int((time.time() - t_conf) * 1000)

    # ---------------------------------------------------------------- case 3
    offline_edit(b, f"await chrome.bookmarks.update(({find('Bookmark 3')}).id, {{title: 'T3 from B'}})")
    ra.edit_and_publish(a, f"await chrome.bookmarks.update(({find('Bookmark 3')}).id, {{url: 'https://example.org/3-from-a'}})")
    online(b, until=lambda: dl(b).get("state") == "conflict")
    t_conf = time.time()
    count_created(b)
    c3 = (open_views(b) or [{}])[0]
    check("C3.1 B changed the title, A the URL: merge offered (first), local=[title] remote=[url]",
          [o["resolution"] for o in c3.get("options", [])][:1] == ["merge"]
          and c3.get("local", {}).get("changed") == ["title"] and c3.get("remote", {}).get("changed") == ["url"], "")
    r = resolve(b, c3.get("conflict_id", ""), "merge")
    sync_both(a, b)
    node = lambda d: next(((t_, u_) for _, t_, u_ in flat(d.tree()) if t_ == "T3 from B"), (None, None))
    host = lambda u: u.split("/")[2] if u else None
    check("C3.2 Merge: both end with B's title + A's URL; converged; resolved",
          r.get("outcome") in ("resolving", "resolved") and node(a)[0] == node(b)[0] == "T3 from B"
          and node(a)[1] == node(b)[1] and host(node(b)[1]) == "example.org" and node(b)[1].endswith("3-from-a")
          and converged(a, b, pid) and any(c["conflict_id"] == c3.get("conflict_id") and c["status"] == "resolved" for c in views(b, True)),
          json.dumps({"A": host(node(a)[1]), "B": host(node(b)[1])}))
    M["conflict_to_convergence_ms"]["case3_merge"] = int((time.time() - t_conf) * 1000)

    # ---------------------------------------------------------------- case 4 (delete conflicts, both choices each)
    offline_edit(b, f"""
      await chrome.bookmarks.update(({find('D1')}).id, {{title: 'D1 edited on B'}});
      await chrome.bookmarks.update(({find('D2')}).id, {{title: 'D2 edited on B'}});
      await chrome.bookmarks.remove(({find('D3')}).id);
      await chrome.bookmarks.remove(({find('D4')}).id)""")
    ra.edit_and_publish(a, f"""
      await chrome.bookmarks.remove(({find('D1')}).id);
      await chrome.bookmarks.remove(({find('D2')}).id);
      await chrome.bookmarks.update(({find('D3')}).id, {{title: 'D3 edited on A'}});
      await chrome.bookmarks.update(({find('D4')}).id, {{title: 'D4 edited on A'}})""")
    online(b, until=lambda: dl(b).get("conflicts") == 4)
    t_conf = time.time()
    count_created(b)
    cs = {c["name"]: c for c in open_views(b)}
    by = lambda n: cs.get(n) or next((c for c in cs.values() if c.get("remote", {}).get("title") == n), {})
    d1, d2 = by("D1 edited on B"), by("D2 edited on B")
    d3, d4 = by("D3 edited on A"), by("D4 edited on A")
    destr = lambda c, r_: next((o["destructive"] for o in c.get("options", []) if o["resolution"] == r_), None)
    check("C4.1 four delete conflicts, distinct ids: 2x edit_delete (local edit vs remote delete), 2x delete_edit",
          len(cs) == 4 and len({c["conflict_id"] for c in cs.values()}) == 4
          and d1.get("subtype") == d2.get("subtype") == "edit_delete" and d3.get("subtype") == d4.get("subtype") == "delete_edit",
          json.dumps([c["subtype"] for c in cs.values()]))
    check("C4.2 nothing deleted or restored while unresolved; destructive choices flagged for confirmation",
          "D1 edited on B" in titles(b) and "D2 edited on B" in titles(b) and "D3 edited on A" not in titles(b)
          and destr(d1, "keep_remote") is True and destr(d3, "keep_local") is True and destr(d1, "keep_local") is False, "")
    rr = [resolve(b, d1["conflict_id"], "keep_local"), resolve(b, d2["conflict_id"], "keep_remote"),
          resolve(b, d3["conflict_id"], "keep_remote"), resolve(b, d4["conflict_id"], "keep_local")]
    sync_both(a, b)
    ta, tb = titles(a), titles(b)
    check("C4.3 edit vs delete: Keep local keeps 'D1 edited on B' (restored on A); Keep remote deletes D2 on B",
          "D1 edited on B" in ta and "D1 edited on B" in tb and "D2 edited on B" not in tb and "D2 edited on B" not in ta, json.dumps(rr))
    check("C4.4 delete vs edit: Keep remote restores 'D3 edited on A' on B; Keep local keeps D4 deleted (also on A)",
          "D3 edited on A" in tb and "D3 edited on A" in ta and "D4 edited on A" not in tb and "D4 edited on A" not in ta, "")
    check("C4.5 converged; all four resolved", converged(a, b, pid) and not open_views(b), json.dumps([c["status"] for c in views(b, True)]))
    M["conflict_to_convergence_ms"]["case4_deletes_x4"] = int((time.time() - t_conf) * 1000)

    # ---------------------------------------------------------------- case 5 apply_failed
    offline_edit(b, f"await chrome.bookmarks.create({{parentId: ({find('Folder F5')}).id, title: 'F5 local child', url: 'https://example.net/f5b'}})")
    ra.edit_and_publish(a, f"await chrome.bookmarks.removeTree(({find('Folder F5')}).id)")
    online(b, until=lambda: dl(b).get("state") == "conflict")
    t_conf = time.time()
    count_created(b)
    c5 = next((c for c in open_views(b) if c["subtype"] == "delete_not_empty"), {})
    check("C5.1 folder delete with a local child: apply_failed/delete_not_empty, folder + child kept, 1 local item reported",
          c5.get("kind") == "apply_failed" and c5.get("local_items") == 1 and "Folder F5" in titles(b)
          and "F5 local child" in titles(b) and "F5 synced" not in titles(b), json.dumps({k: c5.get(k) for k in ("kind", "subtype", "attempts", "local_items")}))
    r = resolve(b, c5.get("conflict_id", ""), "retry")
    ra.pull_b(b)
    c5b = next((c for c in open_views(b) if c["conflict_id"] == c5.get("conflict_id")), {})
    check("C5.2 Retry while the child is still inside: fails again, same conflict unresolved (attempts 2), child NOT lost",
          r.get("outcome") == "resolving" and c5b.get("status") == "unresolved" and c5b.get("attempts") == 2 and "F5 local child" in titles(b), json.dumps(c5b.get("attempts")))
    r = resolve(b, c5.get("conflict_id", ""), "relocate")
    sync_both(a, b)
    tb = b.tree()
    root_kids = [n["t"] for n in tb[0]["c"]] if tb else []
    check("C5.3 explicit resolution (move the local item out, then delete the folder): child kept in 'Synchrome Conflict Test', folder gone, on both",
          "F5 local child" in root_kids and "Folder F5" not in titles(b) and "Folder F5" not in titles(a) and "F5 local child" in titles(a)
          and converged(a, b, pid), json.dumps({"result": r, "root": root_kids}))
    check("C5.4 B's conflict resolved; no decision needed on A (the child arrives already relocated, or its orphan is superseded)",
          not open_views(b) and not open_views(a), json.dumps({"A": [c["subtype"] for c in open_views(a)]}))
    M["conflict_to_convergence_ms"]["case5_apply_failed"] = int((time.time() - t_conf) * 1000)

    # ---------------------------------------------------------------- case 6 orphan (on A)
    offline_edit(a, f"await chrome.bookmarks.removeTree(({find('Folder F6')}).id)")
    ra.edit_and_publish(b, f"await chrome.bookmarks.create({{parentId: ({find('Folder F6')}).id, title: 'F6 child from B', url: 'https://example.net/f6'}})")
    online(a, until=lambda: dl(a).get("state") == "conflict")
    t_conf = time.time()
    count_created(a)
    c6 = next((c for c in open_views(a) if c["kind"] == "orphan"), {})
    check("C6.1 A deleted Folder F6 (unpublished) while B added a child: explicit orphan on A, child not silently dropped or created",
          c6.get("subtype") == "orphan" and "F6 child from B" not in titles(a) and "Folder F6" not in titles(a)
          and [o["resolution"] for o in c6.get("options", [])] == ["relocate", "keep_remote", "keep_local"]
          and next((o["destructive"] for o in c6["options"] if o["resolution"] == "keep_local"), None) is True, json.dumps(c6.get("options")))
    r = resolve(a, c6.get("conflict_id", ""), "relocate")
    ra.pull_b(a)
    check("C6.2 Relocate: the child is kept on A in the nearest existing folder ('Synchrome Conflict Test')",
          "F6 child from B" in [n["t"] for n in a.tree()[0]["c"]], json.dumps(r))
    # B: A's folder delete failed there (B's child was inside) until the child moved out
    ra.pull_b(b)
    count_created(b)
    for c in open_views(b):
        resolve(b, c["conflict_id"], "retry")
    sync_both(a, b)
    check("C6.3 converged without silent loss: child kept on both, Folder F6 deleted on both, no open conflicts",
          "F6 child from B" in titles(a) and "F6 child from B" in titles(b) and "Folder F6" not in titles(b)
          and converged(a, b, pid) and not open_views(a) and not open_views(b), "")
    M["conflict_to_convergence_ms"]["case6_orphan"] = int((time.time() - t_conf) * 1000)

    # ---------------------------------------------------------------- persistence + idempotency
    c1v = next((c for c in views(b, True) if c["conflict_id"] == c1.get("conflict_id")), {})
    stop(b)
    again = resolve(b, c1.get("conflict_id", ""), "keep_local")
    other = resolve(b, c1.get("conflict_id", ""), "keep_remote")
    c1w = next((c for c in views(b, True) if c["conflict_id"] == c1.get("conflict_id")), {})
    check("P1 history persisted across processes (state.sqlite); repeated resolution idempotent; a different one refused",
          c1v and c1w.get("status") == "resolved" and c1w.get("local", {}).get("title") == "X1 on B" and c1w.get("remote", {}).get("title") == "X1 on A"
          and again.get("outcome") == "already_resolved" and other.get("outcome") == "closed", json.dumps({"again": again, "other": other}))
    foreign = resolve(b, next((c["conflict_id"] for c in views(a, True)), ""), "keep_local")
    check("S1 a conflict id of another device's/profile's state is rejected (not found)", foreign.get("outcome") == "error", json.dumps(foreign))

    # ---------------------------------------------------------------- security
    stop(a)
    _, ca, _ = ra.journal(a, pid, a.device_id)
    first_path = ca[0]["path"] if ca else ""
    cdev = ra.Device("C")
    u = cdev.dev("--dev-sign-in", creds, "B")
    check("S2 login of test user B (exact id)", u.get("user_id") == h.EXPECTED["B"], f"user_id={u.get('user_id')}")
    probe = cdev.dev("--dev-cloud-probe-foreign", h.EXPECTED["A"], pid, a.device_id, first_path) if u.get("signed_in") else {}
    refd = lambda v_: isinstance(v_, dict) and "Err" in v_
    check("S3 user B cannot list/read A's journals or store_events, write under A's prefix, or see A's mappings/profile",
          probe.get("list_sees") == {"Ok": 0} and probe.get("get_first_chunk") == {"Ok": False} and probe.get("store_events_sees") == {"Ok": 0}
          and refd(probe.get("put_under_foreign_prefix")) and probe.get("foreign_mappings_visible") == {"Ok": 0}
          and probe.get("foreign_profile_visible") == {"Ok": False}, json.dumps({k: v_ for k, v_ in probe.items() if k != "as_user"}))
    as_b_list = b.dev("--dev-conflict-views", b.mid, "all")
    as_b_res = resolve(b, c1.get("conflict_id", ""), "keep_remote")
    check("S4 signed in as user B: A's conflicts are neither listed nor resolvable on that installation",
          "error" in as_b_list and as_b_res.get("outcome") != "resolved", json.dumps({"list": as_b_list.get("error"), "resolve": as_b_res}))
    a.dev("--dev-sign-in", creds, "A")
    ext = "".join(open(os.path.join(h.EXT, f), errors="replace").read() for f in os.listdir(h.EXT) if os.path.isfile(os.path.join(h.EXT, f)))
    check("S5 extension: no network API, no Supabase URL/JWT; it only reports conflicts (decisions live in the app)",
          not re.search(r"\bfetch\(|XMLHttpRequest|WebSocket|EventSource", ext) and "supabase" not in ext.lower() and not h.JWT.search(ext)
          and "keep_local" not in ext and "resolve_conflict" not in ext, "")
    for dv in (a, b):
        lg = dv.log()
        leaks = [w_ for w_ in ("example.", "Bookmark", "Synchrome Conflict", "Folder", "on B", "on A", "child", "X1", "X2", "T3", "D1", "Y v2") if w_ in lg]
        check(f"S6 device {dv.name} host log: no titles, URLs or tokens", not leaks and h.secret_free(lg), f"leaks={leaks}")

    # ---------------------------------------------------------------- metrics
    by_type = {}
    for k in M["conflicts_created"].values():
        by_type[k] = by_type.get(k, 0) + 1
    M["conflicts_by_type"] = by_type
    M["remaining_open_conflicts"] = {"A": len(open_views(a)), "B": len(open_views(b))}
    M["closed_by_status"] = {}
    for dv in (a, b):
        for c in views(dv, True):
            M["closed_by_status"][c["status"]] = M["closed_by_status"].get(c["status"], 0) + 1
    _, cbj, ebj = ra.journal(a, pid, b.device_id)
    _, caj, eaj = ra.journal(a, pid, a.device_id)
    M["ops_published"] = {"A": sum(len(x["ops"]) for x in caj), "B": sum(len(x["ops"]) for x in cbj)}
    print("METRICS " + json.dumps({k: v_ for k, v_ in M.items() if k != "conflicts_created"}))
    check("M1 metrics collected; no conflicts left open", M["remaining_open_conflicts"] == {"A": 0, "B": 0}, "")

    # ---------------------------------------------------------------- cleanup
    rb = b.dev("--dev-cleanup", b.mid)
    rbd = b.dev("--dev-delete-this-device")
    rA = a.dev("--dev-cleanup", a.mid)
    gone = a.dev("--dev-cloud-tree", pid)
    inv = a.dev("--dev-inventory")
    check("Z1 cloud: Storage objects, store_events, profile, both mappings, both devices removed",
          rb.get("unmapped") and rbd.get("deleted") and rA.get("unmapped") and rA.get("deleted_profile") and rA.get("device_deleted")
          and gone.get("objects") == 0 and gone.get("store_events") == 0 and inv.get("profiles") == 0 and inv.get("mappings") == 0
          and inv.get("devices") == [], json.dumps({"B": rb, "A": rA}))
    so = a.dev("--dev-sign-out")
    check("Z2 Keychain session removed", so.get("signed_out") and not h.keychain_has_session(), "")
    for dv in (a, b, cdev):
        shutil.rmtree(dv.udd, ignore_errors=True)
        shutil.rmtree(dv.data, ignore_errors=True)
    check("Z3 data dirs (state.sqlite incl. conflicts, registry, logs, backups), Helium profiles, manifests removed",
          not any(os.path.exists(p_) for dv in (a, b, cdev) for p_ in (dv.udd, dv.data)), "")


if __name__ == "__main__":
    try:
        main(sys.argv[1])
    except Exception as e:  # noqa: BLE001
        import traceback
        traceback.print_exc()
        check("harness completed without exception", False, type(e).__name__ + ": " + str(e)[:200])
        ra.emergency()
    failed = [n for n, ok in h.results if not ok]
    print(f"\n=== conflicts: {len(h.results) - len(failed)} pass, {len(failed)} fail ===")
    if failed:
        print("  failures: " + ", ".join(failed))
    sys.exit(1 if failed else 0)
