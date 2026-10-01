# 04 — Account, Stores and Backends

Answers brief question 17 and the account model. All store I/O is performed by the desktop app's background service (`05-architecture.md`); the extension never talks to a store.

## 1. Account model

Synchrome has a **cloud account** (Supabase Auth on a private Supabase instance operated by the project). The account is the unit of ownership:

```text
Account (user)
├── Devices        one per installed app; name, platform, last seen, which profiles it maps where
├── Stores         ≥1; the first is always "Synchrome Cloud" (this Supabase instance)
│                  optional extra stores: GitHub repository, Google Drive (later), local/network folder
└── Profiles       "Personal", "Trabajo", "Dev" … each belongs to exactly one store
```

- Signing in on a new computer is all it takes to see the account's profiles and import them.
- Alternative stores exist for users who want their data outside our server. Their credentials (GitHub token, Drive OAuth, folder path) are entered once in the app and kept in the OS keychain; the account only records *that* a store exists and which profiles live there, never its secrets.
- The profile list, device list and store list are account metadata in Postgres. The synced browser data itself lives in whichever store the profile belongs to.

## 2. What the sync model needs from a store

| # | Requirement | Hard? |
|---|---|---|
| R1 | Put/get/list/delete objects by path | yes |
| R2 | Per-object atomic replace (multi-object atomic commit is a bonus) | yes |
| R3 | Compare-and-swap for the few shared objects (snapshots, `profile.json`) | strongly preferred |
| R4 | Sizes: bookmarks ≤ 2 MB, prefs ≤ 1 MB, extension settings up to tens of MB, history ~1–5 MB/month/device | yes |
| R5 | Write rate: a few writes/min/device, bursts of ~10 objects per sync | yes |
| R6 | Change notification (push) or cheap conditional polling | push preferred |
| R7 | Reachable from a background service without a browser session | yes |
| R8 | Optional client-side encryption must be possible (opaque bytes) | yes |

The store layout (`03` §4) keeps per-device objects owner-written, so even no-CAS stores converge safely.

## 3. Default store: Synchrome Cloud (private Supabase)

- **Objects** in Supabase Storage, bucket `stores`, path prefix `<accountId>/<storeId>/…` following the layout in `03` §4. RLS policies on `storage.objects` restrict each account to its own prefix (verified in E9: without policies the bucket is service-role-only; the client uses the user JWT under prefix policies, and the service role never ships in the app).
- **Compare-and-swap** (verified in E9): the standard Storage REST endpoint **ignores `If-Match`/`If-None-Match`**, so there is no native object CAS. Synchrome emulates it in Postgres: a `synchrome.objects` row `(account_id, store_id, path, version int, hlc, device_id)` guards every *shared mutable* object (`store.json`, `profiles/*/profile.json`, snapshots). A write is: `PATCH /rest/v1/synchrome_objects?path=eq.<p>&version=eq.<n>` with `version = n+1` (atomic; 0 rows affected = stale = retry), then upload the bytes to Storage. Per-device journals are owner-written and need no CAS. Storage's per-write `version` UUID (from the `info` endpoint) is the opaque `StorageAdapter` version token used for cheap change detection. (The S3-compatible endpoint, present at `/storage/v1/s3`, supports real conditional writes with S3 keys and remains an alternative if we later prefer object-level CAS.)
- **Metadata** in Postgres: `accounts`, `devices`, `stores`, `profiles`, `profile_scopes`, `synchrome.objects` (CAS guard above), `store_events` (append-only log: which path changed, by which device, HLC). RLS per account.
- **Push** (verified in E9, ~110 ms broadcast latency): Supabase Realtime over WebSocket → devices learn about changes within ~0.1 s and fetch only the changed objects. Polling (5 min) remains as fallback. **Client note:** every HTTP and WebSocket request must send an explicit `User-Agent`; the Cloudflare front returns `403 error code 1010` for default library agents (verified).
- **Journals**: because Postgres is available, journal chunks can *also* be rows (`ops` table keyed by `(profile_id, device_id, seq)`) instead of files. Decision: **keep the object layout for all stores** (one code path, one format for export/import between stores) and use Postgres only for metadata, events and locks. Revisit if row-based journals prove materially cheaper.
- **Quotas**: per-account soft quota (default 2 GB) enforced server-side; history retention default 365 days; compaction keeps snapshots small.
- **Why Supabase**: already operated privately by the project; Auth, Storage, Postgres, RLS and Realtime in one place; local Docker stack for development; no vendor lock-in beyond Postgres + S3-compatible storage.

## 4. Alternative stores (user-chosen, optional)

### 4.1 GitHub repository

- Contents API ≤ 1 MB per file; Git Data API for blobs up to 100 MB; **atomic multi-file commits** (tree → commit → ref) with ref-level CAS (`force:false` → 422 on non-fast-forward); 5,000 req/h, secondary limit **500 content-generating requests/h**; conditional GETs free; fine-grained PAT, repo-scoped, no expiry allowed on personal accounts; CORS `*`.
- Mitigations: one commit per sync, debounce ≥ 60 s, directories sharded (3,000-entry limit), periodic snapshot + orphan-commit squash.
- Encryption default **on** (provider is not us). Bonus: each commit is a recoverable point.

### 4.2 Local or network folder (Syncthing, NAS, Dropbox, iCloud, OneDrive)

- Atomic rename on POSIX / `MoveFileEx(REPLACE_EXISTING)` on Windows; no CAS; cloud-sync tools create "conflicted copy" files instead of failing → the per-device layout makes journals collision-free; a sweeper reconciles conflict copies of snapshots.
- Zero configuration; also the **test adapter** for the whole engine.

### 4.3 Google Drive (later)

- `drive.file`/`drive.appdata` scopes are non-sensitive but production use beyond 100 users needs brand verification (owned domain + privacy policy); no secret-free refresh-token flow for a public client → the app would need a small token-exchange endpoint (an edge function on our Supabase fits). Drive v3 has no ETag CAS; emulate with `version`. Postponed.

### 4.4 WebDAV, S3-compatible (later)

- Nextcloud/ownCloud honour `If-Match`/`If-None-Match` (412); NAS vendors vary. AWS S3 and Cloudflare R2 support conditional PUT; Backblaze B2 does not; MinIO is archived. Both are straightforward adapters once the interface exists.

## 5. Comparison

| Criterion | Synchrome Cloud | GitHub | Folder | Drive | WebDAV | S3/R2 |
|---|---|---|---|---|---|---|
| User effort | sign in | create repo + token | pick folder | OAuth + our relay | app password | cloud account + keys |
| CAS | ✅ (Postgres lock / ETag) | ✅ ref | ❌ | ❌ | ✅ Nextcloud | ✅ AWS, R2 |
| Push notifications | ✅ Realtime | ❌ poll | ❌ (fs watch locally) | ❌ | ❌ | ❌ |
| Multi-object atomic | via lock | ✅ commit | ❌ | ❌ | ❌ | ❌ |
| Who holds the data | the project's server | GitHub | user | Google | user's server | cloud vendor |
| E2E encryption default | off (optional) | on | off | on | off | on |

## 6. Decision

- **v1 ships**: Synchrome Cloud (default, Supabase), GitHub repository (opt-in), local folder (opt-in; also used by tests).
- **Later**: S3/R2, WebDAV, Google Drive.
- Profiles can be moved between stores by export → import of the object tree (same layout everywhere).

## 7. Storage adapter interface (v1)

```ts
interface StorageAdapter {
  readonly capabilities: {
    atomicBatch: boolean;        // GitHub: true; Supabase: via lock; folder: false
    conditionalWrite: boolean;   // Supabase/GitHub/S3/Nextcloud: true; folder: false
    push: boolean;               // Supabase Realtime: true; others: false → poll
    listIsConsistent: boolean;   // folder on cloud-sync: false
  };
  get(path: string): Promise<{ data: Uint8Array; version: string } | null>;
  list(prefix: string): Promise<Array<{ path: string; version: string; size: number }>>;
  put(path: string, data: Uint8Array, opts?: { ifVersion?: string | null }): Promise<{ version: string }>;
  delete(path: string, opts?: { ifVersion?: string }): Promise<void>;
  batch(ops: Array<PutOp | DeleteOp>, opts?: { message?: string }): Promise<void>;
  subscribe?(prefix: string, onChange: (paths: string[]) => void): () => void;   // push-capable stores
}
```

`version` is opaque (ETag, git blob sha, mtime+size). Adapters raise `VersionMismatch` on CAS failure; the engine re-reads and retries. `EncryptedStore` wraps any adapter and is byte-agnostic.

## 8. Supabase schema sketch (packages/cloud)

```sql
create table accounts   (id uuid primary key references auth.users, created_at timestamptz, quota_bytes bigint);
create table devices    (id uuid primary key, account_id uuid references accounts, name text, platform text,
                         app_version text, last_seen timestamptz, mappings jsonb);
create table stores     (id uuid primary key, account_id uuid references accounts, kind text check (kind in
                         ('cloud','github','folder','drive','webdav','s3')), label text, created_at timestamptz);
create table profiles   (id uuid primary key, account_id uuid references accounts, store_id uuid references stores,
                         name text, icon text, scopes jsonb, strategies jsonb, created_at timestamptz);
create table store_events (id bigserial primary key, account_id uuid, store_id uuid, profile_id uuid,
                         device_id uuid, path text, hlc text, created_at timestamptz default now());
-- CAS guard for shared mutable objects (store.json, profile.json, snapshots): verified necessary in E9
create table synchrome_objects (account_id uuid, store_id uuid, path text, version int not null default 0,
                         hlc text, device_id uuid, primary key (account_id, store_id, path));
-- RLS: every table filtered by account_id = auth.uid(); storage.objects policy on name like auth.uid() || '/%'
-- (E9 verified storage.objects is RLS-locked to service role until these policies exist)
```

Secrets for alternative stores never enter these tables.
