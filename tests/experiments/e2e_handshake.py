"""End-to-end validation of the mapping-token handshake (M1 slice 6).

Drives the REAL debug app binary (its --dev-* CLI and its native-messaging host mode), the
REAL Synchrome extension (apps/extension) loaded into Helium with a DISPOSABLE
--user-data-dir, and native messaging between them. Never prints tokens, JWTs or passwords.

Phases:
  python3 e2e_handshake.py unauth                 # no login needed
  python3 e2e_handshake.py auth <creds-env-file>  # needs USER_A_*/USER_B_* test users
"""
import json, os, shutil, signal, struct, subprocess, sys, time

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
APP = os.path.join(ROOT, "target", "debug", "synchrome-desktop")
EXT = os.path.join(ROOT, "apps", "extension")
HELIUM = "/Applications/Helium.app/Contents/MacOS/Helium"
EXT_ID = "gmpmcnknoaogpjdoonlddhpdpgkebbog"
ORIGIN = f"chrome-extension://{EXT_ID}/"
WORK = os.environ.get("E2E_WORK", "/tmp/synchrome-e2e")
LOG = os.path.expanduser("~/Library/Application Support/Synchrome/logs/native-host.log")

results = []


def check(name, ok, detail=""):
    results.append((name, bool(ok)))
    print(f"  [{'PASS' if ok else 'FAIL'}] {name}" + (f" — {detail}" if detail else ""))


def dev(*args):
    p = subprocess.run([APP, *args], capture_output=True, text=True, timeout=120)
    line = (p.stdout.strip().splitlines() or ["{}"])[-1]
    try:
        return json.loads(line)
    except json.JSONDecodeError:
        return {"error": f"non-json output (rc={p.returncode})"}


def helium(udd, secs=16):
    args = [HELIUM, f"--user-data-dir={udd}", "--headless=new", "--no-first-run",
            "--no-default-browser-check", "--disable-gpu", "--profile-directory=Default",
            f"--load-extension={EXT}", f"--disable-extensions-except={EXT}", "about:blank"]
    p = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(secs)
    p.send_signal(signal.SIGTERM)
    try:
        p.wait(25)
    except subprocess.TimeoutExpired:
        p.kill()
    # wait for SingletonLock to disappear (browser fully closed)
    for _ in range(40):
        if not os.path.lexists(os.path.join(udd, "SingletonLock")):
            break
        time.sleep(0.25)


def helium_profile(udd, profile, secs=16):
    p = subprocess.Popen([HELIUM, f"--user-data-dir={udd}", "--headless=new", "--no-first-run",
                          "--no-default-browser-check", "--disable-gpu", f"--profile-directory={profile}",
                          f"--load-extension={EXT}", f"--disable-extensions-except={EXT}", "about:blank"],
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(secs)
    p.send_signal(signal.SIGTERM)
    try:
        p.wait(25)
    except subprocess.TimeoutExpired:
        p.kill()
    for _ in range(40):
        if not os.path.lexists(os.path.join(udd, "SingletonLock")):
            break
        time.sleep(0.25)


def fresh_udd(name):
    udd = os.path.join(WORK, name)
    shutil.rmtree(udd, ignore_errors=True)
    os.makedirs(udd)
    return udd


def frame(obj_or_bytes):
    body = obj_or_bytes if isinstance(obj_or_bytes, bytes) else json.dumps(obj_or_bytes).encode()
    return struct.pack("=I", len(body)) + body


def host_session(stdin_bytes, origin=ORIGIN, timeout=30):
    p = subprocess.run([APP, origin], input=stdin_bytes, capture_output=True, timeout=timeout)
    out, msgs = p.stdout, []
    while len(out) >= 4:
        (n,) = struct.unpack("=I", out[:4])
        msgs.append(json.loads(out[4:4 + n]))
        out = out[4 + n:]
    return p.returncode, msgs


def log_tail_has_no_full_tokens(tokens):
    try:
        text = open(LOG).read()
    except FileNotFoundError:
        return True
    return not any(t and t in text for t in tokens)


# ---------------------------------------------------------------------------- phases
def phase_unauth():
    print("=== phase unauth (no login) ===")
    st = dev("--dev-account")
    check("precondition: app signed out", st.get("signed_in") is False, f"signed_in={st.get('signed_in')}")

    # Real host binary, framing over real stdio
    rc, msgs = host_session(frame({"type": "hello", "ext_version": "e2e"}))
    check("host: Hello without token -> token_missing / unmapped",
          rc == 0 and msgs and msgs[0].get("status") == "token_missing" and msgs[0].get("ui_state") == "unmapped",
          f"rc={rc} reply={msgs[0] if msgs else None}")
    rc, msgs = host_session(frame({"type": "ping"}) + frame(b"{not json") + frame({"type": "ping"}))
    types = [m.get("type") for m in msgs]
    check("host: ping, invalid JSON and ping again -> pong / error invalid_message / pong (stream stays usable)",
          rc == 0 and types == ["pong", "error", "pong"] and msgs[1].get("code") == "invalid_message", f"{types}")
    rc, msgs = host_session(struct.pack("=I", 64 * 1024 + 1))
    check("host: oversized frame -> error too_large, exit 3, body never read",
          rc == 3 and msgs and msgs[0].get("code") == "too_large", f"rc={rc}")
    rc, msgs = host_session(frame({"type": "hello", "ext_version": "e2e"}), origin="chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/")
    check("host: foreign extension origin rejected (exit 2, no reply)", rc == 2 and not msgs, f"rc={rc}")

    # Real extension in Helium, native messaging end to end
    udd = fresh_udd("unauth")
    inst = dev("--dev-install-host", udd)
    check("native host manifest installed in disposable profile", "manifest" in inst, os.path.basename(str(inst.get("manifest", ""))))
    helium(udd)
    s = dev("--dev-ext-status", udd, "Default")
    st = s.get("status") or {}
    check("extension loaded in Helium, connected to host: no token -> unmapped",
          st.get("ui") == "unmapped" and st.get("status") == "token_missing" and s.get("has_mapping_token") is False,
          f"ui={st.get('ui')} status={st.get('status')}")

    seed = dev("--dev-seed-random-token", udd, "Default")
    check("random unmapped token written with production writer (store existed, keys preserved)",
          "token_hint" in seed and seed.get("created") is False and seed.get("preserved_keys", 0) >= 1,
          f"created={seed.get('created')} preserved={seed.get('preserved_keys')}")
    helium(udd)
    s = dev("--dev-ext-status", udd, "Default")
    st = s.get("status") or {}
    check("extension read token, app not signed in -> app_unavailable / not_signed_in",
          st.get("ui") == "app_unavailable" and st.get("status") == "not_signed_in" and s.get("has_mapping_token") is True,
          f"ui={st.get('ui')} status={st.get('status')}")
    check("extension status record carries no token", "mappingToken" not in json.dumps(st) and all(len(str(v)) < 64 or k == "detail" for k, v in st.items()), "")

    os.remove(os.path.join(udd, "NativeMessagingHosts", "com.synchrome.host.json"))
    helium(udd)
    s = dev("--dev-ext-status", udd, "Default")
    st = s.get("status") or {}
    check("native host not installed -> app_unavailable / host_unavailable",
          st.get("ui") == "app_unavailable" and st.get("status") == "host_unavailable", f"ui={st.get('ui')} status={st.get('status')}")
    shutil.rmtree(udd, ignore_errors=True)


def phase_auth(creds):
    print("=== phase auth ===")
    a = dev("--dev-sign-in", creds, "A")
    check("app signs in A (session stored in Keychain by the app binary)", a.get("signed_in") is True, f"user_id={a.get('user_id')}")
    if not a.get("signed_in"):
        return
    a_id = a["user_id"]
    tag = os.urandom(3).hex()
    udd = fresh_udd("auth")
    dev("--dev-install-host", udd)
    helium(udd, 10)  # first run: creates the Default profile (status: unmapped)

    # 1. cloud mapping
    m = dev("--dev-create-mapping", f"E2E {tag}", "helium", "Default")
    check("1 cloud mapping created (pending_local_write)", m.get("token_state") == "pending_local_write", f"mapping {m.get('mapping_id')}")
    mid = m.get("mapping_id")
    # 2. local write (production path: write_token_for_mapping)
    w = dev("--dev-write-token", mid, udd)
    check("2 token written locally -> UI state token_written", w.get("display_status") == "token_written", f"display_status={w.get('display_status')}")
    # 3-6. Helium + extension + native messaging + resolution
    helium(udd)
    s = dev("--dev-ext-status", udd, "Default")
    st = s.get("status") or {}
    check("3-6 extension read token, host resolved mapping -> connected",
          st.get("ui") == "connected" and st.get("status") == "connected" and st.get("mapped") is True
          and st.get("token_state") == "confirmed" and st.get("profile_name") == f"E2E {tag}",
          f"ui={st.get('ui')} profile={st.get('profile_name')} token_state={st.get('token_state')}")
    # 7-8. cloud confirmed + UI view
    views = dev("--dev-list-mappings")
    v = next((x for x in views if isinstance(views, list) and x.get("id") == mid), {}) if isinstance(views, list) else {}
    check("7 cloud mapping confirmed", v.get("status") == "token_confirmed", f"status={v.get('status')}")
    check("8 UI view shows Extension confirmed, no token in payload",
          v.get("display_status") == "extension_confirmed" and "mapping_token" not in json.dumps(views), f"display_status={v.get('display_status')}")

    # negative: token of another account (written as A, app now signed in as B)
    b = dev("--dev-sign-in", creds, "B")
    helium(udd)
    st = (dev("--dev-ext-status", udd, "Default").get("status") or {})
    check("token written under A, app signed in as B -> token_mismatch / token_other_account",
          b.get("signed_in") and st.get("status") == "token_other_account" and st.get("ui") == "token_mismatch", f"status={st.get('status')}")
    dev("--dev-sign-in", creds, "A")

    # negative: mapping deleted between write and handshake, then its cloud profile too
    prof2 = "Profile E2E"
    m2 = dev("--dev-create-mapping", f"E2E2 {tag}", "helium", prof2)
    mid2, pid2 = m2.get("mapping_id"), m2.get("profile_id")
    w2 = dev("--dev-write-token", mid2, udd)
    check("second mapping written for a second local profile", w2.get("display_status") == "token_written", f"display_status={w2.get('display_status')}")
    dev("--dev-unmap", mid2)  # cloud mapping gone; local registry entry kept (like the app's Unmap)
    helium_profile(udd, prof2)
    st = (dev("--dev-ext-status", udd, prof2).get("status") or {})
    check("mapping deleted after the local write -> token_mismatch / mapping_deleted",
          st.get("ui") == "token_mismatch" and st.get("status") == "mapping_deleted", f"status={st.get('status')}")
    dev("--dev-delete-profile", pid2)
    helium_profile(udd, prof2)
    st = (dev("--dev-ext-status", udd, prof2).get("status") or {})
    check("...and its cloud profile deleted -> token_mismatch / cloud_profile_deleted",
          st.get("ui") == "token_mismatch" and st.get("status") == "cloud_profile_deleted", f"status={st.get('status')}")
    dev("--dev-cleanup", mid2)  # removes the registry entry

    check("host log contains no full token", log_tail_has_no_full_tokens([]), LOG)

    # cleanup: mapping, profile, device row, Keychain session, disposable dirs
    c = dev("--dev-cleanup", mid)
    check("cleanup: mapping, profile and device removed", c.get("unmapped") and c.get("deleted_profile") and c.get("remaining_mappings") == 0,
          json.dumps(c))
    so = dev("--dev-sign-out")
    check("cleanup: app signed out (Keychain item removed)", so.get("signed_out") is True, "")
    shutil.rmtree(udd, ignore_errors=True)


if __name__ == "__main__":
    os.makedirs(WORK, exist_ok=True)
    if not os.path.exists(APP):
        sys.exit(f"build the app first: {APP}")
    if sys.argv[1] == "unauth":
        phase_unauth()
    elif sys.argv[1] == "auth":
        phase_auth(sys.argv[2])
    failed = [n for n, ok in results if not ok]
    print(f"\n=== {sys.argv[1]}: {len(results) - len(failed)} pass, {len(failed)} fail ===")
    if failed:
        print("  failures: " + ", ".join(failed))
    sys.exit(1 if failed else 0)
