"""E9 (HTTP) — validate a self-hosted Supabase instance as a Synchrome store.

Covers: Cloudflare/UA, connection, Auth (admin-create + login of two users), PostgREST,
Storage CRUD + metadata (etag/version), RLS default on storage.objects, conditional-write
(CAS) support on the standard endpoint, S3-endpoint presence, and a two-writer lost-update
demonstration with version tracking. Reads credentials from argv[1] (default
/tmp/e9-synchrome.env). No secrets printed. Resources named `e9-*`, torn down unless --keep.

Usage: python3 e9_http.py [/path/to/env] [--keep]
"""
import json, sys, urllib.request, urllib.error, uuid

ENV = sys.argv[1] if len(sys.argv) > 1 and not sys.argv[1].startswith("--") else "/tmp/e9-synchrome.env"
KEEP = "--keep" in sys.argv
UA = "Synchrome-E9/0.1"  # any non-default UA avoids Cloudflare error 1010


def load_env(path):
    e = {}
    for line in open(path):
        line = line.rstrip("\n")
        if line and not line.startswith("#") and "=" in line:
            k, v = line.split("=", 1)
            e[k.strip()] = v.strip()
    return e


E = load_env(ENV)
URL = E["SUPABASE_URL"].rstrip("/")
ANON = E["SUPABASE_ANON_KEY"]
SR = E["SUPABASE_SERVICE_ROLE_KEY"]
BUCKET = "e9-experiments"
PASSES, FAILS = [], []


def req(method, path, *, token=None, apikey=ANON, body=None, headers=None, raw=False):
    url = path if path.startswith("http") else URL + path
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
    r = urllib.request.Request(url, data=data, method=method, headers=h)
    try:
        resp = urllib.request.urlopen(r, timeout=30)
        return resp.status, dict(resp.headers), resp.read()
    except urllib.error.HTTPError as ex:
        return ex.code, dict(ex.headers), ex.read()
    except Exception as ex:
        return -1, {}, str(ex).encode()


def check(name, cond, detail=""):
    (PASSES if cond else FAILS).append(name)
    print(f"  [{'PASS' if cond else 'FAIL'}] {name}" + (f" — {detail}" if detail else ""))
    return cond


def obj_info(key, token=SR):
    st, _, b = req("GET", f"/storage/v1/object/info/{BUCKET}/{key}", apikey=SR, token=token)
    if st == 200:
        try:
            return json.loads(b)
        except Exception:
            return {}
    return {}


print(f"=== E9 HTTP against {URL.split('//')[1]} ===")

check("auth health", req("GET", "/auth/v1/health")[0] == 200)

users = {}
for who in ("A", "B"):
    email = f"e9+{who.lower()}-{uuid.uuid4().hex[:8]}@synchrome.test"
    pw = "E9-" + uuid.uuid4().hex
    st, _, b = req("POST", "/auth/v1/admin/users", apikey=SR, token=SR,
                   body={"email": email, "password": pw, "email_confirm": True})
    uid = json.loads(b).get("id") if st in (200, 201) else None
    check(f"admin create user {who}", uid is not None, f"status {st}")
    st, _, b = req("POST", "/auth/v1/token?grant_type=password", body={"email": email, "password": pw})
    tok = json.loads(b).get("access_token") if st == 200 else None
    check(f"login user {who}", bool(tok), f"status {st}")
    users[who] = {"id": uid, "tok": tok}

check("postgrest reachable", req("GET", "/rest/v1/", token=users["A"]["tok"])[0] == 200)

st, _, b = req("POST", "/storage/v1/bucket", apikey=SR, token=SR,
               body={"id": BUCKET, "name": BUCKET, "public": False})
check("create bucket", st in (200, 201) or b"already exists" in b, f"status {st}")

key = f"acct-A/{uuid.uuid4().hex[:8]}/store.json"
p1 = json.dumps({"v": 1}).encode()

# RLS: a plain authenticated user must NOT be able to write without a policy (expected).
st, _, b = req("POST", f"/storage/v1/object/{BUCKET}/{key}", token=users["A"]["tok"],
               body=p1, raw=True, headers={"Content-Type": "application/json"})
check("storage.objects RLS denies user write by default (needs policy)",
      st in (400, 403) and b"row-level security" in b, f"status {st}")

# Object lifecycle via service role (production will use user JWT + prefix RLS policy).
st, _, _ = req("POST", f"/storage/v1/object/{BUCKET}/{key}", apikey=SR, token=SR,
               body=p1, raw=True, headers={"Content-Type": "application/json"})
check("upload object (service role)", st in (200, 201), f"status {st}")

st, _, gb = req("GET", f"/storage/v1/object/{BUCKET}/{key}", apikey=SR, token=SR)
check("download object matches", st == 200 and gb == p1, f"status {st}")

i1 = obj_info(key)
etag1, ver1 = i1.get("etag"), i1.get("version")
check("object metadata has etag + version", bool(etag1) and bool(ver1),
      f"etag {etag1} version {ver1} size {i1.get('size')}")

# List prefix
pref = key.rsplit("/", 1)[0]
st, _, lb = req("POST", f"/storage/v1/object/list/{BUCKET}", apikey=SR, token=SR,
                body={"prefix": pref + "/", "limit": 100})
names = [o.get("name") for o in (json.loads(lb) if st == 200 else [])]
check("list prefix", st == 200 and any("store.json" in (n or "") for n in names), f"names {names}")

# Update → etag + version change
p2 = json.dumps({"v": 2}).encode()
req("POST", f"/storage/v1/object/{BUCKET}/{key}", apikey=SR, token=SR,
    body=p2, raw=True, headers={"Content-Type": "application/json", "x-upsert": "true"})
i2 = obj_info(key)
check("update changes etag + version", i2.get("etag") != etag1 and i2.get("version") != ver1,
      f"etag {etag1}->{i2.get('etag')} version {ver1}->{i2.get('version')}")

# CAS probe on the standard Storage endpoint
st_inm, _, _ = req("POST", f"/storage/v1/object/{BUCKET}/{key}", apikey=SR, token=SR,
                   body=p1, raw=True, headers={"x-upsert": "true", "If-None-Match": "*", "Content-Type": "application/json"})
st_im, _, _ = req("POST", f"/storage/v1/object/{BUCKET}/{key}", apikey=SR, token=SR,
                  body=p2, raw=True, headers={"x-upsert": "true", "If-Match": '"stale"', "Content-Type": "application/json"})
print(f"      standard endpoint: If-None-Match:* -> {st_inm}, If-Match stale -> {st_im} (412 = honored)")
check("conditional writes honored on standard Storage endpoint", st_inm == 412 or st_im == 412,
      "if both not 412, CAS must be emulated (see RESULTS)")

# S3-compatible endpoint presence
st_s3, _, _ = req("GET", "/storage/v1/s3", apikey=SR, token=SR)
print(f"      /storage/v1/s3 -> {st_s3} (not 404 = S3-compatible endpoint present; supports If-Match with S3 keys)")

# Two-writer lost update, tracked by version
req("POST", f"/storage/v1/object/{BUCKET}/{key}", apikey=SR, token=SR,
    body=json.dumps({"writer": "A"}).encode(), raw=True, headers={"x-upsert": "true", "Content-Type": "application/json"})
vA = obj_info(key).get("version")
req("POST", f"/storage/v1/object/{BUCKET}/{key}", apikey=SR, token=SR,
    body=json.dumps({"writer": "B"}).encode(), raw=True, headers={"x-upsert": "true", "Content-Type": "application/json"})
iB = obj_info(key)
st, _, fb = req("GET", f"/storage/v1/object/{BUCKET}/{key}", apikey=SR, token=SR)
winner = json.loads(fb).get("writer") if st == 200 else None
print(f"      blind concurrent writes: winner={winner}, version {vA}->{iB.get('version')} "
      f"(version changes every write → usable as a CAS token via the info endpoint)")

if not KEEP:
    req("DELETE", f"/storage/v1/object/{BUCKET}/{key}", apikey=SR, token=SR)
    req("POST", f"/storage/v1/bucket/{BUCKET}/empty", apikey=SR, token=SR, body={})
    stx = req("DELETE", f"/storage/v1/bucket/{BUCKET}", apikey=SR, token=SR)[0]
    for who in ("A", "B"):
        if users[who]["id"]:
            req("DELETE", f"/auth/v1/admin/users/{users[who]['id']}", apikey=SR, token=SR)
    print(f"  teardown done (bucket delete {stx}, users removed).")
else:
    print("  --keep: resources left in place.")

print(f"\n=== E9 HTTP summary: {len(PASSES)} pass, {len(FAILS)} fail ===")
if FAILS:
    print("  failures:", ", ".join(FAILS))
