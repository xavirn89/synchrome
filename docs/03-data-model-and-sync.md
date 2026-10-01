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
- A Chromium profile can be mapped to at most one Synchrome profile; a Synchrome profile can be mapped to at most one local profile per device.

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
| `extensions` | Web Store id | `installed` (bool), `enabled` (bool), `name`, `version` (informational), `updateUrl`, `pinned` (from `extensions.pinned_extensions`) | `chrome.management` + prefs | live (enable/disable), descriptor/guided (install) |
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
compact        (occasionally, any device) write snapshot, prune journals acknowledged by all active devices
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

**Generic:** for each field, the value with the highest HLC wins. A `set` with HLC later than a tombstone **resurrects** the entity (undelete) — data safety over tidiness; the resurrection is logged as a conflict of kind `resurrected`.

**Bookmarks (tree invariants, applied after field merge):**

1. *Orphaned child*: parent tombstoned but child alive → the child's parent is re-pointed to the deleted folder's last known parent (or `other` root) with a new op; recorded as conflict `orphan-reparented`. (Alternative, folder resurrection, is offered as a strategy option; default is reparent, because deleting a folder is usually intentional.)
2. *Cycle* (A moved under B on device 1, B under A on device 2): the move with the lower HLC is undone by emitting a new op restoring that node's previous parent; conflict `cycle-broken`.
3. *Root mismatch*: a node whose `parentId` chain does not reach the declared `root` gets `root` recomputed from the chain.
4. *Duplicate URL siblings* created concurrently are kept (keep-both). An optional dedupe pass (same `url` + `title` in the same folder) proposes merges in the UI.
5. Folder *order* is the sort of children by `pos`; no separate order list exists, so reordering = updating `pos` of the moved node only.

**Extensions:** `installed=false` from a device with `hlc` later than any `installed=true` means "uninstall everywhere" → surfaced as a pending confirmable action on other devices (never silent). `enabled` LWW. Version/name informational (max/latest).

**Preferences:** per-key LWW. Keys in the `device` list are never emitted. Keys in the `tracked` list are emitted but applied only where the device can forge MACs; otherwise they stay "pending manual".

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

Per-collection strategy setting (stored in `profile.json`, LWW): `lww` (default everywhere), `local-wins`, `remote-wins`, `keep-both` (bookmarks only), `manual` (apply LWW provisionally, keep the conflict `open` and badge the UI). "Resolving" a conflict in the UI simply emits a new op with the chosen value (hence a later HLC), which propagates like any other change, then deletes the conflict record. Conflict records older than 90 days are auto-closed at compaction.

## 8. Deletions

- Deletions are explicit `delete` ops producing tombstones; an entity merely missing from a journal is never interpreted as deleted.
- Tombstones are retained for `tombstoneRetentionDays` (default 90) after which compaction drops them. A device dormant longer than the retention cannot resurrect by accident: on return it bootstraps from the snapshot (§5) and its stale "set" ops for entities absent from the snapshot are treated as *creations* only if the user confirms in the bootstrap preview; otherwise they are discarded. This closes the classic tombstone-expiry resurrection hole at the cost of one prompt in a rare case.
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

Stored as data in `packages/schemas/prefs.json`; the lists below seed it. Patterns use dot paths with `*` wildcards.

**syncable (untracked):**
`bookmark_bar.show_on_all_tabs`, `bookmark_bar.visibility_state`, `intl.selected_languages`, `intl.accept_languages`, `spellcheck.dictionaries`, `spellcheck.use_spelling_service`, `devtools.preferences`, `profile.content_settings.exceptions.*` (allow-listed categories: `cookies`, `javascript`, `javascript_jit`, `images`, `popups`, `sound`, `notifications`, `geolocation`, `media_stream_camera`, `media_stream_mic`, `clipboard`, `automatic_downloads`, `autoplay`, `ar`, `vr`, `sensors`, `idle_detection`, `local_fonts`, `window_placement`, `legacy_cookie_access`, `mixed_script`, `protocol_handler`, `ssl_cert_decisions`, `insecure_private_network`, `local_network_access`, `helium_noise`, `braveShields`, `cosmeticFiltering*`), `profile.default_content_setting_values.*`, `autofill.profile_enabled`, `autofill.credit_card_enabled`, `credentials_enable_service`, `credentials_enable_autosignin`, `translate.enabled`, `translate_blocked_languages`, `translate_site_blocklist_with_time`, `translate_recent_target`, `webkit.webprefs.default_font_size`, `webkit.webprefs.default_fixed_font_size`, `webkit.webprefs.minimum_font_size`, `webkit.webprefs.fonts.*`, `partition.default_zoom_level.*`, `partition.per_host_zoom_levels.*`, `extensions.pinned_extensions`, `extensions.toolbar`, `browser.theme.*` (except `browser.theme.id` if it references an extension not installed), `browser.show_toolbar_bookmarks_button`, `browser.enable_spellchecking`, `browser.custom_chrome_frame`, `toolbar.*`, `omnibox.*` (toggles only), `ntp.*` (toggles only), `custom_handlers.*`, `protocol_handler.*`, `site_search_settings.*`, `vertical_tabs.*`, `tab_search.*`, `download.prompt_for_download`, `download.open_pdf_in_system_reader`, `plugins.always_open_pdf_externally`, `safebrowsing.enabled`, `safebrowsing.enhanced`, `alternate_error_pages.enabled`, `net.network_prediction_options`, `dns_prefetching.enabled`, `enable_do_not_track`, `privacy_sandbox.*` (toggles), `accessibility.*`, `high_efficiency.*`, `performance_tuning.*`, `helium.*` (excluding `helium.services.*`), `brave.shields.*`, `brave.new_tab_page.*`, `brave.sidebar.*`, `brave.tabs.*`.

**tracked (allow, MAC required):**
`homepage`, `homepage_is_newtabpage`, `browser.show_home_button`, `session.restore_on_startup`, `session.startup_urls`, `default_search_provider_data.template_url_data`, `pinned_tabs`.

**device (never):**
`browser.window_placement*`, `browser.app_window_placement`, `download.default_directory`, `download.directory_upgrade`, `savefile.default_directory`, `selectfile.last_directory`, `printing.*`, `profile.exit_type`, `profile.name`, `profile.avatar_index`, `profile.using_*_avatar`, `profile.created_by_version`, `profile.creation_time`, `profile.last_*`, `profile.managed*`, `profile.password_hash_data_list`, `profile.content_settings.exceptions.{site_engagement,media_engagement,permission_*,notification_*,important_site_info,*_data,client_hints,initialized_translations,ondevice_languages_downloaded,unused_site_permissions,safe_browsing_url_check_data,suspicious_*}`, `media.*`, `gcm.*`, `invalidation.*`, `signin.*`, `sync.*`, `google.services.*`, `enterprise*`, `management.*`, `optimization_guide.*`, `segmentation_platform.*`, `extensions.settings.*`, `extensions.last_chrome_version`, `extensions.alerts.*`, `extensions.install.*`, `extensions.ui.*`, `hardware_acceleration_mode*`, `web_apps.*`, `apps.*`, `sessions.*`, `saved_tab_groups.*`, `countryid_at_install`, `dual_layer_user_pref_store.*`, `prefs.*`, `protection.*`, `glic.*`, `NewTabPage.*` (volatile), `in_product_help.*`, `*_time`, `*_timestamp`, `*last_*`, `*_count`, `*_metrics*`, `*migrat*`, `brave.wallet.*`, `brave.rewards.*`, `brave.p3a.*`, `helium.services.*`.

Unlisted keys are ignored and surfaced in the UI as "unclassified" so users can opt in; the lists are versioned in the schema package and updated with Chromium releases.

## 12. Schema versioning and migrations

`store.json.schemaVersion` and `metadata/schema.json` declare the store format. An agent that sees a newer `schemaVersion` than it supports must go read-only and ask for an update. Migrations are forward-only functions over the materialised state executed by the first upgraded device, written as a new snapshot with `schemaVersion+1`; older devices stop writing (they see the version bump) until upgraded. Journal op format changes are additive (unknown fields ignored, unknown collections skipped with a warning).
