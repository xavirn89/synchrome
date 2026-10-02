"""Focused authenticated validation of the extension fix 850f0d7 (sync_status waiter).

Proves, with the host log (one `stage start` / `stage end` pair per native-messaging
connection, and the ordered reply types of every frame), that:
  - a forced equivalent snapshot travels on its OWN connection and yields 0 ops,
  - each later change is its own connection with exactly one ack followed by one sync_status,
  - no connection idles until the 30 s status timeout any more.
Disposable Helium profile only; prints ids, counts, field names and timings only.

  EXPECTED_A_ID=… python3 e2e_bookmarks_fix.py <creds-env-file>
"""
import json, os, re, shutil, sqlite3, sys, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import e2e_handshake as h  # noqa: E402
import e2e_bookmarks as eb  # noqa: E402

check, dev = h.check, h.dev
latency = {}


def log_since(mark):
    return open(h.LOG, "rb").read()[mark:].decode("utf-8", "replace")


def idle(hl):
    return bool(hl.js("queue.length === 0 && running === null && flushTimer === null"))


def settle(hl, mark, timeout=60):
    """Wait until the extension is idle and every connection opened since `mark` closed."""
    end = time.time() + timeout
    while time.time() < end:
        s = log_since(mark)
        if idle(hl) and s.count("stage start") == s.count("stage end"):
            return s
        time.sleep(0.5)
    return log_since(mark)


def conns(slice_):
    return {
        "starts": slice_.count("stage start"),
        "ends": slice_.count("stage end"),
        "hellos": len(re.findall(r"hello ext=\S+ browser=\S+ token=\S+ -> Connected\b", slice_)),
        "snapshots": re.findall(r"bookmarks snapshot parts=\d+ nodes=\d+ -> (.*)", slice_),
        "events": re.findall(r"bookmarks events n=\d+ -> (.*)", slice_),
        "sent": re.findall(r"^\d+ sent (.*)$", slice_, re.M),
        "frames": re.findall(r"stage end after (\d+) ms frames_in=(\d+) frames_out=(\d+)", slice_),
    }


def main(creds):
    h.prepare_binary()
    run_mark = os.path.getsize(h.LOG) if os.path.exists(h.LOG) else 0
    print("=== focused validation of 850f0d7 ===")
    check("precondition: signed out, no Keychain session", dev("--dev-account").get("signed_in") is False and not h.keychain_has_session())
    a = dev("--dev-sign-in", creds, "A")
    check("1 login A (exact user id)", a.get("signed_in") and a.get("user_id") == h.EXPECTED["A"], f"user_id={a.get('user_id')}")
    if not a.get("signed_in"):
        return
    me = dev("--dev-register-device").get("device_id")
    udd = h.fresh_udd("fix")
    eb.RUN["udd"] = udd
    dev("--dev-install-host", udd)

    # 3. minimal structure, created while the extension is not linked yet
    hl = eb.Helium(udd)
    hl.js("""(async () => {
      const t = await chrome.bookmarks.create({parentId: "1", title: "Fix Test"});
      const x = await chrome.bookmarks.create({parentId: t.id, title: "Folder X"});
      await chrome.bookmarks.create({parentId: x.id, title: "Alpha One", url: "https://fixcheck.example/1"});
      await chrome.bookmarks.create({parentId: x.id, title: "Alpha Two", url: "https://fixcheck.example/2"});
      return true; })()""")
    hl.stop()

    # 2. device / profile / mapping, confirmed by the handshake
    m = dev("--dev-create-mapping", f"E2E FIX {os.urandom(3).hex()}", "helium", "Default")
    mid, pid = m.get("mapping_id"), m.get("profile_id")
    eb.RUN["mid"] = mid
    dev("--dev-write-token", mid, udd)
    t0 = eb.now_ms()
    mark = os.path.getsize(h.LOG)
    hl = eb.Helium(udd)
    st = hl.wait(lambda s: eb.up_to_date(s) and eb.bm(s).get("bookmarks") == 2, 90, after=t0)
    settle(hl, mark)
    v = next((x for x in dev("--dev-list-mappings") if x.get("id") == mid), {})
    check("2 device/profile/mapping created and mapping confirmed end-to-end",
          me and (st or {}).get("status") == "connected" and v.get("status") == "token_confirmed", f"mapping={mid}")
    c0 = dev("--dev-cloud-tree", pid)
    check("3-4 minimal structure captured and initial snapshot published (4 ops, 1 chunk, 1 store_event)",
          c0.get("ops") == 4 and len(c0["chunks"]) == 1 and c0.get("store_events") == 1 and c0.get("bookmarks") == 2, "")

    # 5–6. forced equivalent snapshot, isolated
    mark = os.path.getsize(h.LOG)
    t = time.time()
    hl.js("(async () => { await chrome.storage.session.clear(); needSnapshot = true; await run(true); return true; })()", timeout=90)
    dur = int((time.time() - t) * 1000)
    s = conns(settle(hl, mark))
    c1 = dev("--dev-cloud-tree", pid)
    latency["forced_snapshot_ms"] = dur
    print("     forced snapshot connection:", json.dumps(s))
    check("5 forced equivalent snapshot received by the host on ONE isolated connection",
          s["starts"] == 1 and s["ends"] == 1 and s["hellos"] == 1 and len(s["snapshots"]) == 1 and not s["events"]
          and s["frames"] and s["frames"][0][1:] == ("2", "3"), "")
    check("6 isolated snapshot = 0 ops, 0 new chunks, 0 new store_events; ack then sync_status",
          s["snapshots"] == ["Ok(Applied { ops: 0, pending: 0 })"] and s["sent"] == ["hello_result", "bookmarks_ack,sync_status"]
          and c1.get("ops") == c0.get("ops") and len(c1["chunks"]) == len(c0["chunks"]) and c1.get("store_events") == c0.get("store_events"),
          f"sent={s['sent']} ops={c1.get('ops')} chunks={len(c1['chunks'])} events={c1.get('store_events')}")
    check("6b no 30 s stall: the forced snapshot round trip completed quickly", dur < 10_000, f"{dur} ms")

    # 7–12. separate changes, one connection each
    prev = c1
    changes = [
        ("create", "const x = (await chrome.bookmarks.search({title: 'Folder X'}))[0]; await chrome.bookmarks.create({parentId: x.id, title: 'Alpha Three', url: 'https://fixcheck.example/3'})", eb.CREATION_FIELDS),
        ("rename", "await chrome.bookmarks.update((await chrome.bookmarks.search({title: 'Alpha One'}))[0].id, {title: 'Alpha One renamed'})", ["title"]),
        ("url", "await chrome.bookmarks.update((await chrome.bookmarks.search({title: 'Alpha Three'}))[0].id, {url: 'https://fixcheck.example/3b'})", ["url"]),
        ("move", "const x = (await chrome.bookmarks.search({title: 'Folder X'}))[0]; await chrome.bookmarks.move((await chrome.bookmarks.search({title: 'Alpha Two'}))[0].id, {parentId: x.id, index: 0})", ["pos"]),
    ]
    for i, (name, js, fields) in enumerate(changes):
        mark = os.path.getsize(h.LOG)
        t = eb.now_ms()
        hl.js(f"(async () => {{ {js}; return true; }})()")
        st = hl.wait(lambda s: eb.up_to_date(s) and s.get("bookmarks_at", 0) >= t, 60, after=t)
        latency[name + "_ms"] = (st or {}).get("bookmarks_at", 0) - t
        s = conns(settle(hl, mark))
        cur = dev("--dev-cloud-tree", pid)
        delta = eb.new_chunks(prev, cur)
        ops = [o for c in delta for o in c["ops"]]
        label = "7-8" if i == 0 else f"10.{i}"
        check(f"{label} {name}: own connection, 1 event batch, no snapshot, exactly 1 new op {fields}",
              s["starts"] == 1 and s["ends"] == 1 and s["hellos"] == 1 and not s["snapshots"]
              and s["events"] == ["Ok(Applied { ops: 1, pending: 1 })"] and len(delta) == 1 and len(ops) == 1 and ops[0]["fields"] == fields
              and cur.get("ops") == prev.get("ops") + 1 and cur.get("store_events") == prev.get("store_events") + 1,
              json.dumps({"ops": ops, "events": s["events"]}))
        check(f"{label}b {name}: ack then sync_status, frames in/out 2/3, latency {latency[name + '_ms']} ms (< 10 s, no 30 s stall)",
              s["sent"] == ["hello_result", "bookmarks_ack,sync_status"] and s["frames"] and s["frames"][0][1:] == ("2", "3")
              and 0 < latency[name + "_ms"] < 10_000, f"sent={s['sent']} conn_ms={s['frames'][0][0] if s['frames'] else None}")
        prev = cur

    # 13–14. restart
    hl.stop()
    mark = os.path.getsize(h.LOG)
    t = eb.now_ms()
    hl = eb.Helium(udd)
    st = hl.wait(eb.up_to_date, 90, after=t)
    s = conns(settle(hl, mark))
    cur = dev("--dev-cloud-tree", pid)
    check("13-14 Helium restart: equivalent snapshot on its own connection = 0 ops, cloud unchanged",
          s["snapshots"] == ["Ok(Applied { ops: 0, pending: 0 })"] and not s["events"] and s["sent"][-1:] == ["bookmarks_ack,sync_status"]
          and cur.get("ops") == prev.get("ops") and len(cur["chunks"]) == len(prev["chunks"]) and cur.get("store_events") == prev.get("store_events")
          and cur.get("seq_contiguous"), json.dumps(s["snapshots"]))
    hl.stop()
    print("METRICS " + json.dumps({"latency_ms": latency, "ops": cur.get("ops"), "chunks": [(c["seq_from"], c["seq_to"]) for c in cur["chunks"]],
                                   "store_events": cur.get("store_events"), "tree": eb.rows(cur)}))

    # 15. logs
    log = log_since(run_mark)
    leaks = [w for w in ("fixcheck", "Fix Test", "Folder X", "Alpha") if w in log]
    check("15 native-host.log of this run: no URLs, titles, mapping tokens, access/refresh tokens", not leaks and h.secret_free(log), f"leaks={leaks}")

    # 16. cleanup
    cl = dev("--dev-cleanup", mid)
    gone = dev("--dev-cloud-tree", pid)
    inv = dev("--dev-inventory")
    check("16a storage objects, store_events, profile, mapping, device removed",
          cl.get("purged_objects") == len(cur["chunks"]) and cl.get("deleted_profile") and cl.get("device_deleted")
          and gone.get("objects") == 0 and gone.get("store_events") == 0 and inv.get("profiles") == 0 and inv.get("mappings") == 0 and inv.get("devices") == [],
          json.dumps(cl))
    so = dev("--dev-sign-out")
    check("16b Keychain session removed", so.get("signed_out") and not h.keychain_has_session(), "")
    shutil.rmtree(udd, ignore_errors=True)
    check("16c disposable Helium profile + host manifest removed", not os.path.exists(udd), "")
    con = sqlite3.connect(eb.STATE_DB)
    counts = {t: con.execute(f"select count(*) from {t}").fetchone()[0] for t in ("profiles", "entities", "chromium_index", "ops", "chunks", "ext_cursor", "lease")}
    con.close()
    if all(v == 0 for v in counts.values()):
        for sfx in ("", "-wal", "-shm"):
            if os.path.exists(eb.STATE_DB + sfx):
                os.remove(eb.STATE_DB + sfx)
    bdir = os.path.join(eb.APPDATA, "backups", "ext-storage")
    for n in os.listdir(bdir) if os.path.isdir(bdir) else []:
        if n.endswith(h.EXT_ID):
            shutil.rmtree(os.path.join(bdir, n), ignore_errors=True)
    check("16d test state.sqlite empty and deleted; store backups removed",
          not os.path.exists(eb.STATE_DB) and not any(n.endswith(h.EXT_ID) for n in (os.listdir(bdir) if os.path.isdir(bdir) else [])),
          json.dumps(counts))


if __name__ == "__main__":
    eb.RUN["creds"] = sys.argv[1]
    try:
        main(sys.argv[1])
    except Exception as e:  # noqa: BLE001
        check("harness completed without exception", False, type(e).__name__ + ": " + str(e)[:200])
        eb.emergency_cleanup()
    failed = [n for n, ok in h.results if not ok]
    print(f"\n=== fix validation: {len(h.results) - len(failed)} pass, {len(failed)} fail ===")
    if failed:
        print("  failures: " + ", ".join(failed))
    sys.exit(1 if failed else 0)
