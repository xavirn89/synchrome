# 03 — Data Model, Synchronization Strategy, Conflicts, Deletions, Recovery, Encryption

Answers brief questions 9, 10, 12–16. This is the normative part of the specification: anything an implementation must do is written as "must".

## 1. Identities

```ts
type DeviceId  = string;   // UUID v4, generated once per agent installation, stored in agent config
type ProfileId = string;   // UUID v4, generated when a Synchrome profile is created in a store
type StoreId   = string;   // UUID v4, written into store.json when the store is initialised
```

- A **device** is one agent installation (one OS user account on one machine). Its id never changes; a reinstall creates a new device, old journals remain valid.
- A **Synchrome profile** is an abstract "Personal / Work / Dev" identity that lives in exactly one store. It is *not* a Chromium profile.
- A **mapping** binds, on one device, a Synchrome profile to one local Chromium profile: `(deviceId, profileId) → { browser, userDataDir, profileDir }`. Mappings are device-local configuration and are also published to the store (in the device record) for display ("Work is mapped on 3 devices"). Local folder names (`Profile 2`, `Default`) therefore never matter to the sync model.
- A Chromium profile can be mapped to at most one Synchrome profile. A Synchrome profile can be mapped to any number of local profiles, including several on the same device (e.g. "Personal" in both Helium and Chrome on one Mac) — decision of 2026-10-01, enforced by `profile_mappings` in migration 0003.

The device record in the store:

```json
{
  "deviceId": "6fba69b7-…",
  "name": "Xavi MacBook",
  "platform": "darwin",
  "agentVersion": "0.1.0",
  "lastSeen": "2026-10-01T09:50:00Z",
  "mappings": { "<profileId>": { "browser": "helium", "browserVersion": "154.0.8037.92", "profileDir": "Profile 2" } }
}
```

## 2. Clocks and ordering

Every operation carries a **hybrid logical clock (HLC)** timestamp `hlc = { wallMs, counter, deviceId }`, serialised as a fixed-width sortable string `"<wallMs:13>-<counter:4>-<deviceId>"`. HLC gives a total order that respects causality for anything a device has already seen, tolerates moderate wall-clock skew, and makes "last writer wins" deterministic across devices (ties broken by `deviceId`). Devices must refuse to generate ops when their wall clock is more than 24 h behind the maximum HLC they have observed, and must warn the user when the skew exceeds 5 minutes.

## 3. Entities and collections

All synchronised state is modelled as **entities with independently timestamped fields** (per-field LWW registers), plus tombstones. One generic shape serves every collection:

```ts
interface Entity<F> {
  id: string;                        // collection-specific stable id (see below)
  fields: { [K in keyof F]: { value: F[K]; hlc: HLC } };
  deleted?: { hlc: HLC; by: DeviceId };   // tombstone
}
```

| Collection | Entity id | Fields (all LWW) | Live source | Apply path |
|---|---|---|---|---|
| `bookmarks` | Synchrome UUID | `parentId`, `pos` (fractional index), `title`, `url` (absent for folders), `kind` (`url`/`folder`), `root` (`bar`/`other`/`mobile`), `createdAt` (ms, informational) | `chrome.bookmarks` events | live, API |
| `extensions` | Chromium extension id | tombstone = uninstalled, `enabled` (bool), `name`, `version`, `source` (class, not the update URL), `type` (§3.4); `pinned` later | `chrome.management` | live (enable/disable), guided (install), confirmed (uninstall) |
| `prefs` | pref path (`"bookmark_bar.show_on_all_tabs"`) | `value` (JSON) | agent file diff + `chrome.privacy` where applicable | closed-profile file write; live for API-settable keys |
| `extsettings` | `<extId>/<area>/<key>` (`area` = `local`/`sync`) | `value` (JSON) | agent LevelDB diff | closed-profile store rewrite |
| `history.urls` | canonical URL | `title`, `typedCount` (max-merge, not LWW), `hidden` | `chrome.history` | closed-profile SQLite |
| `history.visits` | `sha256(url ‖ visitTimeUs ‖ originDevice)` | `url`, `visitTimeUs`, `transition`, `originDevice` (immutable; grow-only set) | `onVisited`, initial `getVisits` import | closed-profile SQLite |
| `history.deletions` | UUID | `kind` (`url`/`range`), `url?`, `fromUs?`, `toUs?` | `onVisitRemoved` | both store filtering and SQLite delete |
| `profile` | `"meta"` | `name`, `icon`, per-category `enabled` flags, `historyRetentionDays`, conflict strategies | UI | n/a |

Rules:

- `history.visits` is a grow-only set (G-Set): union on merge, never conflicts. Deletions are separate tombstone entities that filter the set (a visit matching a deletion is suppressed on apply and dropped at compaction).
- `history.urls.typedCount` and `visitCount` are derived (max / recount) — never synced as LWW values.
- Bookmark `pos` uses fractional indexing (base-62 strings between siblings) so concurrent inserts into the same folder interleave deterministically without renumbering; ties broken by entity id.
- Field-level LWW is the universal default; collection-specific structural rules are in §6.

### 3.1 Bookmarks — normative details (fixed by M1 slice 7, 2026-10-02)

Clarifications of the table above, decided when the first real capture was implemented. They refine, not replace, the model.

- **Identity.** `id` is a Synchrome UUID v4 minted by the device that first captures the node. Chromium node ids (`"12"`) are *local* and only live in the device's `state.sqlite` index `(profileId, chromiumId) → id`; they are never published and never used as identity elsewhere.
- **Roots are not entities.** A top-level node has `parentId = "root:bar" | "root:other" | "root:mobile"` (from Chromium `folderType` `bookmarks-bar` / `other` / `mobile`, falling back to the well-known ids `1`/`2`/`3`). `root` is the root sentinel's suffix and is recomputed for a whole subtree when it moves between roots. Managed bookmarks (`unmodifiable: "managed"`) and account-storage nodes (`syncing: true`, Chromium's "bookmarks in your Google Account") are excluded from capture.
- **Fields on the wire** (op `fields`): `parentId`, `pos`, `title`, `kind` (`url`/`folder`), `root`, `url` (url kind only), `createdAt` (Chromium `dateAdded`, ms, sent on creation only). A creation op carries all fields; later ops carry only the fields that changed.
- **`pos`** is a fractional key over the alphabet `0-9A-Za-z` (ASCII order = numeric order), never ending in `0`. Keys for `k` new siblings between two neighbours are produced by balanced bisection (lengths grow with log k, not k). A move changes only the moved node's `pos` (unless it already lies between its new neighbours, which makes replays no-ops); `onChildrenReordered` keeps the longest run of siblings whose keys are already in the new order and re-keys only the others.
- **HLC string**: `"<wallMs:13>-<counter:4>-<deviceId>"` with the **full** device UUID (the 8-char form in the examples above is illustrative). One HLC per op applies to every field the op sets.
- **Snapshot reconcile** (initial capture, every extension start, after import, after a `resync_required`): the full tree is diffed against the materialised state through the Chromium index — unknown Chromium ids become creations (first trying to pair with an un-indexed live entity by `(parentId, title, url)`, e.g. after the index was lost), known ids emit only changed fields, indexed ids missing from the tree emit deletes (a folder's descendants are deleted explicitly, one tombstone each, so §6 rule 1 never sees an orphan). Entities that were never indexed on this device are never deleted by a reconcile. Repeating an unchanged snapshot emits nothing.
- **Chunk sealing.** The device's open journal chunk is sealed when a publication starts (and still rotates at 256 KB or at a month boundary). A sealed chunk is immutable, so each publication uploads only the new ops and a retried upload is byte-identical; ops captured during a publication go to the next chunk. This replaces "the current chunk is rewritten in place" in §4 — it trades more, smaller objects (which compaction folds into snapshots) for delta-only uploads and no rewrite races.
- **Interim constraint**: one capture source per `(profile, device)` — a second local profile mapped to the same Synchrome profile on the same device is refused with an error (both would share one journal; lifted when same-device multi-source merge exists). The slice-7 refusal of profiles with other devices' journals was lifted by slice 8 (§3.2 bootstrap).

### 3.2 Remote apply — normative details (fixed by M1 slice 8, 2026-10-02)

- **Download.** The puller lists `store_events` of the profile, ignores its own device, and per remote device downloads only chunks past its cursor (`remote_cursors.applied_through`). A chunk is verified before use (ops contiguous, first/last `seq` = the event's range, every op authored by the journal's device); its ops and the cursor advance in one transaction, so nothing is applied twice and a crash loses nothing. A missing event or object is a **gap**: that device stops there (recorded) until it is filled. Duplicate events are skipped. Polling (the extension's 1-minute alarm and the app's 30 s loop) is the wake-up; from slice 13 Realtime adds an early wake-up hint (§3.7 A), never a source of truth.
- **Merge.** Per-field LWW by HLC (`entities.field_hlc`); a `set` newer than the tombstone resurrects; a delete older than the newest field value is ignored; an unknown entity's delete is kept as a tombstone. Remote HLCs are observed so later local ops sort after them.
- **Conflict.** If the entity has a LOCAL op not yet published (`local_seq > published_through`) — or already has an unresolved conflict — the remote op is not applied: it is held in the entity's conflict record (`local_pending`) and remote apply stops for that entity only. A failed browser operation (e.g. deleting a folder that still holds a local child) is an `apply_failed` conflict; a remote create/move into a folder deleted on this device is an `orphan`. Resolution: §3.3.
- **Apply.** Every entity whose browser-visible state changed is `pending_apply` (with the changed field names). The planner emits explicit operations in a safe order — creates top-down (a created folder can parent later creates by op reference), rename / URL / move, one exact `reorder` per affected folder, deletes bottom-up and non-recursive — capped at 512 KiB per plan. Results set the Chromium index for created nodes and clear `pending_apply` only when every op tied to the entity succeeded (missing results = retried; "not found" on delete = done).
- **Echo suppression (no time windows).** (1) The remote state is materialised *before* the apply, so the browser is brought to the state Synchrome already holds. (2) Entities with `pending_apply` are frozen: the normaliser never turns what the browser reports about them into local ops. (3) The extension drops `chrome.bookmarks` events from the start of an apply until the reconcile snapshot that immediately follows it, and that snapshot is diffed against the materialised state: applied changes produce no diff, a concurrent user edit produces a real one. (4) The extension always sends its own events before asking for remote changes, so a local change can never be overtaken by an apply. A worker that dies mid-apply is covered by the startup snapshot (created nodes pair with their pending entities instead of duplicating) and the plan is simply re-sent (operations are idempotent).
- **Bootstrap.** A device never publishes before one successful pull, and a joining device pulls *before* its first snapshot, so identical local nodes pair with existing entities (§5 bootstrap step 3) instead of being uploaded again. This replaces the slice-7 refusal of profiles that had other devices' journals.

### 3.3 Conflict resolution — normative details (fixed by M1 slice 9, 2026-10-02)

**Where conflicts live.** A conflict is *local reconciliation state* of one device, kept in `state.sqlite` — not a shared cloud entity. It exists because this device's own unpublished changes, its browser, or its local deletions met a remote op; every other device has already converged by plain per-field LWW and has nothing to decide. A decision is published as **ordinary journal ops with a fresh HLC** (later than every version involved, because the device's clock has observed all of them), so every device converges on it through the normal cycle, and no cloud schema, RLS policy or migration is needed. This replaces the `conflicts/<hlc>-<deviceId>.json` objects of §4/§7 for bookmarks: two devices can each hold their own symmetric record (e.g. `apply_failed` here, `orphan` there); a decision on either emits ops that make the other one moot, and it is then superseded automatically.

**Record** (`conflicts`, one *open* record per entity, enforced by a partial unique index): `conflict_id` (UUID v4, stable), `account_id`, `profile_id`, entity id, `kind`, `subtype`, `status`, `created_at`, `updated_at`; LOCAL / REMOTE / BASE snapshots (`exists`, `title`, `url`, `parent`, `pos`, per-field HLC, the field *groups* each side changed; REMOTE also carries the author device and `seq`); `detail` (a short code such as `not_empty`), `attempts`; once decided `resolution`, the chosen version, the HLC of the op that carries it, the number of ops emitted, `resolved_at`. The held remote ops are stored in `conflict_remote` (canonical line, device, `seq`, HLC). Records are never deleted on resolution (history); they go with the profile's local state.
- **Groups.** Fields are decided in groups: `title`, `url`, `location` (`parentId` + `pos` + derived `root` — a move is never split), `existence`.
- **REMOTE** = the held ops folded in HLC order (per-field LWW, a newer set resurrects). Several remote ops on the same entity attach to the same record in order; the remote cursor still advances (nothing is downloaded twice), and nothing held is skipped: the decision is taken against the folded REMOTE version.
- **BASE.** When an entity gets its first unpublished local change, the previous values of the changed fields are recorded (`entities.base`, with the `seq` that starts the unpublished run, so a stale record is ignored). BASE is therefore known for the fields this device changed and is otherwise LOCAL; it may be partial (a database from before slice 9).

**Kinds / subtypes.** `local_pending` with `edit_edit`, `edit_delete` (local edit vs remote delete), `delete_edit` (local delete vs remote edit), `delete_delete`; `apply_failed` with `delete_not_empty` (a remote folder delete refused because the folder still holds items Synchrome did not delete) or `apply_error` (any other browser failure); `orphan`.

**When a remote op is held.** Only if it is *relevant*: newer than this device's pre-edit state for some field it sets (or than all of it, for a delete). BASE keeps the pre-edit HLCs for that. A stale op — one per-field LWW would discard even without the local edits, e.g. another device's old edit published late while it sorted out its own conflict — merges normally instead of opening a conflict. Without a BASE record the op is held (conservative).

**Status.** `unresolved` → `resolving` (decision persisted, the browser is being brought to it by the next apply plan) → `resolved`; or `superseded` when later changes made the record moot — both sides made the same change, the remote side only re-asserted this device's BASE (three-way rule: nothing new arrived, the local edit wins; it is re-emitted with a fresh HLC when LWW alone would pick the remote op), both deleted, a folder whose refused delete no longer holds anything (the remote delete is then applied), the remote side restored a folder whose delete had failed, an orphan's folder exists again or the orphan was deleted. Superseding merges the held ops by LWW (no browser work when the values are already the browser's). A failed apply of a decision sends the same record back to `unresolved` as `apply_failed` (attempts + 1) with both snapshots intact — no version is ever lost.

**Resolutions** (validated against the record's options; a repeat of the recorded one is idempotent, any other choice on a closed record is refused):
- **Keep local** — LOCAL wins for every group either side changed: one op (all those fields, or every field when the remote had deleted it, so it resurrects there) and the record is `resolved` at once (no browser work). For `delete_edit` it is a delete (the remote edit is discarded; confirmation required). For `delete_not_empty` it keeps the folder (full op, resurrects it elsewhere). For an orphan it *accepts the deletion*: the item and its descendants are deleted (confirmation required).
- **Keep remote** — REMOTE wins for the groups it changed; groups only this device changed go back to BASE when known. Applied to the browser through the planner (`resolving` → `resolved` after a successful apply; echo-free like any remote apply: the entity is pending, hence frozen). It still emits one op with a fresh HLC, so a device that had adopted this device's older value converges too. `edit_delete` → delete (with its live descendants; confirmation required). `delete_edit` → restore: the entity, deleted ancestors (the folder chain only, not their other children) and the descendants removed in the same local removal (same capture batch). `delete_not_empty` → delete the folder **and** the local items in it (confirmation required). Orphan → restore the deleted folder chain.
- **Merge** — only `edit_edit` with *disjoint* groups (e.g. local title + remote URL): each side's groups are kept, one op. Two different titles, two different URLs or two different locations always need an explicit choice; nothing is merged automatically.
- **Relocate** — orphan: keep the item in the nearest existing ancestor of its deleted folder (else *Other bookmarks*), appended at the end. `delete_not_empty`: move the folder's local items to the nearest existing ancestor, right after the folder, then delete the folder (the plan's order guarantees moves before the non-recursive delete). No "Recovered by Synchrome" folder is introduced: the nearest surviving ancestor is a place the user already knows.
- **Retry** — `apply_failed`: the held ops are merged again and the entity re-planned; a new failure keeps the record (attempts + 1). Local data is never force-deleted by a retry.
- A kept item whose ancestors have their own open "remote deleted it" record keeps them too (they are resolved with it), so the other devices can place it.

**Flow.** While a record is open only its entity is frozen; every other entity keeps syncing. Resolutions are taken in the app (Rust): the extension only ever receives the resulting apply plan (no protocol change). The app publishes the decision's ops right away (publish only: anything newer merges after it by LWW); the browser is updated on the extension's next pull. A request is honoured only for the signed-in account's own profile on this device: a `conflict_id` of another profile or account is "not found".

### 3.4 Extensions — normative details (fixed by M1 slice 10, 2026-10-02)

**Same machinery as bookmarks, no parallel infrastructure.** Extension observations are ordinary ops with `col: "extensions"` in the same per-device journal (same `seq`, HLC, sealed chunks, `store_events`, checkpoint, lease); the puller routes ops by `col` (unknown collections are skipped, so the bookmarks model never sees them). No cloud migration: the store layout and its policies are collection-agnostic. Devices must run a version that routes by `col` (slice 10+).

- **Identity**: the Chromium extension id (32 letters `a`–`p`); no UUID.
- **Synchronised (per-field LWW)**: installed (`delete` op = uninstalled; a later `set` = installed again), `enabled`, and informational `name`, `version`, `source`, `type`. `source` is a class — `webstore`, `edge_store`, `development`, `sideload`, `other`, `unknown` — derived locally from `installType` + the update-URL host; the update URL, paths, permissions, host permissions and any extension data are never published.
- **Never synchronised**: the Synchrome extension itself, non-extensions (apps, themes — out of scope), policy-installed items (`installType: admin`: listed locally as *Protected*, never published, never touched). Component extensions are not reported by `chrome.management`.
- **Local only (state.sqlite `ext_entities`)**: installed here, enabled here, `mayDisable`, `installType`, "has been installed here" (an absence is an *uninstall* only for something seen here — the analogue of the bookmarks Chromium index), pending apply, explicit user intent (`install`/`remove`), local condition (*user action required*, *protected*, *apply failed*).
- **Capture**: a full `getAll()` reconcile at every worker start (hash-skipped when unchanged) + numbered `onInstalled/onUninstalled/onEnabled/onDisabled` events (own epoch namespace). A disable by Chromium after a permission increase (`disabledReason: permissions_increase`) is a local condition, not a published state. A newer local version updates `name`/`version`.
- **Remote changes**: never install and never uninstall automatically. Missing here → *Missing* with availability (*Install available* = an official store page for this browser; *Unavailable* for unpacked/sideloaded/self-hosted; *Incompatible* e.g. an Edge Add-ons item on Chrome). Uninstalled elsewhere → *Removed on another device* with *Remove here* (confirmed) / *Keep here* (re-publishes it as installed). Enabled-state changes are applied with `chrome.management.setEnabled` when `mayDisable`; otherwise *Protected* (never attempted). A refusal (e.g. user gesture required) → *User action required*: the popup offers it as a click.
- **Apply plans** carry only `set_enabled`, `uninstall` (after the user confirmed *Remove here*) and `open_install` (the user clicked *Install*; only `https://chromewebstore.google.com/detail/<id>` or the Edge Add-ons page, checked again by the extension). Echo suppression as for bookmarks: the entity is pending (frozen) until the browser reports the materialised value; the extension drops `chrome.management` events from the start of an apply until its reconcile snapshot.
- **Conflicts** use the shared records (§3.3, `col = extensions`): a remote op that would change the pre-edit state of an entity with an unpublished local change is held. The three-way rule makes most toggles moot — with a boolean, two concurrent changes from the same BASE either agree or one of them only returns to the BASE — so explicit conflicts are in practice *local toggle vs remote uninstall* (`edit_delete`) and *local uninstall vs remote toggle* (`delete_edit`). Resolutions: Keep local / Keep remote; neither installs nor uninstalls anything by itself (Keep remote on an uninstall → *Removed on another device*).
- **Browser-dependent behaviour (E7)** — validated on Helium 154 only: `setEnabled` from the extension's service worker works without a gesture (development items); `chrome.management.uninstall` of another extension requires a user gesture, and with one Chromium shows its own confirmation dialog. **Not validated on Google Chrome stable / Web Store items**: the design does not assume `setEnabled` works there without a gesture (the refusal path is the popup click). Helium's bundled uBlock Origin reports `mayDisable: true` yet `setEnabled` fails with "cannot be modified by user": that error is classified *protected* (never retried). A reinstall on one device concurrent with a *Keep here* elsewhere is a genuine conflict (reinstalled+enabled vs kept+disabled), resolved like any other.

### 3.5 Extension settings — normative details (fixed by M1 slice 11, 2026-10-02)

**Source.** Chromium gives no extension (Synchrome's included) access to another extension's `chrome.storage`; there is no cross-extension API and none is invented. The app therefore reads and writes the LevelDB stores `<profile>/Local Extension Settings/<id>` (`chrome.storage.local`) and `<profile>/Sync Extension Settings/<id>` (`chrome.storage.sync`) itself (E4: key = UTF-8 key, value = JSON text), **only while the browser of that profile is closed** (no live `SingletonLock`, store `LOCK` free). A copy of a store in use is not trusted: it can miss its newest records, and a missing key would be published as a delete. With the browser open the dataset waits (*Waiting for browser to close*); captures and applies happen in the app's cycle (background loop every 30 s, or *Retry*). `storage.session`, IndexedDB, Local Storage and cookies are out of scope.

**Scope, opt-in.** Per extension and per device: *Off* (default; nothing read, published or written), *Local only* (`storage.local`), *Local + Sync* (both areas, never mixed). Only for extensions in the synchronised inventory, installed here, not protected/policy, never Synchrome itself, never themes/apps. The UI warns that extension settings may contain private data.

**Model.** Same journal/HLC/chunks/store_events/lease and conflict records (`col: "extension_settings"`, id `<extension id>/<area>/<key>`, key may contain `/`). A `set` op carries `{value: <JSON>, ver: <extension version of the writer>}`; a key removed from the store is a `delete` (tombstone). Per-key LWW; state.sqlite v5 `ext_settings_entities` keeps the shared value next to this device's last-seen store value (`local_value`: an absence becomes a delete only for a key seen here — the analogue of the Chromium index), pending writes and conflict flags; `ext_settings_config` keeps the mode, extra denylist, status and exclusion counts per area.
- **Values**: any JSON (null, booleans, numbers, strings, arrays, objects; canonical JSON with sorted object keys, so equal values compare equal). Not JSON → excluded as invalid. Key ≤ 512 B, value ≤ 64 KiB, dataset ≤ 5 MiB per extension and area (keys taken in key order; one that does not fit is skipped without being charged, so smaller later keys still fit); larger → excluded (counted, kept local, never published). A key that is still in the store but excluded this time (oversized, invalid, over the budget, newly denylisted) is **not** gone: it keeps its last synchronised value and never becomes a tombstone — only a key absent from the store is a local delete.
- **Sensitive keys** (denylist, case-insensitive substring of the key name): token, secret, password, passwd, passphrase, api_key/apikey/api-key, auth, session, cookie, credential, private_key, bearer, jwt, otp, seed, mnemonic; plus a per-extension list (exact keys or `prefix*`). Excluded keys are never read into the dataset, never published, never written, and never turned into deletes. **This is a heuristic**: a secret stored under an innocent name, or nested inside an object, is not detected; some harmless keys are excluded ("author").
- **Capture** (browser closed): for each enabled area, keys whose value differs from the shared value are `set` (with `ver`), keys gone since the last capture are deleted; keys with a remote value still to be written are frozen (the store value is stale, not a local decision); repeating the same snapshot emits nothing. A dataset captured for the first time pulls first, so equal values pair and the joining device adopts the synchronised values (its previous store is backed up).
- **Apply** (browser closed): per extension and area, all-or-nothing: backup of the store outside the profile → staging copy → put/delete only Synchrome's keys → verify (written keys read back, deleted keys gone, every other key byte-identical, no unexpected key) → atomic swap; any failure leaves the store untouched. Other extension ids are never opened. After an apply the next equivalent capture produces 0 ops (the written value is recorded as the store value; no time window).
- **Version gate**: a pending value written by another extension version (`ver` ≠ the version installed here) is not applied: *Version mismatch*; the dataset waits until the versions match (no override in this slice).
- **Conflicts**: per key, through the shared records (§3.3): a remote op that would change the pre-edit state of a key with an unpublished local change is held (`edit_edit`, `edit_delete`, `delete_edit`); Keep local / Keep remote; the three-way BASE rule supersedes re-assertions; different keys always merge.

### 3.6 Browser preferences — normative details (fixed by M1 slice 12, 2026-10-02)

**Source.** `<profile>/Preferences` (JSON), read and written by the app **only while the browser of that profile is closed** (no live `SingletonLock`; Chromium keeps prefs in memory and rewrites the whole file on change and on exit, so an edit made while it runs is lost — docs/01 §3). `Secure Preferences`, `Local State` and every other file are never written. The whole file is never copied between profiles: only allowlisted paths are extracted and only allowlisted paths are written. A Synchrome writer takes an advisory `flock` on `Preferences` for the whole read-modify-swap (Chromium does not use one — the browser-running check is the real guard; the lock serialises Synchrome processes and any other cooperating tool): held by someone else → *Waiting* (retry).

**Allowlist v1** (evidence: E13, `tests/experiments/e13_preferences.py` / `e13b_prefs_detail.py`, disposable profiles, Helium 0.18.2.1 = Chromium 154.0.8037.92 and Google Chrome 154.0.8037.93; for each path: known by the settings UI with this type, persisted in `Preferences` without a MAC, and an external edit made with the browser closed is **kept** after relaunch with no reset marker):

| path | schema | compatibility | why it is portable |
|---|---|---|---|
| `download.prompt_for_download` | boolean | chromium_generic | "Ask where to save each file"; no path, same default (false) in both browsers |
| `browser.enable_spellchecking` | boolean | chromium_generic | spell-check on/off; same default (true); the checker itself is the OS one on macOS (dictionaries are not synchronised) |
| `intl.selected_languages` | string: comma-separated BCP-47 tags, 1–32 tags, each `[A-Za-z]{2,3}(-[A-Za-z0-9]{1,8})*` | chromium_generic | the user's preferred languages. **This is the real source** of `intl.accept_languages`: an external edit of `selected_languages` is kept and `accept_languages` is recomputed from it at startup, while an edit of `accept_languages` alone is reverted (E13b) |
| `bookmark_bar.show_on_all_tabs` | boolean | chromium_generic, **with a Helium representation** | "Show bookmarks bar" on all tabs; same default (false). Chrome stores only this key. Helium 154 also writes `bookmark_bar.visibility_state` (0 = shown on all tabs, 1/2 = not) and, when present, it wins over `show_on_all_tabs` (E13b: an external `false` was reverted by `visibility_state: 0`; removing `visibility_state` makes `show_on_all_tabs` authoritative; Helium's own UI writes `false` exactly as `{show_on_all_tabs: false}` without `visibility_state`). Synchrome reads the logical value (`visibility_state == 0` when present, else `show_on_all_tabs`) and, on Helium, writes `show_on_all_tabs` and removes `visibility_state` — the same shape Helium's UI produces. Chrome ignores `visibility_state` (E13b) and it is never written there. **Helium's settings page control is `bookmark_bar.visibility_state` itself** (NUMBER, default 1; setting 0 also writes `show_on_all_tabs: true`, 1 or 2 write `false`), so a Helium user changes the bar through it; 1 and 2 are both "not on all tabs" and are not distinguished by the synchronised boolean (a remote `false` only rewrites a bar that was shown, so a local 1/2 choice is never overwritten by another 1/2) |

**Evaluated and excluded** (never read into the dataset, never written):
- `browser.show_home_button`, `homepage`, `homepage_is_newtabpage` — **tracked prefs**: stored in `Secure Preferences` with a MAC in both browsers; an external edit is reset at relaunch (E13, and docs/01 §3: enforced on macOS for all builds). Writable only by forging MACs (deferred, E2).
- `intl.accept_languages` — derived from `intl.selected_languages`; an external edit is reverted.
- `spellcheck.dictionaries` — recomputed by Chromium at startup (an external edit was reverted even with the language selected); on macOS the OS spell checker is used.
- `spellcheck.use_spelling_service` — sends typed text to Google's service; enabling it on another device is a privacy change the user must make there.
- `translate.enabled` — different defaults (Helium false, Chrome true), so a reset on one browser means different behaviour on the other; it enables a Google network service.
- `download.default_directory` (and every path-valued pref) — local path.

**Never touched (documented denylist; the allowlist already excludes them, this is the explicit contract):** `account_info`, `google.*`, `signin.*`, `sync.*`, `profile.avatar*` / `profile.*` identity and paths, credentials / `credentials_enable_*`, `password_manager.*`, `autofill.*`, `payments.*`, media and device ids (`media.*`, `media_device_id_salt`, `*device_id*`), extension state (`extensions.*`), `custom_handlers.*` / protocol handlers, site permissions and `profile.content_settings.*` (exceptions and defaults), `profile.default_content_setting_values.*`, enterprise policy state (`*.managed*`, `enterprise_*`, `policy.*`), `protection.*` / MACs, `session.*`, `default_search_provider*`, `pinned_tabs`, experiments / field trials / flags (`browser.enabled_labs_experiments`, `variations*`). A path in this list is refused by the writer even if a future allowlist entry asked for it (defence in depth: the writer checks both lists).

**Compatibility and version.** Each allowlist entry declares a compatibility class — `chromium_generic` (Helium and Google Chrome, the browsers validated by E13), `helium_only`, `chrome_only`, `unsupported` — and `since_major` (154 for all v1 entries). Other browser kinds (Brave, Edge, …) are *Unsupported* until validated. The browser major comes from `<user-data-dir>/Last Version` (E13: present in both, e.g. `154.0.8037.92`); this device's browser kind comes from the mapping. Every `set` op carries `{value, bk: <writer browser kind>, maj: <writer browser major>}`. On apply:
- entry unknown here (written by a newer Synchrome) or incompatible with this browser kind, or this browser's major < `since_major` → **Unsupported** for that path (not written; the shared value is kept and waits);
- writer major ≠ this major and either is newer than `VALIDATED_THROUGH` (154): semantics across those versions are not validated → **Version mismatch** (not written; waits until the majors match or the allowlist is re-validated). Equal majors always pass (same semantics by definition);
- value fails the entry's schema (remote input is untrusted) → not written, counted as invalid.

**Cloud.** No migration: the ops travel in the existing journals (0004 policies are per account/profile, independent of `col`), and a version that does not know `browser_preferences` skips those ops.

**Model.** Same journal/HLC/chunks/store_events/lease and conflict records (`col: "browser_preferences"`, id = the preference path, e.g. `bookmark_bar.show_on_all_tabs`). Per-path LWW. state.sqlite v6 `browser_preferences_entities` (path, shared value, deleted, writer kind/major, field HLCs, local seq, BASE, last observed value + when, remote pending (`pending_apply`), compatibility, conflict/frozen) and `browser_preferences_config` (mode, status, error, browser kind/major observed, counts, last capture/apply).
- **Opt-in** per profile and device: *Off* (default) / *Sync recommended preferences* (the whole allowlist; no per-path toggles in this slice). The UI warns: "Browser preferences may affect how your browser behaves on this device."
- **Regenerated defaults (E2E slice 12).** Removing `download.prompt_for_download` is durable: the key stays absent and the default applies. Removing `intl.selected_languages` is not: Helium rebuilds it at startup from `intl.accept_languages` (the previous languages came back), so the tombstone is followed by a `set` of the regenerated value captured on the next snapshot; both devices converge and go quiet. A regenerated value is an ordinary local value, never a conflict.
- **Absent = browser default.** Chromium stores non-default values (and values the user set explicitly); an allowlisted path absent from the file is "default". A path seen here and gone now (reset to default) is a `delete` (tombstone); applying a tombstone removes the key, so the target browser uses its own default (the v1 entries have equal defaults in Helium and Chrome; `intl.selected_languages` defaults to the OS languages). A path never seen here is not a delete.
- **Capture** (browser closed): extract the allowlist, normalise (logical value, schema check), diff against the last observed values: changed → `set`, gone → `delete`; a value that fails the schema is excluded (counted, never published, never a tombstone); a path with a remote value still to be written is frozen; the same snapshot again → 0 ops. A dataset captured for the first time pulls first (the joining device adopts the synchronised values; its previous file is backed up).
- **Apply** (browser closed): verify profile, browser kind, compatibility, schema and version gate per path; then backup of `Preferences` outside the profile → edit only the allowlisted paths (and the Helium companion of the bookmark bar) **in place in the original bytes** (span splice: replaced values, inserted members, removed members; every other byte unchanged) → validate (new file parses; each written path reads back; the document minus allowlisted paths equals the original minus allowlisted paths) → staging file in the profile directory, fsync, atomic rename → re-read and compare; any failure before the rename leaves the file untouched, a failure after it restores the backup (rollback). Afterwards the written values are the observed values, so the next capture is 0 ops (no echo, no time window).
- **Conflicts**: per path through the shared records (§3.3): `local_pending` `edit_edit` / `edit_delete` / `delete_edit`, Keep local / Keep remote, three-way BASE rule; published changes LWW; different paths always merge.

### 3.7 Compaction and Realtime wake-up — normative design (M1 slice 13, 2026-10-03; needs migration 0005, under review)

Two independent mechanisms. Neither changes the durable model: journals + `store_events` + per-device `seq` + HLC + per-remote-device cursors stay the only authority; Realtime only shortens waits, compaction only replaces an already-agreed prefix of history by a verified, equivalent snapshot.

**A. Realtime wake-up (a hint, never a source of truth).**
- *Subscription.* The app (not the native host, not the extension: they hold no Supabase session) keeps at most **one** Realtime connection per signed-in account, with one channel subscribed to `postgres_changes` `INSERT` on `public.store_events` (`filter: account_id=eq.<uid>`), authenticated with the user's access token (refreshed tokens are pushed to the channel; the service role is never used). RLS (`store_events_select`) is what Realtime enforces, so another account's rows are never delivered. One subscription covers every profile/mapping of the account.
- *Behaviour.* An event is reduced to `(profile_id, device_id)`; events of this device or of profiles not mapped (confirmed) on this device are ignored; otherwise the profile is marked **dirty** and an early sync (pull → apply where possible → publish) is scheduled. The payload (path, seq, hlc) is **never** applied or trusted: the pull re-reads `store_events` from the durable cursor exactly as the periodic loop does.
- *Coalescing (no sync storm).* Per profile: a 500 ms debounce window; at most one sync in flight; events arriving during a sync set a single trailing re-run; a minimum of 2 s between wake-up syncs of the same profile. N events in a burst → at most 2 pulls.
- *Failure model.* Lost, duplicated, reordered or late events and disconnections are all harmless: correctness comes from the cursor-based pull, the 30 s periodic loop stays as the fallback, and every (re)connection triggers one catch-up sync of every mapped profile (events missed while disconnected).
- *Lifecycle.* Exponential backoff with jitter (1 s → 60 s) on connect/join failures; Phoenix heartbeats; a supervisor keyed by account id guarantees a single connection (a new subscription replaces the previous one); logout or an account change closes the socket and drops all state; a deleted profile/mapping is simply ignored on arrival (and its dirty flag dropped).
- *Limits of the hint.* Wake-up accelerates download/materialisation and file-based applies (extension settings, preferences, with the browser closed). Live browser applies (bookmarks, extensions) still happen on the extension's next connection (native messaging is extension-initiated; the extension holds no Supabase credentials), i.e. its alarm.

**B. Compaction.**
- *Snapshot = folded shared state.* A snapshot of a profile at generation `G` is the deterministic fold of `snapshot G-1` (if any) + every journal op with `seq ≤ W_D` for each source device `D` (`W` = the snapshot's **watermarks**), per collection, with exactly the merge of the puller: per-field LWW by HLC, tombstone HLC, a `set` newer than the tombstone resurrects. It is built **only from cloud data** (downloaded chunks), never from a device's local state: local unpublished ops, local conflict records, pending applies, Chromium indexes and observed values are device-local and never enter a snapshot. Collections are folded separately (`bookmarks`, `extensions`, `extension_settings`, `browser_preferences`); unknown collections are carried verbatim (an older builder must not drop a newer collection — if it cannot carry them it does not compact).
- *Entity record* (one NDJSON line): `{"id": <entity id>, "f": {field: value}, "h": {field: hlc}, "d": <deleted>, "dh": <tombstone hlc or null>}` — every field with its own HLC, the tombstone HLC kept even when a later `set` resurrected the entity. Records are sorted by id inside a collection part.
- *Equivalence.* Importing a snapshot = feeding the puller synthetic ops reconstructed from the records (one `set` per distinct field HLC carrying the fields of that HLC, plus a `delete` with `dh`), sorted by HLC, authored by the device embedded in each HLC. Because every collection's merge is per-field last-writer-wins with deterministic tie-breaks (commutative, associative, idempotent), `import(snapshot ≤ W) + replay(ops > W)` yields the same shared state as `replay(all ops)`. This is enforced by property tests over random multi-device histories per collection (including out-of-order delivery, tombstones and resurrection).
- *Watermarks (what may be folded).* For each source device `D`: `W_D = min(P_D, min over active snapshot-capable readers R ≠ D of A_R[D])`, where `P_D` = the last seq of `D`'s **contiguous** `store_events` chain (a gap ends it) and `A_R[D]` = what reader `R` reported as applied (`sync_cursors.applied`). Only history every active device has already applied is folded, so active devices never need the snapshot and conflict semantics for their unpublished edits are unchanged. Readers report their cursors and their `snapshot_protocol` after every pull (`sync_cursors`, server-side `updated_at`). **The database re-validates the watermarks when a generation becomes ready** (it does not trust the builder): every value is an integer; no device of the previous ready generation is dropped or lowered (monotonic); each `W_D` above the previous one is the `seq_to` of a publication and the publications `(W_prev_D, W_D]` form a contiguous chain with none crossing either end (so a watermark is always a chunk boundary and never skips a gap); and `W_D` ≤ the applied cursor of every active (≤ 30 days), non-retired reader with `snapshot_protocol ≥ 1`.
- *Active / dormant / retired / reactivated.* Activity is `sync_cursors.updated_at`, set by the **server** whenever a device reports its cursors. **ACTIVE**: last report ≤ `ACTIVE_GRACE = 30 days` ago and not retired — participates in watermarks. **DORMANT**: last report > 30 days ago — a *derived* state (nothing is written, `retired_at` never changes); it no longer holds compaction back; if it returns after its unapplied history was folded and deleted it **re-bootstraps** (below) and then publishes its own unpublished ops normally (their HLCs keep LWW correct). **RETIRED**: only by an explicit user action in the app ("Remove this device from the profile's sync"); `retired_at` is stamped with server time and is **never** set automatically by age, by a compactor or by the server. A retired device is excluded from watermarks immediately and does not block GC; if it keeps syncing it still pulls/publishes normally (re-bootstrapping when it is behind collected history) but **stays retired**: a cursor report never carries `retired_at`, so nothing reactivates it silently. **REACTIVATE**: only by an explicit user action in the app ("Reactivate"), which clears `retired_at`; the device's next report makes it an active reader again (holding watermarks until it has applied the history). `VERY_INACTIVE = 180 days` is informational only (the UI may suggest removing such a device; it changes nothing). A device row is never deleted while any of its `store_events` lie above the journal-GC watermark (its history would vanish with the cascading FK); retirement does not delete rows.
- *Snapshot capability and old clients.* A client that does not implement this protocol (any build before slice 13) never reads snapshots and never writes `sync_cursors`: it **cannot continue from a history whose journals were garbage-collected** (it would stop at a permanent gap — no corruption, but no sync). Therefore every device sends `snapshot_protocol` (0 = none, 1 = `synchrome-snapshot/1`) with its cursors, and journal GC is refused by the database while any device **mapped to the profile** (`profile_mappings`) has no `sync_cursors` row with `snapshot_protocol ≥ 1` and is not explicitly retired — dormancy does not lift this block, because an old client is invisible to `sync_cursors` (it may be syncing every day). Building and publishing snapshots stays allowed (harmless to old clients: journals remain). Rollout: GC can only start once every mapped device of the profile runs a snapshot-capable build or the user removed the stale ones; the UI names the blocking devices. A future incompatible snapshot format bumps the required protocol the same way.
- *Bootstrap / catch-up.* Before listing events, the puller reads the latest **ready** snapshot row. If it has no local state for the profile, or some cursor `A[D] < W_D` while the events `(A[D], W_D]` are no longer listed (folded and deleted), it **imports** the snapshot (download manifest → verify its SHA-256 against the row → download each part → verify its SHA-256 against the manifest → import part by part, each part in one transaction through the normal merge, so local pending edits become explicit conflicts exactly as with replay) and then sets `A[D] = max(A[D], W_D)`; the tail (`seq > W_D`) is then pulled as usual. A device whose cursors are at or past `W` never downloads the snapshot. If the current snapshot fails verification, the reader uses the previous one (G-1) with the retained tail instead (reported; never imports an unverified object). A chunk listed but missing (deleted between list and download) is treated as "folded" when `seq_to ≤ W_D` of the latest ready snapshot (re-check, then import), and as a gap otherwise.
- *Tombstones.* A tombstone leaves the journals only by being folded into a snapshot record (`d`, `dh` and every field HLC). **v1 keeps every tombstone indefinitely** in every snapshot generation (they are compact: id + HLCs); therefore no late op older than the tombstone can resurrect an entity, whatever device it comes from or how long it was offline. Tombstone garbage collection is a possible future optimisation (it would need a confirmation flow for returning devices, §8) and is not part of v1.
- *Generations kept (current + previous).* The **current** snapshot is the highest `ready` generation G; the **previous** is the next-lower `ready` generation G-1. Both are always kept, and so are all journals with `seq > W(G-1)`: journal GC runs **one generation behind** — when G becomes ready, only ops with `seq_to ≤ W(G-1)_D` become eligible (with a single ready generation, none). So the previous snapshot plus the retained journals rebuild the state independently of the current one: if the current snapshot fails verification on a reader, the reader falls back to G-1 + tail. Generations ≤ G-2 become eligible for cleanup only once G is **stable**: ready for `STABLE_AFTER = 24 h` (server time). Cleanup marks them `superseded` (the server refuses the transition earlier), deletes their objects, then their rows.
  Example (each row = state after that generation became ready and the stated server-time conditions hold):

  | after | ready snapshots kept | current | fallback (if current fails verification) | journals GC-eligible up to | may become `superseded` |
  |---|---|---|---|---|---|
  | G1 ready | G1 | G1 | none needed: all journals still present | nothing (no previous generation) | — |
  | G2 ready, +10 min | G1, G2 | G2 | G1 + journals `> W(G1)` | `W(G1)` | — (G1 is the fallback) |
  | G3 ready, +10 min | G1, G2, G3 | G3 | G2 + journals `> W(G2)` | `W(G2)` | G1, once G3 has been ready 24 h |
  | G4 ready, +10 min | G2, G3, G4 (G1 deleted) | G4 | G3 + journals `> W(G3)` | `W(G3)` | G2, once G4 has been ready 24 h |

  Invariant: journals `> W(G-1)` are never deleted while G is current, and watermarks are monotonic (server-checked), so `G-1 + retained journals` always rebuilds G and everything after it. A generation is superseded only when two newer ready generations exist — i.e. when it is neither current nor the fallback — and the current one is stable (24 h). Once the journals `≤ W(G-1)` are gone, generations older than G-1 have no tail left and serve nothing; cleaning them up is safe.
- *Atomicity (claim → build → upload → verify → ready → GC).* (1) Claim: insert `profile_snapshots (profile, generation = latest + 1)`; the server sets `state = building`, `claimed_at`, `expires_at = claimed_at + 30 min`; the unique `(profile_id, generation)` makes concurrent compactors lose cleanly. (2) Build in the app from cloud data. (3) Upload parts and the manifest under the claimed generation (Storage accepts snapshot objects only for an unexpired `building` row of the caller's account). (4) Verify: re-download every object and check sizes + SHA-256 against the manifest, and the fold's counts. (5) Mark the row `ready` with the manifest path/hash, watermarks, counts, bytes; the server stamps `ready_at`. (6) Journal GC — only when the **database** allows it, through one rule `synchrome_journal_gc_allowed(profile, source device, seq)`: own account (RLS), a previous ready generation exists, `now() ≥ ready_at(G) + 10 min` (database clock, server-stamped `ready_at`), no GC blocker (above), and `seq ≤ W(G-1)_D`. The client deletes chunk objects first (Storage's RESTRICTIVE delete policy applies the same rule, the chunk's file name being its first seq and watermarks being chunk boundaries), then calls the RPC `synchrome_gc_journal_events(profile, device, through)` per source device (SECURITY INVOKER; refuses unless `through` is eligible). 0004's generic owner DELETE on `store_events` is replaced by a policy with the same rule, so even a raw `DELETE` can only remove eligible rows; profile/device deletion still removes rows through FK cascades. (Consequence for tooling: the slice-8 harness command that hides a single `store_events` row to simulate a gap no longer works once 0005 is applied.) (7) Generation cleanup as above. Any failure before (5) leaves journals untouched and the row `building` (an expired claim is removed by the next compactor with its objects); a failure during (6)/(7) leaves extra objects/rows, which the next compaction sweeps (objects under `journal/<D>/` with `seq_to ≤ W(G-1)_D` and no event row). Readers never see a `building` snapshot, and a `ready` snapshot can neither be overwritten nor deleted (Storage policies; its row only goes away with the profile).
- *When.* Any device of the account, from the app's background loop, when the profile's listed history since the last snapshot reaches any of `5 000 ops`, `300 chunks` or `16 MiB`, at most once per 6 h per profile (server time: the last claim's `claimed_at`), never while the device is offline or has unpublished ops of its own older than 10 min (so its own history is in the cloud first).
- *Limits.* Snapshot parts ≤ 3.5 MiB (the bucket accepts ≤ 4 MiB objects), ≤ 2 000 parts; a fold holds one collection's entity map in memory at a time, capped at 250 000 entities / 128 MiB per collection — above that the compactor refuses (reported; journals kept). Import streams part by part.
- *Implementation (slice 13).* `synchrome-sync::snapshot` (fold, records, NDJSON parts, manifest, SHA-256, synthetic ops), `synchrome-sync::wake` (coalescing), `synchrome-state::compaction` (report / compact / gc / import, `shared_state` digest; state.sqlite v7 `compaction_state`), `synchrome-cloud::{compaction, realtime}` (0005 REST/RPC, Phoenix frames — the access token never leaves that crate), app `compaction.rs` (after every sync: cursor report; every 10 min: compaction + GC evaluation) and `realtime.rs` (supervisor). Cursor reports PATCH the device's row and INSERT it when absent (0005 grants no UPDATE on the ids, so a PostgREST upsert is not used). Profile deletion runs in two phases (objects the server allows → the profile row, cascading events/snapshots/cursors → the remaining objects → verify none left).
- *Integrity.* Manifest: `format: "synchrome-snapshot/1"`, `profile_id`, `generation`, `base_generation`, `created_at`, `created_by` (device), `watermarks {device: seq}`, per collection `{entities, tombstones, parts: [{name, bytes, sha256, entities}]}`, `total_bytes`. The row carries `manifest_sha256`; the manifest carries every part's SHA-256; nothing is imported before both match.

## 4. Store layout

```text
<store root>/
├── store.json                       { synchrome: 1, schemaVersion: 1, storeId, createdAt,
│                                      encryption: { enabled, kdf, salt, keyCheck } }
├── devices/<deviceId>.json          device record (§1), rewritten by owner only
├── profiles/<profileId>/
│   ├── profile.json                 name, icon, category flags, strategies (LWW-merged like an entity)
│   ├── journal/<deviceId>/<yyyy>/<mm>/<seq:08>.jsonl     append-only op chunks, written by owner only
│   ├── cursors/<deviceId>.json      { appliedThrough: { <otherDevice>: lastSeqSeen }, snapshot: <id> }
│   ├── snapshots/<hlc>.json         materialised state incl. field HLCs and live tombstones (CAS-protected)
│   ├── history/<deviceId>/<yyyy-mm>.jsonl  visits emitted by that device in that month (G-Set shards)
│   └── conflicts/<hlc>-<deviceId>.json     conflict records (§7), written by detector, deleted on resolution
└── metadata/schema.json             machine-readable schema version + migration log
```

Ownership rule: **a device writes only under paths containing its own `deviceId`**, except `store.json` (init only), `profile.json`/`snapshots/*` (CAS-protected, or LWW with conflict-copy sweep on no-CAS backends) and `conflicts/*` (unique names). This is what makes no-CAS backends (local folder) safe and keeps GitHub commits conflict-free in practice.

A journal chunk is newline-delimited JSON of ops:

```json
{"seq":4121,"hlc":"1790000000000-0003-6fba69b7","col":"bookmarks","id":"c0b4…","op":"set",
 "fields":{"title":"Docs","parentId":"a1f9…","pos":"V"},"by":"6fba69b7"}
{"seq":4122,"hlc":"1790000000001-0000-6fba69b7","col":"bookmarks","id":"77e2…","op":"delete","by":"6fba69b7"}
```

`seq` is per device, strictly increasing, and persisted locally before the op is published so a crash never produces gaps or reuse. Chunks rotate at 256 KB or at month boundary. The current chunk is rewritten in place (append semantics on object stores); with encryption the chunk is re-encrypted on each flush.

## 5. The sync cycle

Each device runs the same loop per mapped profile; there is no master. The loop runs in the **desktop app's background service**; the thin extension, when present, feeds the `collect` step with API events and executes live applies. Scopes switched off for a profile are skipped in `collect` and `apply` but their ops are still pulled, merged and kept in the materialised state, so they can be applied later if the scope is enabled.

```text
collect        local change events (extension) + agent file diffs
  ↓
normalise      → ops with HLC (per-field, only fields that changed vs. local materialised state)
  ↓ debounce   default 60 s after the last change (configurable 10 s–30 min), or manual "Sync now"
pull           list journals/snapshots/history shards; fetch chunks after the stored cursor (ETag/304 cheap)
  ↓
merge          apply remote ops to the local materialised state (per-field LWW + structural rules)
  ↓
plan           compute the diff between materialised state and the real browser state
  ↓
apply-live     bookmarks via API; extension enable/disable; API-settable prefs
apply-queued   prefs / extsettings / history → pending-apply queue, executed by the app when the profile is closed
  ↓
push           append own ops to own journal chunk; update own cursor; one batch/commit
  ↓
compact        (occasionally, any device) write snapshot, prune journals acknowledged by all active devices, one generation behind (§3.7)
```

Remote change detection: push notification where the store supports it (Synchrome Cloud via Realtime), otherwise polling every 5 min (configurable), plus a pull immediately before every push. Devices not seen for 30 days are "dormant" and excluded from the "acknowledged by all" compaction rule; a dormant device that returns bootstraps from the latest snapshot and then merges its own unpublished ops normally (field HLCs make this safe).

**Bootstrap of a new mapping** (joining an existing profile on a new device, or adopting an existing store):

1. Load snapshot + journals → remote state `R`.
2. Read the local browser → local state `L`.
3. Pair entities: bookmarks by `(root, folder path, title, url)`, extensions by id, prefs by path, extsettings by key, history by `(url, visitTime)`. Paired entities adopt the remote id; unpaired local entities become new ops with `hlc = now` (union, never deletion); unpaired remote entities are created locally.
4. The user sees a preview ("will add 143 bookmarks, 2 extensions, 37 prefs; nothing will be deleted") and confirms. Bootstrap never deletes local data.

**Change detection details**

- Bookmarks: event-driven; a full tree re-scan on service-worker start and every 6 h reconciles missed events (hash the tree; only diff if the hash changed).
- Prefs / extsettings: agent snapshots the files on `onStartup`, on profile close and every 15 min; diff against the last snapshot (SHA-256 of the canonical JSON per key, so unchanged keys cost nothing).
- History: `onVisited` + `getVisits` for the exact visit; backfill via `search` windows on first run (last 90 days only) and a daily reconciliation.
- Extensions: `management.onInstalled/onUninstalled/onEnabled/onDisabled` + `getAll` on start.

## 6. Merge rules per collection

**Generic:** for each field, the value with the highest HLC wins. A `set` with HLC later than a tombstone **resurrects** the entity (undelete) — data safety over tidiness; the resurrection is logged as a conflict of kind `resurrected`. **Convergence rules (fixed by slice 13):** a tombstone keeps the **newest** delete HLC (a delete arriving for an already deleted entity still raises it — otherwise `del@5, set@6, del@7` would end alive or deleted depending on delivery order); an entity is deleted iff its newest tombstone is newer than every field; in extension settings and browser preferences a deleted entity carries **no value** (its field HLC is kept), whatever the order in which a stale set and the tombstone arrived. With these, every collection's merge is order-independent, which is what makes `snapshot + tail ≡ full replay` (§3.7).

**Bookmarks (tree invariants, applied after field merge):**

1. *Orphaned child*: parent tombstoned but child alive → an explicit `orphan` record (§3.3); the user relocates it (nearest existing ancestor, or `other` root), restores the folder, or accepts the deletion; each choice is published as new ops. (Fixed by slice 9: no automatic re-parenting while the strategies of §7 do not exist.)
2. *Cycle* (A moved under B on device 1, B under A on device 2): the move with the lower HLC is undone by emitting a new op restoring that node's previous parent; conflict `cycle-broken`.
3. *Root mismatch*: a node whose `parentId` chain does not reach the declared `root` gets `root` recomputed from the chain.
4. *Duplicate URL siblings* created concurrently are kept (keep-both). An optional dedupe pass (same `url` + `title` in the same folder) proposes merges in the UI.
5. Folder *order* is the sort of children by `pos`; no separate order list exists, so reordering = updating `pos` of the moved node only.

**Extensions:** `installed=false` from a device with `hlc` later than any `installed=true` means "uninstall everywhere" → surfaced as a pending confirmable action on other devices (never silent). `enabled` LWW. Version/name informational (max/latest).

**Preferences:** per-path LWW over an allowlist (§3.6, fixed by slice 12). Tracked prefs are not in the v1 allowlist (writable only with MAC forging, deferred); device-bound keys are never emitted.

**Extension settings:** per-key LWW with three-way check: if base == local, take remote; if base == remote, keep local; if both changed and differ → LWW + conflict record of kind `concurrent-edit` with both values. Keys matching an exclusion pattern are never emitted. If an extension's store exceeds the size cap, the whole extension is marked `tooLarge` (reported, not synced).

**History:** union of visit sets minus deletions; `urls.title` LWW; counters derived.

## 7. Conflict representation

Conflicts never block convergence: the deterministic rule is applied immediately so all devices reach the same state, and a **conflict record** is written so a human can override.

```json
{
  "id": "1790000000500-0001-6fba69b7",
  "profileId": "…", "col": "extsettings", "entityId": "cjpalhdlnbpafiamejdnhcphjbkeiagm/local/userSettings",
  "kind": "concurrent-edit",
  "base":   { "value": {...}, "hlc": "…" },
  "local":  { "value": {...}, "hlc": "…", "device": "6fba…" },
  "remote": { "value": {...}, "hlc": "…", "device": "9c21…" },
  "resolution": { "strategy": "lww", "winner": "remote", "at": "…" },
  "status": "open"
}
```

Kinds: `concurrent-edit`, `resurrected`, `orphan-reparented`, `cycle-broken`, `uninstall-pending`, `apply-failed` (e.g. MAC forging unavailable), `too-large`.

Per-collection strategy setting (stored in `profile.json`, LWW): `lww` (default everywhere), `local-wins`, `remote-wins`, `keep-both` (bookmarks only), `manual` (apply LWW provisionally, keep the conflict `open` and badge the UI). "Resolving" a conflict in the UI simply emits a new op with the chosen value (hence a later HLC), which propagates like any other change, then deletes the conflict record. Conflict records are device-local (state.sqlite); compaction never reads, folds or closes them (§3.7).

## 8. Deletions

- Deletions are explicit `delete` ops producing tombstones; an entity merely missing from a journal is never interpreted as deleted.
- **v1: tombstones are kept indefinitely** (in journals and, once folded, in every snapshot generation — §3.7). *Possible future optimisation, not active:* drop tombstones older than a retention period at compaction; a device dormant longer than that period would then have to bootstrap from the snapshot and confirm any stale "set" for an entity absent from it before it is treated as a creation (bootstrap preview), closing the classic tombstone-expiry resurrection hole.
- Local apply of a remote deletion always goes through the backup step (§9) first. Bookmarks removed by remote ops are additionally exported to the device's local "trash" file for 30 days.
- History deletions: `url` or `range` tombstones; applied to SQLite when closed; they also remove matching visits from the device's own history shards at next compaction. Expiry heuristics are in `02-feasibility.md` §2.4.
- Extension uninstall is a deletion that requires confirmation (Chromium dialog); extension settings of an uninstalled extension are tombstoned 30 days later unless the extension is reinstalled.

## 9. Backups and rollback

Two independent layers:

**Local file backups (agent).** Before any closed-profile write, the agent copies the exact files it will touch into `<agentData>/backups/<deviceId>/<profileId>/<ISO ts>/` with a `manifest.json` (file list, SHA-256, sizes, reason, store HLC). Retention: keep the last 20 per profile and everything from the last 7 days (configurable). `synchrome restore --profile <id> --backup <ts>` (CLI and UI) restores the files while the profile is closed, after backing up the current state again. Live bookmark applies are preceded by a JSON export of the full tree into the same backup folder ("bookmarks-before.json") so a bad merge can be reverted via the API.

**Store snapshots (remote).** Every snapshot is a complete, timestamped materialised state. "Roll back profile to snapshot X" emits ops that make the current state equal to X (with fresh HLCs), so the rollback propagates to every device through the normal cycle and remains itself undoable. With GitHub, each commit is additionally a recoverable point.

Safety rules an implementation must obey: never write a profile file without a backup; never apply when liveness is uncertain; verify after write (re-parse JSON / `PRAGMA integrity_check` / reopen LevelDB) and roll back automatically on failure; keep `Bookmarks.bak` semantics intact (we never touch `.bak`).

## 10. Encryption

**Decision: optional end-to-end encryption. Default OFF for the Synchrome Cloud store (the project's own server, protected by TLS, RLS and encryption at rest) and for local folders; default ON for third-party stores (GitHub, Drive, S3, WebDAV). Chosen at store creation; cannot be toggled later without re-initialising the store.** Mandatory E2E would force every user to manage a passphrase and would make a forgotten passphrase or a password reset destructive, which conflicts with the "install, sign in, done" goal; third-party stores get it by default because the provider is not us.

Design (implemented in `packages/crypto`, usable from the first version):

- Passphrase → **Argon2id** (m = 64 MiB, t = 3, p = 1; parameters recorded in `store.json`) → 256-bit root key `K`. WASM implementation so the same code runs in Node and, if ever needed, in the extension.
- Per-purpose subkeys via HKDF-SHA256 (`K_content`, `K_names` reserved).
- Every object is `AES-256-GCM(K_content, nonce = random 96-bit, aad = object path)`; the ciphertext is prefixed with a 1-byte format version and the nonce. Binding the path in AAD stops an attacker from swapping one device's journal for another's.
- Journal chunks are compressed (zstd or deflate) before encryption. Paths (profile/device UUIDs, dates) stay in clear; they leak only cardinalities and activity timing, which is documented.
- `store.json` carries `keyCheck = HMAC-SHA256(K, "synchrome-keycheck")` so a wrong passphrase is detected before anything is read; the key never leaves the device.
- The key is derived and held by the desktop app in the OS keychain; the extension never sees it (it receives only concrete, already-decrypted applies).
- Rotation: new passphrase → new `K'`; the device re-encrypts the latest snapshot + live journals under `K'` and bumps `encryption.generation`; other devices prompt for the new passphrase on next sync.

## 11. Preference classification lists (initial)

**Superseded for v1 by the allowlist of §3.6 (slice 12): only the paths listed there are synchronised.** The lists below are the long-term candidate pool, each to be validated per browser and major (E13 method) before it can enter the allowlist. Patterns use dot paths with `*` wildcards.

**syncable (untracked):**
`bookmark_bar.show_on_all_tabs`, `bookmark_bar.visibility_state`, `intl.selected_languages`, `intl.accept_languages`, `spellcheck.dictionaries`, `spellcheck.use_spelling_service`, `devtools.preferences`, `profile.content_settings.exceptions.*` (allow-listed categories: `cookies`, `javascript`, `javascript_jit`, `images`, `popups`, `sound`, `notifications`, `geolocation`, `media_stream_camera`, `media_stream_mic`, `clipboard`, `automatic_downloads`, `autoplay`, `ar`, `vr`, `sensors`, `idle_detection`, `local_fonts`, `window_placement`, `legacy_cookie_access`, `mixed_script`, `protocol_handler`, `ssl_cert_decisions`, `insecure_private_network`, `local_network_access`, `helium_noise`, `braveShields`, `cosmeticFiltering*`), `profile.default_content_setting_values.*`, `autofill.profile_enabled`, `autofill.credit_card_enabled`, `credentials_enable_service`, `credentials_enable_autosignin`, `translate.enabled`, `translate_blocked_languages`, `translate_site_blocklist_with_time`, `translate_recent_target`, `webkit.webprefs.default_font_size`, `webkit.webprefs.default_fixed_font_size`, `webkit.webprefs.minimum_font_size`, `webkit.webprefs.fonts.*`, `partition.default_zoom_level.*`, `partition.per_host_zoom_levels.*`, `extensions.pinned_extensions`, `extensions.toolbar`, `browser.theme.*` (except `browser.theme.id` if it references an extension not installed), `browser.show_toolbar_bookmarks_button`, `browser.enable_spellchecking`, `browser.custom_chrome_frame`, `toolbar.*`, `omnibox.*` (toggles only), `ntp.*` (toggles only), `custom_handlers.*`, `protocol_handler.*`, `site_search_settings.*`, `vertical_tabs.*`, `tab_search.*`, `download.prompt_for_download`, `download.open_pdf_in_system_reader`, `plugins.always_open_pdf_externally`, `safebrowsing.enabled`, `safebrowsing.enhanced`, `alternate_error_pages.enabled`, `net.network_prediction_options`, `dns_prefetching.enabled`, `enable_do_not_track`, `privacy_sandbox.*` (toggles), `accessibility.*`, `high_efficiency.*`, `performance_tuning.*`, `helium.*` (excluding `helium.services.*`), `brave.shields.*`, `brave.new_tab_page.*`, `brave.sidebar.*`, `brave.tabs.*`.

**tracked (allow, MAC required):**
`homepage`, `homepage_is_newtabpage`, `browser.show_home_button`, `session.restore_on_startup`, `session.startup_urls`, `default_search_provider_data.template_url_data`, `pinned_tabs`.

**device (never):**
`browser.window_placement*`, `browser.app_window_placement`, `download.default_directory`, `download.directory_upgrade`, `savefile.default_directory`, `selectfile.last_directory`, `printing.*`, `profile.exit_type`, `profile.name`, `profile.avatar_index`, `profile.using_*_avatar`, `profile.created_by_version`, `profile.creation_time`, `profile.last_*`, `profile.managed*`, `profile.password_hash_data_list`, `profile.content_settings.exceptions.{site_engagement,media_engagement,permission_*,notification_*,important_site_info,*_data,client_hints,initialized_translations,ondevice_languages_downloaded,unused_site_permissions,safe_browsing_url_check_data,suspicious_*}`, `media.*`, `gcm.*`, `invalidation.*`, `signin.*`, `sync.*`, `google.services.*`, `enterprise*`, `management.*`, `optimization_guide.*`, `segmentation_platform.*`, `extensions.settings.*`, `extensions.last_chrome_version`, `extensions.alerts.*`, `extensions.install.*`, `extensions.ui.*`, `hardware_acceleration_mode*`, `web_apps.*`, `apps.*`, `sessions.*`, `saved_tab_groups.*`, `countryid_at_install`, `dual_layer_user_pref_store.*`, `prefs.*`, `protection.*`, `glic.*`, `NewTabPage.*` (volatile), `in_product_help.*`, `*_time`, `*_timestamp`, `*last_*`, `*_count`, `*_metrics*`, `*migrat*`, `brave.wallet.*`, `brave.rewards.*`, `brave.p3a.*`, `helium.services.*`.

Unlisted keys are ignored and surfaced in the UI as "unclassified" so users can opt in; the lists are versioned in the schema package and updated with Chromium releases.

## 12. Schema versioning and migrations

`store.json.schemaVersion` and `metadata/schema.json` declare the store format. An agent that sees a newer `schemaVersion` than it supports must go read-only and ask for an update. Migrations are forward-only functions over the materialised state executed by the first upgraded device, written as a new snapshot with `schemaVersion+1`; older devices stop writing (they see the version bump) until upgraded. Journal op format changes are additive (unknown fields ignored, unknown collections skipped with a warning).
