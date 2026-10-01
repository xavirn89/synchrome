"""E9 (OCC) — validate the e9_occ migration end to end, as a real client would.

Run AFTER Codex applies cloud/migrations/0001_e9_occ_experiment.sql.
Uses only SUPABASE_URL + ANON_KEY for client ops; the service role is used solely to
create and delete two test users. No DB access. Asserts user-JWT Storage writes under the
user's own prefix, RLS isolation between accounts, and atomic compare-and-swap via the
e9_cas_put RPC. No secrets printed.

Usage: python3 e9_occ.py [/path/to/env]
"""
import json, sys, urllib.request, urllib.error, uuid

ENV = sys.argv[1] if len(sys.argv) > 1 else "/tmp/e9-synchrome.env"
UA = "Synchrome-E9/0.1"
E = {}
for line in open(ENV):
    line = line.rstrip("\n")
    if line and not line.startswith("#") and "=" in line:
        k, v = line.split("=", 1)
        E[k.strip()] = v.strip()
URL = E["SUPABASE_URL"].rstrip("/")
ANON = E["SUPABASE_ANON_KEY"]
SR = E["SUPABASE_SERVICE_ROLE_KEY"]
BUCKET = "e9-experiments"
STORE = str(uuid.uuid4())
PASS, FAIL = [], []


def req(method, path, *, token=None, apikey=ANON, body=None, raw=False, headers=None):
    h = {"apikey": apikey, "User-Agent": UA}
    if token:
        h["Authorization"] = "Bearer " + token
    if headers:
        h.update(headers)
    data = None
    if body is not None:
        data = body if raw else json.dumps(body).encode()
        if not raw:
            h.setdefault("Content-Type", "application/json")
    r = urllib.request.Request(URL + path, data=data, method=method, headers=h)
    try:
        resp = urllib.request.urlopen(r, timeout=30)
        return resp.status, dict(resp.headers), resp.read()
    except urllib.error.HTTPError as ex:
        return ex.code, dict(ex.headers), ex.read()
    except Exception as ex:
        return -1, {}, str(ex).encode()


def check(name, cond, detail=""):
    (PASS if cond else FAIL).append(name)
    print(f"  [{'PASS' if cond else 'FAIL'}] {name}" + (f" — {detail}" if detail else ""))
    return cond


def cas(token, path, expected, hlc=None, device=None):
    st, _, b = req("POST", "/rest/v1/rpc/e9_cas_put", token=token,
                   body={"p_store_id": STORE, "p_path": path, "p_expected_version": expected,
                         "p_hlc": hlc, "p_device_id": device})
    row = (json.loads(b)[0] if st == 200 and json.loads(b) else {}) if st == 200 else {}
    return st, row


print(f"=== E9 OCC against {URL.split('//')[1]} (store {STORE[:8]}) ===")

# Create + login two users
users = {}
for who in ("A", "B"):
    email = f"e9occ+{who.lower()}-{uuid.uuid4().hex[:8]}@synchrome.test"
    pw = "E9-" + uuid.uuid4().hex
    st, _, b = req("POST", "/auth/v1/admin/users", apikey=SR, token=SR,
                   body={"email": email, "password": pw, "email_confirm": True})
    uid = json.loads(b).get("id") if st in (200, 201) else None
    st, _, b = req("POST", "/auth/v1/token?grant_type=password", body={"email": email, "password": pw})
    tok = json.loads(b).get("access_token") if st == 200 else None
    users[who] = {"id": uid, "tok": tok}
    check(f"user {who} ready", bool(uid and tok))

A, B = users["A"]["tok"], users["B"]["tok"]
uidA = users["A"]["id"]
path = "profiles/p1/profile.json"

# 1. Create CAS row (expected=0 -> version 1)
st, row = cas(A, path, 0, hlc="100-0-A")
check("CAS create (expected 0 -> v1)", st == 200 and row.get("ok") is True and row.get("new_version") == 1,
      f"status {st} row {row}")

# 2. User A uploads an object under their own prefix with the USER JWT
obj = f"{uidA}/{STORE}/{path}"
st, _, _ = req("POST", f"/storage/v1/object/{BUCKET}/{obj}", token=A,
               body=b'{"v":1}', raw=True, headers={"Content-Type": "application/json", "x-upsert": "true"})
check("user-JWT upload under own prefix", st in (200, 201), f"status {st}")

# 3a. User B cannot read A's object
st, _, _ = req("GET", f"/storage/v1/object/{BUCKET}/{obj}", token=B)
check("user B denied reading A's object (RLS)", st in (400, 403, 404), f"status {st}")

# 3b. User B cannot write under A's prefix
st, _, _ = req("POST", f"/storage/v1/object/{BUCKET}/{obj}", token=B,
               body=b'{"evil":1}', raw=True, headers={"Content-Type": "application/json", "x-upsert": "true"})
check("user B denied writing under A's prefix (RLS)", st in (400, 403), f"status {st}")

# 3c. User B cannot see A's CAS row: a create at the same (store,path) as B succeeds independently
st, rowB = cas(B, path, 0)
check("user B's CAS row is isolated from A (own row created at v1)",
      st == 200 and rowB.get("ok") is True and rowB.get("new_version") == 1, f"row {rowB}")

# 4. CAS race: two writers both expect version 1 on A's row -> exactly one wins
st1, r1 = cas(A, path, 1, hlc="200-0-A")
st2, r2 = cas(A, path, 1, hlc="200-0-A2")
winners = [r for r in (r1, r2) if r.get("ok")]
losers = [r for r in (r1, r2) if r.get("ok") is False]
check("CAS race: exactly one winner", len(winners) == 1 and len(losers) == 1,
      f"r1 {r1} r2 {r2}")
check("loser sees current_version = 2", losers and losers[0].get("current_version") == 2,
      f"loser {losers[0] if losers else None}")

# 5. Correct retry with expected=2 -> v3
st, row = cas(A, path, 2, hlc="300-0-A")
check("CAS retry (expected 2 -> v3)", row.get("ok") is True and row.get("new_version") == 3, f"row {row}")

# 6. Stale expected value is rejected
st, row = cas(A, path, 1)
check("CAS stale expected rejected", row.get("ok") is False and row.get("current_version") == 3, f"row {row}")

# Teardown: remove A's object + both users (service role)
req("DELETE", f"/storage/v1/object/{BUCKET}/{obj}", apikey=SR, token=SR)
for who in ("A", "B"):
    if users[who]["id"]:
        req("DELETE", f"/auth/v1/admin/users/{users[who]['id']}", apikey=SR, token=SR)
# CAS rows for the deleted users remain owned by now-removed accounts; harmless in the
# experimental table. The migration rollback drops the whole table.

print(f"\n=== E9 OCC summary: {len(PASS)} pass, {len(FAIL)} fail ===")
if FAIL:
    print("  failures:", ", ".join(FAIL))
    sys.exit(1)
