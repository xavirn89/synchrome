"""End-to-end validation of the mapping-token handshake (M1 slice 6).

Drives the REAL debug app binary (its --dev-* CLI and its native-messaging host mode), the
REAL Synchrome extension (apps/extension) loaded into Helium with a DISPOSABLE
--user-data-dir, and native messaging between them. Never prints tokens, JWTs or passwords.

Phases:
  python3 e2e_handshake.py unauth                 # no login needed
  python3 e2e_handshake.py auth <creds-env-file>  # needs USER_A_*/USER_B_* test users
"""
import json, os, re, shutil, signal, struct, subprocess, sys, time

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
APP_BUILD = os.path.join(ROOT, "target", "debug", "synchrome-desktop")
# The host Helium launches must NOT live under ~/Documents (or Desktop/Downloads): macOS TCC
# blocks dyld's open() of the executable while it waits on a Documents-folder consent for the
# responsible app (Helium) — observed: host processes hung pre-main, extension timed out. So
# every phase runs a byte-identical copy (same ad-hoc cdhash => same Keychain identity) from an
# unprotected directory, for BOTH the dev CLI (sign-in) and the native host.
APP = os.path.join(os.environ.get("E2E_BIN", "/tmp/synchrome-e2e-bin"), "synchrome-desktop")
EXT = os.path.join(ROOT, "apps", "extension")
HELIUM = "/Applications/Helium.app/Contents/MacOS/Helium"
EXT_ID = "ceflmoojogokmklgcmcihdgakbefgiee"  # store id (pre-store: gmpmcnknoaogpjdoonlddhpdpgkebbog)
ORIGIN = f"chrome-extension://{EXT_ID}/"
WORK = os.environ.get("E2E_WORK", "/tmp/synchrome-e2e")
LOG = os.path.expanduser("~/Library/Application Support/Synchrome/logs/native-host.log")
REGISTRY = os.path.expanduser("~/Library/Application Support/Synchrome/local_tokens.json")


def prepare_binary():
    """Copy the freshly built binary out of ~/Documents and export the PUBLIC client config
    (SUPABASE_URL / SUPABASE_ANON_KEY) so the host found by Helium can load it. Values are
    never printed."""
    os.makedirs(os.path.dirname(APP), exist_ok=True)
    shutil.copy2(APP_BUILD, APP)
    env_file = os.path.join(ROOT, ".env")
    if os.path.exists(env_file):
        for line in open(env_file):
            k, _, v = line.strip().partition("=")
            if k in ("SUPABASE_URL", "SUPABASE_ANON_KEY") and v:
                os.environ.setdefault(k, v.strip().strip('"'))

results = []
OUTPUTS = []  # every dev-CLI stdout, audited for secrets at the end


def check(name, ok, detail=""):
    results.append((name, bool(ok)))
    print(f"  [{'PASS' if ok else 'FAIL'}] {name}" + (f" — {detail}" if detail else ""))


def dev(*args):
    p = subprocess.run([APP, *args], capture_output=True, text=True, timeout=120)
    OUTPUTS.append((args[0] if args else "", p.stdout))
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
    rc, msgs = host_session(struct.pack("=I", 256 * 1024 + 1))
    check("host: oversized frame -> error too_large, exit 3, body never read",
          rc == 3 and msgs and msgs[0].get("code") == "too_large", f"rc={rc}")
    rc, msgs = host_session(frame({"type": "hello", "ext_version": "e2e"}), origin="chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/")
    check("host: foreign extension origin rejected (exit 2, no reply)", rc == 2 and not msgs, f"rc={rc}")

    rc, msgs = host_session(frame({"type": "hello", "ext_version": "e2e"}) + frame(
        {"type": "bookmarks_events", "epoch": "e", "batch_id": "b",
         "events": [{"n": 1, "kind": "removed", "id": "1"}]}) + frame({"type": "sync_now"}))
    types = [m.get("type") for m in msgs]
    check("host: bookmarks / sync_now without a confirmed mapping -> error not_confirmed, ack rejected",
          rc == 0 and types == ["hello_result", "error", "bookmarks_ack", "error", "bookmarks_ack"]
          and msgs[1].get("code") == "not_confirmed" and msgs[2].get("result") == "rejected", f"{types}")

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


HEX64 = re.compile(r"(?<![0-9a-f])[0-9a-f]{64}(?![0-9a-f])")
JWT = re.compile(r"eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}")
EXPECTED = {"A": os.environ.get("EXPECTED_A_ID"), "B": os.environ.get("EXPECTED_B_ID")}


def secret_free(text):
    return not HEX64.search(text) and not JWT.search(text) and "refresh_token" not in text


def ext(udd, profile="Default"):
    s = dev("--dev-ext-status", udd, profile)
    return s, (s.get("status") or {})


def view_of(mid):
    views = dev("--dev-list-mappings")
    if not isinstance(views, list):
        return {}, views
    return next((x for x in views if x.get("id") == mid), {}), views


def keychain_has_session():
    r = subprocess.run(["security", "find-generic-password", "-s", "com.synchrome.app", "-a", "session"],
                       capture_output=True)  # no -w/-g: prints attributes only, never the secret
    return r.returncode == 0


def phase_auth(creds):
    print("=== phase auth ===")
    log_start = os.path.getsize(LOG) if os.path.exists(LOG) else 0
    backups = os.path.expanduser("~/Library/Application Support/Synchrome/backups/ext-storage")
    check("precondition: app signed out, no Keychain session", dev("--dev-account").get("signed_in") is False and not keychain_has_session())

    # 1. login A
    a = dev("--dev-sign-in", creds, "A")
    check("1 login real de A", a.get("signed_in") is True and (not EXPECTED["A"] or a.get("user_id") == EXPECTED["A"]),
          f"user_id={a.get('user_id')}")
    if not a.get("signed_in"):
        return
    inv0 = dev("--dev-inventory")
    print(f"     inventory before: profiles={inv0.get('profiles')} mappings={inv0.get('mappings')} devices={len(inv0.get('devices') or [])}")
    # 2. register device
    d = dev("--dev-register-device")
    inv = dev("--dev-inventory")
    me = d.get("device_id")
    check("2 register device (this device row visible to A)", d.get("is_current") is True and me in (inv.get("devices") or []),
          f"device_id={me}")

    tag = os.urandom(3).hex()
    udd = fresh_udd("auth")
    inst = dev("--dev-install-host", udd)
    helium(udd, 10)  # first run: creates the Default profile; extension stores its own status key
    s0, st0 = ext(udd)
    print(f"     before mapping: ui={st0.get('ui')} status={st0.get('status')} keys={s0.get('keys')}")

    # 3-4. cloud profile + mapping to the disposable Helium profile
    m = dev("--dev-create-mapping", f"E2E {tag}", "helium", "Default")
    mid, pid = m.get("mapping_id"), m.get("profile_id")
    inv = dev("--dev-inventory")
    check("3 cloud profile de prueba creado", pid is not None and inv.get("profiles") == (inv0.get("profiles") or 0) + 1,
          f"profile_id={pid} profiles={inv.get('profiles')}")
    check("4 mapping creado al perfil Helium desechable (Default, este device)", mid is not None and m.get("device_id") == me,
          f"mapping_id={mid}")
    # 5. initial state
    v, views = view_of(mid)
    check("5 estado inicial pending_local_write",
          m.get("token_state") == "pending_local_write" and v.get("status") == "token_pending_local_write"
          and v.get("display_status") == "pending_local_write" and v.get("token_confirmed_at") is None,
          f"cloud={v.get('status')} display={v.get('display_status')} confirmed_at={v.get('token_confirmed_at')}")

    # 6-7. real local write
    n_backups = len(os.listdir(backups)) if os.path.isdir(backups) else 0
    w = dev("--dev-write-token", mid, udd)
    wr = w.get("write") or {}
    s1, _ = ext(udd)
    reg = dev("--dev-registry", mid)
    check("6 mappingToken escrito en Local Extension Settings (navegador cerrado)",
          w.get("display_status") == "token_written" and s1.get("has_mapping_token") is True
          and s1.get("token_sha256_prefix") == reg.get("token_sha256_prefix") and reg.get("state") == "written",
          f"display={w.get('display_status')} registry={reg.get('state')} sha_prefix_match={s1.get('token_sha256_prefix') == reg.get('token_sha256_prefix')}")
    n_backups2 = len(os.listdir(backups)) if os.path.isdir(backups) else 0
    check("7 backup creado fuera del perfil, sin token, y claves ajenas preservadas",
          wr.get("created") is False and wr.get("backup_exists") and wr.get("backup_outside_profile")
          and n_backups2 == n_backups + 1 and "synchromeStatus" in (wr.get("backup_keys") or [])
          and wr.get("backup_has_token") is False and wr.get("preserved_keys", 0) >= 1
          and set(s0.get("keys") or []) <= set(s1.get("keys") or []),
          f"preserved_keys={wr.get('preserved_keys')} backup_keys={wr.get('backup_keys')} after_keys={s1.get('keys')}")
    hash_written = s1.get("token_sha256_prefix")
    token_hint = v.get("token_hint")

    # 8-13. Helium + extension + native messaging + Keychain + cloud resolution
    log_mark = os.path.getsize(LOG) if os.path.exists(LOG) else 0
    helium(udd)
    s2, st = ext(udd)
    # byte offset (the log has multi-byte "…" token hints): slice bytes, then decode
    run_log = open(LOG, "rb").read()[log_mark:].decode("utf-8", "replace") if os.path.exists(LOG) else ""
    check("8 extensión lee el token (status ya no es token_missing)", s2.get("has_mapping_token") and st.get("status") not in (None, "token_missing"),
          f"status={st.get('status')}")
    check("9 extensión conecta con com.synchrome.host (host lanzado por Helium)",
          "stage start" in run_log and "hello ext=" in run_log and "-> Connected" in run_log and st.get("status") != "host_unavailable",
          "native-host.log shows a Helium-launched host session")
    check("10 host lee la sesión del Keychain", "session_loaded signed_in=true" in run_log, "")
    check("11 host resuelve el mapping con el JWT de A",
          st.get("mapped") is True and st.get("profile_id") == pid and st.get("device_id") == me and st.get("profile_name") == f"E2E {tag}",
          f"profile={st.get('profile_name')} device_match={st.get('device_id') == me}")
    v, views = view_of(mid)
    reg = dev("--dev-registry", mid)
    confirmed_at = v.get("token_confirmed_at")
    check("12 mapping confirmed con token_confirmed_at",
          v.get("status") == "token_confirmed" and confirmed_at and reg.get("state") == "confirmed"
          and v.get("display_status") == "extension_confirmed",
          f"cloud={v.get('status')} confirmed_at={confirmed_at} registry={reg.get('state')} display={v.get('display_status')}")
    check("13 extensión queda connected", st.get("ui") == "connected" and st.get("status") == "connected" and st.get("token_state") == "confirmed",
          f"ui={st.get('ui')} token_state={st.get('token_state')}")

    # 14. restart keeps the mapping, no new token
    helium(udd)
    s3, st = ext(udd)
    v2, _ = view_of(mid)
    check("14 reinicio de Helium/extensión: sigue connected, mismo token, sin re-confirmar",
          st.get("ui") == "connected" and s3.get("token_sha256_prefix") == hash_written
          and v2.get("token_hint") == token_hint and v2.get("token_confirmed_at") == confirmed_at and v2.get("id") == mid,
          f"same_token={s3.get('token_sha256_prefix') == hash_written} confirmed_at_unchanged={v2.get('token_confirmed_at') == confirmed_at}")

    # 15. other account
    b = dev("--dev-sign-in", creds, "B")
    helium(udd)
    _, st = ext(udd)
    check("15 con B: token de A -> token_other_account sin revelar profile_id/profile_name",
          b.get("signed_in") and (not EXPECTED["B"] or b.get("user_id") == EXPECTED["B"])
          and st.get("status") == "token_other_account" and st.get("ui") == "token_mismatch"
          and st.get("profile_id") is None and st.get("profile_name") is None and st.get("mapped") is False and st.get("device_id") is None,
          f"user_id={b.get('user_id')} status={st.get('status')} profile_id={st.get('profile_id')} profile_name={st.get('profile_name')}")
    binv = dev("--dev-inventory")
    check("15b B no ve nada de A (RLS)", binv.get("profiles") == 0 and binv.get("mappings") == 0 and me not in (binv.get("devices") or []),
          f"profiles={binv.get('profiles')} mappings={binv.get('mappings')}")
    dev("--dev-sign-in", creds, "A")

    # 16-17. mapping deleted, then cloud profile deleted
    prof2 = "Profile E2E"
    m2 = dev("--dev-create-mapping", f"E2E2 {tag}", "helium", prof2)
    mid2, pid2 = m2.get("mapping_id"), m2.get("profile_id")
    w2 = dev("--dev-write-token", mid2, udd)
    dev("--dev-unmap", mid2)
    helium_profile(udd, prof2)
    _, st = ext(udd, prof2)
    check("16 mapping eliminado -> mapping_deleted",
          w2.get("display_status") == "token_written" and st.get("ui") == "token_mismatch" and st.get("status") == "mapping_deleted"
          and st.get("profile_name") is None, f"status={st.get('status')}")
    dev("--dev-delete-profile", pid2)
    helium_profile(udd, prof2)
    _, st = ext(udd, prof2)
    check("17 profile eliminado -> cloud_profile_deleted",
          st.get("ui") == "token_mismatch" and st.get("status") == "cloud_profile_deleted", f"status={st.get('status')}")
    dev("--dev-cleanup", mid2)

    # 18. random token, signed in
    prof3 = "Profile Rnd"
    helium_profile(udd, prof3, 10)
    seed = dev("--dev-seed-random-token", udd, prof3)
    helium_profile(udd, prof3)
    s4, st = ext(udd, prof3)
    check("18 token aleatorio -> token_unknown (con sesión)",
          s4.get("has_mapping_token") and st.get("status") == "token_unknown" and st.get("ui") == "token_mismatch",
          f"status={st.get('status')} seeded={'token_hint' in seed}")

    # 21 (origin) while signed in with a valid mapping on disk
    rc, msgs = host_session(frame({"type": "hello", "ext_version": "e2e"}), origin="chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/")
    rc2, msgs2 = host_session(frame({"type": "hello", "ext_version": "e2e"}), origin=f"chrome-extension://{EXT_ID}x/")
    rc3, msgs3 = host_session(frame({"type": "ping"}))
    check("21d solo se acepta el origin de Synchrome (ajeno y casi-igual rechazados con sesión activa)",
          rc == 2 and not msgs and rc2 == 2 and not msgs2 and rc3 == 0 and msgs3 and msgs3[0].get("type") == "pong",
          f"foreign rc={rc} lookalike rc={rc2} own rc={rc3}")

    # 19. no session
    dev("--dev-sign-out")
    helium(udd)
    _, st = ext(udd)
    check("19 sin sesión -> app_unavailable / not_signed_in", st.get("ui") == "app_unavailable" and st.get("status") == "not_signed_in",
          f"ui={st.get('ui')} status={st.get('status')}")
    # 20. host unavailable
    os.remove(os.path.join(udd, "NativeMessagingHosts", "com.synchrome.host.json"))
    helium(udd)
    _, st = ext(udd)
    check("20 host no disponible -> app_unavailable / host_unavailable", st.get("ui") == "app_unavailable" and st.get("status") == "host_unavailable",
          f"ui={st.get('ui')} status={st.get('status')}")

    # 21. security audit
    log_text = open(LOG).read() if os.path.exists(LOG) else ""
    check("21a ningún mappingToken completo / access / refresh token en native-host.log",
          secret_free(log_text), f"log bytes this run={len(log_text) - log_start}")
    leaks = sorted({cmd for cmd, o in OUTPUTS if not secret_free(o) or '"mapping_token"' in o})
    reg_text = open(REGISTRY).read() if os.path.exists(REGISTRY) else ""
    check("21b ningún secreto en las salidas del CLI ni token en claro en el registro local",
          not leaks and not JWT.search(reg_text), f"{len(OUTPUTS)} outputs scanned; offending commands={leaks}")
    check("21c mappingToken no llega al frontend (vistas sin token ni campo mapping_token)",
          secret_free(json.dumps(views)) and '"mapping_token"' not in json.dumps(views), "")

    # 22. cleanup
    dev("--dev-sign-in", creds, "A")
    c = dev("--dev-cleanup", mid)
    inv = dev("--dev-inventory")
    check("22a cleanup cloud: mappings, profiles y devices de A a cero",
          c.get("unmapped") and c.get("deleted_profile") and c.get("device_deleted")
          and inv.get("profiles") == 0 and inv.get("mappings") == 0 and inv.get("devices") == [],
          f"profiles={inv.get('profiles')} mappings={inv.get('mappings')} devices={len(inv.get('devices') or [])}")
    reg = [dev("--dev-registry", x).get("found") for x in (mid, mid2)]
    check("22b registro local sin entradas de esta prueba", reg == [False, False], "")
    so = dev("--dev-sign-out")
    check("22c sesión Keychain eliminada", so.get("signed_out") is True and not keychain_has_session()
          and dev("--dev-account").get("signed_in") is False, "")
    shutil.rmtree(udd, ignore_errors=True)
    check("22d manifest del host y perfil desechable eliminados", not os.path.exists(udd), udd)


if __name__ == "__main__":
    os.makedirs(WORK, exist_ok=True)
    if not os.path.exists(APP_BUILD):
        sys.exit(f"build the app first: {APP_BUILD}")
    prepare_binary()  # once, before any sign-in: login and host then use the same file
    if sys.argv[1] == "unauth":
        phase_unauth()
    elif sys.argv[1] == "auth":
        phase_auth(sys.argv[2])
    failed = [n for n, ok in results if not ok]
    print(f"\n=== {sys.argv[1]}: {len(results) - len(failed)} pass, {len(failed)} fail ===")
    if failed:
        print("  failures: " + ", ".join(failed))
    sys.exit(1 if failed else 0)
