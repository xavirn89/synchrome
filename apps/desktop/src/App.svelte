<script>
  import { allowsIconMotion } from "./lib/iconography.js";
  import { invoke, previewEnabled, previewScenario } from "./lib/api.js";
  import { onMount } from "svelte";
  import AuthView from "./components/AuthView.svelte";
  import Dialog from "./components/Dialog.svelte";
  import Icon from "./components/Icon.svelte";
  import { localKey, validateName, connectionState, profileState, groupConnections, disconnectAndDelete, sameDeviceConflict, connectionConflictCount, cloudState, attentionSummary } from "./lib/product.js";
  import "./styles.css";

  // ---- state ----
  let version = $state("");
  let browsers = $state([]);
  let cloud = $state(null);
  let account = $state(null);
  let device = $state(null);
  let devices = $state([]);
  let profiles = $state([]);
  let mappings = $state([]);

  let loadingBrowsers = $state(true);
  let loadingCloud = $state(true);
  let cloudDataBusy = $state(false);
  let error = $state(null);
  let cloudDataError = $state(null);
  let actionError = $state(null);
  let busyKey = $state(null); // which row/action is in flight

  // Authentication form values stay transient inside AuthView.
  let authBusy = $state(false);
  let authError = $state(null);
  // install location (the native messaging host needs a stable path) + bundled extension
  let location = $state(null);
  let extFolder = $state(null);
  let extFolderError = $state(null);
  let deviceConfirm = $state(null); // "<profile>:<device>" waiting for confirmation


  const ACCESS = {
    accessible: { label: "Accessible", cls: "ok" },
    requires_full_disk_access: { label: "Requires Full Disk Access", cls: "warn" },
    browser_running: { label: "Browser running", cls: "info" },
    unknown: { label: "Unknown", cls: "muted" },
  };
  const MAP_STATUS = {
    pending_local_write: { label: "Pending local write", cls: "muted" },
    token_written: { label: "Token written", cls: "info" },
    extension_confirmed: { label: "Extension confirmed", cls: "ok" },
    error: { label: "Error", cls: "bad" },
  };
  const BM_STATUS = {
    not_synced: { label: "Not synced", cls: "muted" },
    initial_upload: { label: "Initial upload", cls: "info" },
    up_to_date: { label: "Up to date", cls: "ok" },
    pending_changes: { label: "Pending changes", cls: "info" },
    offline: { label: "Offline", cls: "warn" },
    error: { label: "Error", cls: "bad" },
  };
  const DL_STATUS = {
    up_to_date: { label: "Up to date", cls: "ok" },
    remote_changes: { label: "Remote changes", cls: "info" },
    applying: { label: "Applying", cls: "info" },
    conflict: { label: "Conflict", cls: "bad" },
    offline: { label: "Offline", cls: "warn" },
    error: { label: "Error", cls: "bad" },
  };
  const when = (t) => (t ? new Date(t * 1000).toLocaleString() : "never");

  // ---- conflicts (decided here, in Rust; the extension only receives the resulting apply plan) ----
  const C_STATUS = {
    unresolved: { label: "Conflict", cls: "bad" },
    resolving: { label: "Applying", cls: "info" },
    resolved: { label: "Resolved", cls: "ok" },
    superseded: { label: "Superseded", cls: "muted" },
  };
  const C_SUBTYPE = {
    edit_edit: "Changed here and on another device",
    edit_delete: "Changed here, deleted on another device",
    delete_edit: "Deleted here, changed on another device",
    delete_delete: "Deleted on both devices",
    delete_not_empty: "Deleted on another device, but it holds items that only exist here",
    apply_error: "The browser refused a change",
    orphan: "Added on another device into a folder deleted here",
  };
  const RES_DONE = {
    keep_local: "kept this device's version",
    keep_remote: "took the other device's version",
    merge: "merged both changes",
    retry: "retried",
    relocate: "moved to a valid folder",
  };
  // ---- extensions (inventory + enabled state) ----
  const X_STATE = {
    synced: { label: "Synced", cls: "ok" },
    missing: { label: "Missing", cls: "info" },
    removed_elsewhere: { label: "Removed on another device", cls: "warn" },
    applying: { label: "Applying", cls: "info" },
    user_action_required: { label: "User action required", cls: "warn" },
    protected: { label: "Protected", cls: "muted" },
    conflict: { label: "Conflict", cls: "bad" },
    apply_failed: { label: "Apply failed", cls: "bad" },
    install_opened: { label: "Install opened in the browser", cls: "info" },
  };
  const X_AVAIL = { available: "Install available", unavailable: "Unavailable (not from a store)", incompatible: "Incompatible with this browser", unknown: "Unknown source" };
  const X_ACTION = { install: "Install", enable: "Enable", disable: "Disable", remove: "Remove here", keep: "Keep here", retry: "Retry" };
  const S_STATE = {
    off: { label: "Off", cls: "muted" },
    waiting_for_browser_close: { label: "Waiting for browser to close", cls: "info" },
    syncing: { label: "Syncing", cls: "info" },
    synced: { label: "Synced", cls: "ok" },
    version_mismatch: { label: "Version mismatch", cls: "warn" },
    conflict: { label: "Conflict", cls: "bad" },
    error: { label: "Error", cls: "bad" },
  };
  const S_MODE = { off: "Off", local: "Local only", local_sync: "Local + Sync" };
  let setList = $state({});  // mapping id -> { ext_id -> SettingsView }
  let setMode = $state({});  // "<mapping>:<ext>" -> chosen mode (before confirming)
  async function loadSettings(m) {
    const generation=accountGeneration;
    try {
      const l = await invoke("list_extension_settings", { mappingId: m.id });
      if(generation!==accountGeneration)return;
      setList = { ...setList, [m.id]: Object.fromEntries(l.map((v) => [v.ext_id, v])) };
    } catch (e) { actionError = String(e); }
  }
  async function applyMode(m, x, mode) {
    if(busyKey)return;
    busyKey = `set:${x.ext_id}`; actionError = null;
    try {
      if(dataLoadPromise)await dataLoadPromise;
      await invoke("set_extension_settings_mode", { mappingId: m.id, extId: x.ext_id, mode });
      extMsg = { ...extMsg, [m.id]: `“${x.name}”: settings sync ${S_MODE[mode]}.` };
      await loadSettings(m);
    } catch (e) { actionError = String(e); }
    finally { busyKey = null; }
  }
  async function settingsNow(m, x) {
    if(busyKey)return;
    busyKey = `set:${x.ext_id}`; actionError = null;
    try {
      if(dataLoadPromise)await dataLoadPromise;
      const r = await invoke("extension_settings_sync_now", { mappingId: m.id });
      extMsg = { ...extMsg, [m.id]: r.browser_running ? "The browser is open: extension settings are read and written when it is closed." : `Settings synced (${r.captured_ops} changes sent, ${r.keys_written} keys written).` };
      await loadSettings(m);
    } catch (e) { actionError = String(e); }
    finally { busyKey = null; }
  }
  // Browser preferences (allowlist), per mapping of this device
  const P_STATE = { ...S_STATE, unsupported: { label: "Unsupported", cls: "muted" } };
  const P_MODE = { off: "Off", recommended: "Sync recommended preferences" };
  let prefList = $state({});  // mapping id -> PrefsView
  let prefMode = $state({});  // mapping id -> chosen mode (before confirming)
  let prefMsg = $state({});
  async function loadPrefs(m) {
    const generation=accountGeneration;
    try { const result=await invoke("list_browser_preferences", { mappingId: m.id });if(generation===accountGeneration)prefList = { ...prefList, [m.id]:result }; }
    catch (e) { /* no local profile state yet for this mapping */ }
  }
  async function applyPrefsMode(m, mode) {
    if(busyKey)return;
    busyKey = `prefs:${m.id}`; actionError = null;
    try {
      if(dataLoadPromise)await dataLoadPromise;
      await invoke("set_browser_preferences_mode", { mappingId: m.id, mode });
      prefMode = { ...prefMode, [m.id]: undefined };
      prefMsg = { ...prefMsg, [m.id]: `Browser preferences: ${P_MODE[mode]}.` };
      await loadPrefs(m);
    } catch (e) { actionError = String(e); }
    finally { busyKey = null; }
  }
  async function prefsNow(m) {
    if(busyKey)return;
    busyKey = `prefs:${m.id}`; actionError = null;
    try {
      if(dataLoadPromise)await dataLoadPromise;
      const r = await invoke("browser_preferences_sync_now", { mappingId: m.id });
      prefMsg = { ...prefMsg, [m.id]: r.browser_running ? "The browser is open: preferences are read and written when it is closed." : `Preferences synced (${r.captured_ops} changes sent, ${r.paths_written} written).` };
      await loadPrefs(m);
    } catch (e) { actionError = String(e); }
    finally { busyKey = null; }
  }
  // Realtime + compaction diagnostics (slice 13), per mapping of this device
  let diag = $state({});
  async function loadDiag(m) {
    const generation=accountGeneration;
    try { const result=await invoke("sync_diagnostics", { mappingId: m.id });if(generation===accountGeneration)diag = { ...diag, [m.id]:result }; }
    catch (e) { /* no local state for this mapping yet */ }
  }
  // a server timestamp as local epoch seconds, corrected by the server/local clock offset
  const serverSecs = (at, now) => { const a = Date.parse(at ?? ""), n = Date.parse(now ?? ""); return Number.isNaN(a) ? null : Math.round((a - (Number.isNaN(n) ? 0 : n - Date.now())) / 1000); };
  const ago = (t) => { if (!t) return "never"; const s = Math.max(0, Math.round(Date.now() / 1000 - t)); return s < 90 ? `${s} s ago` : s < 5400 ? `${Math.round(s / 60)} min ago` : `${Math.round(s / 3600)} h ago`; };
  let extOpen = $state({});
  let extList = $state({});
  let extConfirm = $state(null);
  let extMsg = $state({});
  async function loadExtensions(m) {
    const generation=accountGeneration;
    try { const result=await invoke("list_extensions", { mappingId: m.id });if(generation===accountGeneration)extList = { ...extList, [m.id]:result }; }
    catch (e) { actionError = String(e); }
  }
  async function toggleExtensions(m) {
    const open = !extOpen[m.id];
    extOpen = { ...extOpen, [m.id]: open };
    if (open) { await loadExtensions(m); await loadSettings(m); }
  }
  async function extAct(m, x, action) {
    if(busyKey)return;
    const key = `${x.ext_id}:${action}`;
    if (action === "remove" && extConfirm !== key) { extConfirm = key; return; }
    extConfirm = null; busyKey = `ext:${x.ext_id}`; actionError = null;
    try {
      if(dataLoadPromise)await dataLoadPromise;
      await invoke("extension_action", { mappingId: m.id, extId: x.ext_id, action });
      extMsg = { ...extMsg, [m.id]: `“${x.name}”: ${X_ACTION[action]} — ${action === "install" ? "the store page opens in the browser on the next sync" : "saved; the browser is updated on the next sync"}.` };
      await loadExtensions(m);
      mappings = await invoke("list_mappings");
    } catch (e) { actionError = String(e); }
    finally { busyKey = null; }
  }
  const onOff = (v) => (v === true ? "Enabled" : v === false ? "Disabled" : "—");

  let conflictsOpen = $state({});   // mapping id -> panel open
  let conflictList = $state({});    // mapping id -> ConflictView[]
  let confirmKey = $state(null);    // "<conflict>:<resolution>" waiting for confirmation
  let resolvedMsg = $state({});     // conflict id -> confirmation text
  let showHistory = $state({});
  let lastResolved = $state({});    // mapping id -> latest confirmation (stays visible after the card closes)

  function optLabel(c, r) {
    const n = c.local_items;
    switch (r) {
      case "keep_local":
        return { delete_edit: "Keep it deleted", orphan: "Accept the deletion", delete_not_empty: "Keep the folder" }[c.subtype] ?? "Keep this device's version";
      case "keep_remote":
        return {
          edit_delete: "Delete it (as on the other device)",
          delete_edit: "Restore the other device's version",
          delete_not_empty: `Delete the folder and its ${n} item${n === 1 ? "" : "s"}`,
          orphan: "Restore the deleted folder",
        }[c.subtype] ?? "Take the other device's version";
      case "merge": {
        const mine = c.local.changed.join(" + "), theirs = c.remote.changed.join(" + ");
        return `Merge: ${mine} from here, ${theirs} from the other device`;
      }
      case "retry": return "Retry in the browser";
      case "relocate":
        return c.subtype === "orphan" ? "Keep it, in the nearest existing folder" : `Move the ${n} item${n === 1 ? "" : "s"} out, then delete the folder`;
    }
    return r;
  }
  const deviceName = (id) => (id ? devices.find((d) => d.device_id === id)?.name ?? "another device" : "another device");
  const folderText = (f) => (f ? f.join(" › ") : "—");
  /// Rows of the Local vs Remote comparison. A remote value absent means "unchanged there".
  function compareRows(c) {
    const has = (side, g) => side.changed.includes(g);
    if (c.item_kind === "setting" || c.item_kind === "preference") {
      const rows = [
        { k: "Exists", l: c.local.exists ? "yes" : "deleted", r: c.remote.exists ? "yes" : "deleted" },
        { k: "Value", l: c.local.value ?? "—", r: c.remote.exists ? (c.remote.value ?? "—") : "—" },
      ];
      if (c.base) for (const row of rows) row.b = row.k === "Exists" ? (c.base.exists ? "yes" : "deleted") : (c.base.value ?? "");
      return rows.map((x) => ({ ...x, diff: x.l !== x.r }));
    }
    if (c.item_kind === "extension") {
      const rows = [
        { k: "Installed", l: c.local.exists ? "yes" : "removed", r: c.remote.exists ? "yes" : "removed" },
        { k: "Enabled", l: onOff(c.local.enabled), r: c.remote.exists ? (has(c.remote, "enabled") ? onOff(c.remote.enabled) : "unchanged") : "—" },
      ];
      if (c.base) for (const row of rows) row.b = row.k === "Installed" ? (c.base.exists ? "yes" : "removed") : onOff(c.base.enabled);
      return rows.map((x) => ({ ...x, diff: x.r !== "unchanged" && x.l !== x.r }));
    }
    const rv = (g, v) => (c.remote.exists === false && g !== "existence" ? "—" : has(c.remote, g) ? v : "unchanged");
    const rows = [
      { k: "Exists", l: c.local.exists ? "yes" : "deleted", r: c.remote.exists ? "yes" : "deleted" },
      { k: "Title", l: c.local.title ?? "—", r: rv("title", c.remote.title ?? "—") },
    ];
    if (c.item_kind === "url") rows.push({ k: "URL", l: c.local.url ?? "—", r: rv("url", c.remote.url ?? "—") });
    rows.push({ k: "Folder", l: folderText(c.local.folder), r: rv("location", folderText(c.remote.folder)) });
    if (has(c.local, "location") || has(c.remote, "location")) {
      const pos = (s) => (s.position ? `${s.position} of ${s.siblings}` : "—");
      rows.push({ k: "Position", l: pos(c.local), r: rv("location", pos(c.remote)) });
    }
    if (c.base) {
      const b = c.base;
      for (const row of rows) {
        row.b = { Title: b.title, URL: b.url, Folder: b.folder ? folderText(b.folder) : null, Exists: b.exists ? "yes" : "deleted" }[row.k] ?? null;
      }
    }
    return rows.map((x) => ({ ...x, diff: x.r !== "unchanged" && x.l !== x.r }));
  }
  async function loadConflicts(m) {
    const generation=accountGeneration;
    try {
      const result=await invoke("list_conflicts", { mappingId: m.id, includeClosed: true });
      if(generation===accountGeneration)conflictList = { ...conflictList, [m.id]:result };
    } catch (e) { actionError = String(e); }
  }
  async function toggleConflicts(m) {
    const open = !conflictsOpen[m.id];
    conflictsOpen = { ...conflictsOpen, [m.id]: open };
    if (open) await loadConflicts(m);
  }
  function choose(m, c, o) {
    const key = `${c.conflict_id}:${o.resolution}`;
    if (o.destructive && confirmKey !== key) { confirmKey = key; return; }
    return resolveOne(m, c, o.resolution);
  }
  async function resolveOne(m, c, resolution) {
    if(busyKey)return;
    confirmKey = null; actionError = null; busyKey = `resolve:${c.conflict_id}`;
    try {
      if(dataLoadPromise)await dataLoadPromise;
      const [r] = await invoke("resolve_conflicts", { mappingId: m.id, items: [{ conflict_id: c.conflict_id, resolution }] });
      const what = RES_DONE[resolution] ?? resolution;
      const msg = r.outcome === "resolved" ? `Resolved — ${what}.`
        : r.outcome === "resolving" ? `Decision saved — ${what}; the browser is updated on the next sync.`
        : r.outcome === "already_resolved" ? "Already resolved."
        : r.outcome === "closed" ? `No longer open (${r.status}).`
        : `Could not resolve: ${humanize(r.error).text}`;
      resolvedMsg = { ...resolvedMsg, [c.conflict_id]: msg };
      lastResolved = { ...lastResolved, [m.id]: { ok: r.outcome !== "error", text: `“${c.name}”: ${msg}` } };
      await loadConflicts(m);
      mappings = await invoke("list_mappings");
    } catch (e) { actionError = String(e); }
    finally { busyKey = null; }
  }
  let connectorMsg = $state({});

  // ---- loaders ----
  async function loadBrowsers() {
    loadingBrowsers = true;
    try { browsers = await invoke("list_browsers"); } catch (e) { error = String(e); }
    finally { loadingBrowsers = false; }
  }
  async function loadCloud() {
    loadingCloud = true;
    try { cloud = await invoke("cloud_status"); } catch (e) { error = String(e); }
    finally { loadingCloud = false; }
  }
  async function loadAccount() {
    const generation=accountGeneration;
    try { const result=await invoke("account_status");if(generation===accountGeneration)account = result; } catch (e) { if(generation===accountGeneration)error = String(e); }
  }
  async function loadDevice() {
    try { device = await invoke("device_identity"); } catch { device = null; }
  }
  async function loadLocation() {
    try { location = await invoke("install_location"); } catch { location = null; }
    try { extFolder = await invoke("extension_folder"); extFolderError = null; } catch (e) { extFolder = null; extFolderError = String(e); }
  }
  async function revealExtension() {
    try { await invoke("reveal_extension_folder"); } catch (e) { actionError = String(e); }
  }
  async function openFdaSettings() {
    try { await invoke("open_full_disk_access_settings"); } catch (e) { actionError = String(e); }
  }
  async function deviceAction(m, d, action) {
    if(busyKey)return;
    const key = `${m.profile_id}:${d.device_id}:${action}`;
    if (action === "retire" && deviceConfirm !== key) { deviceConfirm = key; return; }
    deviceConfirm = null; busyKey = `dev:${d.device_id}`; actionError = null;
    try {
      if(dataLoadPromise)await dataLoadPromise;
      await invoke(action === "retire" ? "retire_device" : "reactivate_device", { profileId: m.profile_id, deviceId: d.device_id });
      await loadDiag(m);
    } catch (e) { actionError = String(e); }
    finally { busyKey = null; }
  }
  /// Register this device, then fetch devices, profiles and mappings. Only when signed in.
  let dataLoadPromise = null;
  let accountGeneration = 0;
  async function loadCloudData() {
    if (!account?.signed_in) { devices = []; profiles = []; mappings = []; return; }
    if (dataLoadPromise) return dataLoadPromise;
    const generation=accountGeneration;
    const userId=account.user_id;
    cloudDataBusy = true;
    cloudDataError = null;
    dataLoadPromise=(async()=>{
    try {
      await invoke("register_device");
      const result = await Promise.all([
        invoke("list_devices"), invoke("list_profiles"), invoke("list_mappings"),
      ]);
      if(generation!==accountGeneration||account?.user_id!==userId||!account?.signed_in)return;
      [devices,profiles,mappings]=result;
      // open panels show fresh data after every reload
      for (const m of mappings) {
        if (conflictsOpen[m.id]) await loadConflicts(m);
        if (extOpen[m.id]) { await loadExtensions(m); await loadSettings(m); }
        if (generation!==accountGeneration)break;
        if (m.is_this_device && m.display_status === "extension_confirmed") { await loadPrefs(m); await loadDiag(m); }
      }
    } catch (e) {
      if(generation===accountGeneration)cloudDataError = String(e);
    } finally {
      cloudDataBusy = false;
      dataLoadPromise=null;
    }
    })();
    return dataLoadPromise;
  }
  async function loadAll() {
    error = null;
    version = await invoke("app_version").catch(() => "");
    await Promise.all([loadBrowsers(), loadCloud(), loadAccount(), loadDevice(), loadLocation()]);
    await loadCloudData();
  }


  // ---- auth ----
  /// Plain-language version of an internal error; the raw text stays available as detail.
  function humanize(e) {
    const raw = String(e ?? "");
    const l = raw.toLowerCase();
    // messages the app already writes for people: shown as they are
    const FRIENDLY = ["Drag it to Applications", "Move it to Applications", "was not detected on this device",
      "Unmap it on every device first", "Account created", "could not map the new profile", "is not bundled with this build"];
    const pick = () => {
      if (!raw) return null;
      if (FRIENDLY.some((f) => raw.includes(f))) return raw;
      if (l.includes("not signed in")) return "Sign in to continue.";
      if (l.includes("session expired") || l.includes("sessioninvalid") || l.includes("sign in again")) return "Your session expired. Sign in again.";
      if (l.includes("invalid email or password") || l.includes("invalid login") || l.includes("invalid credentials") || l.includes("invalidcredentials")) return "Wrong e-mail or password.";
      if (l.includes("operation not permitted") || l.includes("eperm") || l.includes("full disk access") || l.includes("permission denied"))
        return "macOS is blocking access to this browser's files. Grant Synchrome Full Disk Access (System Settings → Privacy & Security), then retry.";
      if (l.includes("browser is open") || l.includes("browser running") || l.includes("browser is running") || l.includes("singletonlock") || l.includes("waiting_for_browser_close"))
        return "The browser is open. Close it and retry; Synchrome writes these files only while the browser is closed.";
      if (l.includes("version mismatch") || l.includes("version_mismatch")) return "The browser versions differ. Update them to a compatible version, then open the browser and retry.";
      if (l.includes("cleanup is incomplete")) return "The Synchrome profile was deleted, but some cloud data could not be cleaned up. Your local browser profiles and bookmarks are safe. See diagnostics before retrying cleanup.";
      if (l.includes("the browsers are disconnected, but") || l.includes("the profile was not deleted")) return raw.replace(/^Error: /, "");
      if (l.includes("disconnect")) return "Some connections could not be removed. Review the remaining connections and retry. Your local browser data is unchanged.";
      if (l.includes("token mismatch")) return "This browser profile holds a link to a different Synchrome profile. Unmap it here, or reset the link in the browser's extension.";
      if (l.includes("no token") || l.includes("token missing") || l.includes("not confirmed") || l.includes("unknown token")) return "The browser has not confirmed the link yet. Load the companion extension and open it once, or use “Retry local write” with the browser closed.";
      if (l.includes("mapping deleted") || l.includes("mapping not found") || l.includes("no longer mapped")) return "This link was removed. Map the profile again if you want to keep syncing it.";
      if (l.includes("profile not found") || l.includes("profile was deleted") || l.includes("profileinuse")) return "The Synchrome profile no longer exists or is still in use on another device.";
      if (l.includes("another device") || l.includes("wrong device")) return "This link belongs to another of your devices.";
      if (l.includes("unsupported")) return "This browser build is not supported for this feature.";
      if (l.includes("user action required") || l.includes("user_action_required") || l.includes("gesture")) return "The browser needs you to confirm this change itself (open the extension's popup).";
      if (l.includes("snapshot integrity") || l.includes("snapshot_invalid")) return "A downloaded snapshot failed its integrity check; nothing was changed. Synchrome retries with the previous one.";
      if (l.includes("offline") || l.includes("network") || l.includes("unreachable") || l.includes("timed out") || l.includes("dns") || l.includes("connection")) return "Synchrome Cloud is not reachable right now. Your changes are kept and sent when it is back.";
      if (l.includes("conflict")) return "This change conflicts with one made on another device. Review it below.";
      if (l.includes("not configured") || l.includes("missing required configuration")) return "Synchrome Cloud is not configured in this build.";
      return "Something went wrong.";
    };
    const text = pick();
    return { text, detail: text && raw && !text.includes(raw) ? raw : null };
  }
  async function signOut() {
    if(busyKey)return;
    authBusy = true; authError = null; accountGeneration++;
    try {
      account = await invoke("sign_out");
      devices = []; profiles = []; mappings = [];
      extList={};setList={};prefList={};diag={};conflictList={};extOpen={};conflictsOpen={};detailsOpen={};toast="";
    } catch (e) { authError = String(e); }
    finally { authBusy = false; }
  }
  async function refresh() {
    if(busyKey)return;
    authBusy = true; authError = null;
    try { account = await invoke("refresh_session"); } catch (e) { authError = String(e); }
    finally { authBusy = false; }
  }

  // ---- generic mutation wrapper: run, then reload cloud data ----
  async function act(key, fn) {
    if(busyKey)return false;
    actionError = null; busyKey = key;
    try { if(dataLoadPromise)await dataLoadPromise; await fn(); await loadCloudData(); return true; }
    catch (e) { actionError = String(e); await loadCloudData(); return false; }
    finally { busyKey = null; }
  }

  // Browser connections reuse the existing native services.
  const mappingFor = (b, p) =>
    mappings.find((m) => m.is_this_device && m.browser_kind === b.kind && m.browser_profile_dir === p.dir_name);
  const mappingsOf = (profileId) => mappings.filter((m) => m.profile_id === profileId);
  const unmap = (m) => requestDisconnect(m);
  const retryWrite = (m) => act(`write:${m.id}`, () => invoke("write_mapping_token_local", { mappingId: m.id }));
  const checkExtension = (m) => act(`check:${m.id}`, async () => {});
  const uploadNow = (m) => act(`upload:${m.id}`, () => invoke("publish_bookmarks", { mappingId: m.id }));
  async function installConnector(b) {
    if(busyKey||!b)return;busyKey=`connector:${b.kind}`;
    connectorMsg = { ...connectorMsg, [b.kind]: "Installing…" };
    try {
      await invoke("install_native_host", { browserKind: b.kind, userDataDir: b.user_data_dir });
      connectorMsg = { ...connectorMsg, [b.kind]: "Connector installed. Restart the browser." };
    } catch (e) {
      connectorMsg = { ...connectorMsg, [b.kind]: `Connector install failed: ${humanize(e).text}` };
      actionError=String(e);
    }finally{busyKey=null;}
  }

  // ---- derived ----
  const signedIn = $derived(!!account?.signed_in);
  const needsFda = $derived(browsers.some((b) => b.access === "requires_full_disk_access"));
  const totalProfiles = $derived(browsers.reduce((n, b) => n + b.profiles.length, 0));
  // this device's mappings whose local profile is not among the detected ones
  const unmatched = $derived(mappings.filter((m) => m.is_this_device
    && !browsers.some((b) => b.kind === m.browser_kind && b.profiles.some((p) => p.dir_name === m.browser_profile_dir))));
  const myMappings = $derived(mappings.filter((m) => m.is_this_device));
  const confirmedMappings = $derived(myMappings.filter((m) => m.display_status === "extension_confirmed"));
  function probeBadge(p) {
    if (!p) return { label: "—", cls: "muted" };
    if (p.outcome === "unreachable") return { label: "unreachable", cls: "bad" };
    return { label: `${p.status ?? "?"} · ${p.latency_ms} ms`, cls: p.outcome === "ok" ? "ok" : "warn" };
  }
  const fmtExpiry = (ts) => (ts ? new Date(ts * 1000).toLocaleString() : "");
  // Product shell: visible state comes from native data, never guessed browser activity.
  let route = $state('overview');
  let selectedId = $state(null);
  let profileSearch = $state('');
  let modal = $state(null);
  let modalProfile = $state(null);
  let modalMapping = $state(null);
  let profileMethod = $state('choose');
  let selectedLocal = $state('');
  let selectedTarget = $state('');
  let formName = $state('');
  let formError = $state('');
  let deleteName = $state('');
  let toast = $state('');
  let booting = $state(true);
  let onboarding = $state(false);
  let onboardingDismissed = $state(false);
  let appearance = $state('system');
  let systemDark = $state(false);
  let detailsOpen = $state({});
  const nav = [{id:'overview',name:'Overview',icon:'overview'},{id:'profiles',name:'Profiles',icon:'profiles'},{id:'devices',name:'Devices',icon:'devices'},{id:'activity',name:'Activity',icon:'activity'},{id:'settings',name:'Settings',icon:'settings'}];
  const selectedProfile = $derived(profiles.find(p=>p.id===selectedId) ?? profiles[0] ?? null);
  const selectedConnections = $derived(selectedProfile ? mappingsOf(selectedProfile.id) : []);
  const visibleProfiles = $derived(profiles.filter(p=>p.name.toLocaleLowerCase().includes(profileSearch.toLocaleLowerCase())));
  const attention = $derived(attentionSummary(myMappings.map(withDatasets),cloud?.reachable!==false));
  const conflictCount = $derived(myMappings.reduce((n,m)=>n+connectionConflictCount(withDatasets(m)),0));
  const localOptions = $derived(browsers.flatMap(b=>b.profiles.map(p=>({key:localKey(b,p),b,p,mapping:mappingFor(b,p)}))));
  const pickedLocal = $derived(localOptions.find(o=>o.key===selectedLocal));
  const setupMapping = $derived(modalMapping ? mappings.find(m=>m.id===modalMapping.id) : myMappings.find(m=>m.display_status!=='extension_confirmed'||!m.bookmarks?.last_upload_at));
  const setupBrowser = $derived(setupMapping ? browsers.find(b=>b.kind===setupMapping.browser_kind) : null);
  const setupComplete = $derived(!!setupMapping?.bookmarks && (!!setupMapping.bookmarks.last_upload_at || setupMapping.bookmarks.state==='up_to_date'));
  let appearanceIconMotion = $state(false);
  const dark = $derived(appearance==='dark'||(appearance==='system'&&systemDark));
  $effect(()=>{ document.documentElement.dataset.theme=dark?'dark':'light'; document.documentElement.style.colorScheme=dark?'dark':'light'; });

  function setAppearance(value, event) { appearanceIconMotion=allowsIconMotion(event); appearance=value; try{localStorage.setItem('synchrome.appearance',value);}catch{} }
  function navigate(next) { route=next; actionError=null; if(next==='activity') for(const m of confirmedMappings){ conflictsOpen={...conflictsOpen,[m.id]:true}; loadConflicts(m); } }
  function openProfile(p) { selectedId=p.id; navigate('profiles'); }
  function openCreate(method='choose', option=null) { modal='profile'; modalProfile=null; modalMapping=null; profileMethod=method; selectedLocal=option?.key??''; selectedTarget=''; formName=option?.p.display_name??''; formError=''; }
  function openConnect(p, option=null, existing=null) { modal='profile'; modalProfile=p; modalMapping=existing; profileMethod='connect'; selectedLocal=option?.key??''; selectedTarget=p?.id??''; formName=''; formError=''; }
  function closeModal() { if(busyKey||authBusy)return; modal=null; formError=''; }
  function dialogTrigger(node) {
    // WKWebView does not focus mouse-clicked buttons by default. Capture the
    // invoking control before Dialog records its focus-restoration target.
    const focus = () => node.focus({ preventScroll: true });
    node.addEventListener('click', focus, true);
    return { destroy: () => node.removeEventListener('click', focus, true) };
  }
  function pickLocal(value) { selectedLocal=value; if(profileMethod==='import'&&!formName) formName=localOptions.find(o=>o.key===value)?.p.display_name??''; formError=''; }
  async function submitProfile(event) {
    event.preventDefault(); if(busyKey)return;
    const creates=['manual','import'].includes(profileMethod);
    formError=creates?validateName(formName,profiles):'';
    if(formError)return;
    if(profileMethod!=='manual'&&!pickedLocal){formError='Choose a browser profile on this Mac.';return;}
    const target=modalProfile?.id??selectedTarget;
    if(profileMethod==='connect'&&!target){formError='Choose a Synchrome profile.';return;}
    if(profileMethod==='connect'&&sameDeviceConflict(mappings,target,modalMapping?.id)){formError='This Synchrome profile already has a browser connected on this Mac. Choose a different profile, or change its existing connection.';return;}
    busyKey='profile-form'; actionError=null;
    try {
      if(dataLoadPromise)await dataLoadPromise;
      let result;
      if(profileMethod==='manual') result=await invoke('create_profile',{name:formName.trim()});
      else {
        if(modalMapping){await invoke('unmap_local_profile',{mappingId:modalMapping.id});modalMapping=null;}
        const args={browserKind:pickedLocal.b.kind,browserProfileDir:pickedLocal.p.dir_name,userDataDir:pickedLocal.b.user_data_dir};
        result=await invoke(profileMethod==='import'?'create_and_map_local_profile':'map_local_profile', profileMethod==='import'?{...args,name:formName.trim()}:{...args,profileId:target});
      }
      await loadCloudData();
      selectedId=result?.profile_id??result?.id??target; route='profiles';
      if(profileMethod==='manual'){modal=null;toast='Profile created. Connect a browser to start syncing.';}
      else {modalMapping=result;modal='setup';toast='Browser connected. Finish setup to start the first sync.';}
    } catch(e) {formError=humanize(e).text;actionError=String(e);await loadCloudData();}
    finally{busyKey=null;}
  }
  function requestDelete(p) { modal='delete';modalProfile=p;deleteName='';formError=''; }
  async function confirmDelete() {
    if(busyKey||deleteName!==modalProfile?.name)return;
    busyKey='delete';actionError=null;
    try {if(dataLoadPromise)await dataLoadPromise;await disconnectAndDelete(invoke,modalProfile.id,mappings);selectedId=null;modal=null;toast='Synchrome profile deleted. Your local browser profiles and bookmarks are still on each Mac.';}
    catch(e){formError=humanize(e).text;actionError=String(e);if(e.profileDeleted){modal=null;selectedId=null;toast='Profile removed. Cloud cleanup needs attention; see the message above.';}}
    finally{await loadCloudData();busyKey=null;}
  }
  async function submitAuth(values) {
    authBusy=true;authError=null;
    try{accountGeneration++;account=await invoke(values.mode==='signup'?'sign_up':'sign_in',{email:values.email,password:values.password});if(account?.signed_in){if(dataLoadPromise)await dataLoadPromise;await loadCloudData();onboarding=!myMappings.length;}return true;}
    catch(e){authError=String(e);return false;}
    finally{authBusy=false;}
  }
  function requestDisconnect(m) {modal='disconnect';modalMapping=m;formError='';}
  async function confirmDisconnect() {
    const ok=await act(`unmap:${modalMapping.id}`,()=>invoke('unmap_local_profile',{mappingId:modalMapping.id}));
    if(ok){modal=null;toast='Browser disconnected. Its local data is unchanged.';}else formError=humanize(actionError).text;
  }
  function continueSetup(m) {modalMapping=m;modal='setup';formError='';}
  async function saveProfileName(event) {event.preventDefault();formError=validateName(formName,profiles,modalProfile.id);if(formError)return;const ok=await act('rename',()=>invoke('rename_profile',{profileId:modalProfile.id,name:formName.trim()}));if(ok){modal=null;toast='Profile renamed.';}else formError=humanize(actionError).text;}
  const withDatasets=m=>({...m,preferences:prefList[m.id],extension_settings:setList[m.id]});
  const stateFor=m=>connectionState(withDatasets(m),cloud?.reachable!==false);
  const profileView=p=>profileState(p,mappings.map(withDatasets),cloud?.reachable!==false);
  function localName(m) { if(!m.is_this_device)return m.browser_profile_dir;const b=browsers.find(b=>b.kind===m.browser_kind);return b?.profiles.find(p=>p.dir_name===m.browser_profile_dir)?.display_name??m.browser_profile_dir; }
  function browserName(kind) { return browsers.find(b=>b.kind===kind)?.display_name??({chrome:'Google Chrome',helium:'Helium'}[kind]??kind); }
  async function openConnectionDetails(m) {detailsOpen={...detailsOpen,[m.id]:true};if(detailsOpen[m.id]){await Promise.all([loadPrefs(m),loadDiag(m)]);}}
  async function refreshWorkspace(){await loadAll();toast=cloud?.reachable===false?'Still offline. Local changes are kept.':'Status refreshed.';}
  onMount(()=>{
    let alive=true; const media=window.matchMedia('(prefers-color-scheme: dark)');systemDark=media.matches;
    try{const saved=localStorage.getItem('synchrome.appearance');if(['system','light','dark'].includes(saved))appearance=saved;}catch{}
    const change=e=>systemDark=e.matches;media.addEventListener('change',change);
    loadAll().finally(()=>{if(alive){booting=false;onboarding=signedIn&&!myMappings.length&&!onboardingDismissed;}});
    let probeAt=Date.now();
    const refreshVisible=()=>{if(!document.hidden&&signedIn&&!busyKey&&!authBusy&&!modal&&!cloudDataBusy){loadCloudData();if(Date.now()-probeAt>60000||cloud?.reachable===false){probeAt=Date.now();loadCloud();}}};
    const timer=setInterval(refreshVisible,15000);window.addEventListener('focus',refreshVisible);
    return()=>{alive=false;clearInterval(timer);window.removeEventListener('focus',refreshVisible);media.removeEventListener('change',change);};
  });
</script>

{#snippet mappingBlock(m)}
  <span class="arrow">→</span>
  <strong>{m.profile_name ?? "(unknown profile)"}</strong>
  <span class="badge small {MAP_STATUS[m.display_status]?.cls ?? 'muted'}">{MAP_STATUS[m.display_status]?.label ?? m.display_status}</span>
  <span class="pdir">{m.device_name ?? "this device"}</span>
  {#if m.display_status !== "extension_confirmed"}
    <button class="tiny-btn" onclick={() => retryWrite(m)} disabled={!!busyKey}
      title="Writes the link into this browser profile. The browser must be closed.">Retry local write</button>
  {/if}
  <button class="tiny-btn" onclick={() => checkExtension(m)} disabled={!!busyKey}
    title="Re-reads the link status. The extension reconnects when the browser starts, or from its popup.">Check extension</button>
  <button class="tiny-btn" onclick={() => unmap(m)} disabled={!!busyKey}>Unmap</button>
  {#if m.local_error}{@const h = humanize(m.local_error)}<span class="small bad">{h.text}{#if h.detail}<details class="tech"><summary>Technical details</summary><code>{h.detail}</code></details>{/if}</span>{/if}
  {#if m.display_status === "extension_confirmed"}
    {@const bm = m.bookmarks ?? { state: "not_synced", bookmarks: 0, folders: 0, pending: 0 }}
    <div class="bookmarks-row">
      <span class="small">Bookmarks</span>
      <span class="badge small {BM_STATUS[bm.state]?.cls ?? 'muted'}">{BM_STATUS[bm.state]?.label ?? bm.state}</span>
      <span class="pdir">{bm.bookmarks} bookmarks · {bm.folders} folders</span>
      <span class="pdir">last upload {when(bm.last_upload_at)}</span>
      <span class="pdir">{bm.pending} pending</span>
      {#if bm.pending > 0}
        <button class="tiny-btn" onclick={() => uploadNow(m)} disabled={!!busyKey}
          title="Downloads remote changes and uploads the queued local ones now.">Sync now</button>
      {/if}
      {#if bm.error}{@const h = humanize(bm.error)}<span class="small bad">{h.text}{#if h.detail}<details class="tech"><summary>Technical details</summary><code>{h.detail}</code></details>{/if}</span>{/if}
    </div>
    {#if bm.download}
      <div class="bookmarks-row">
        <span class="small">Bookmarks download</span>
        <span class="badge small {DL_STATUS[bm.download.state]?.cls ?? 'muted'}">{DL_STATUS[bm.download.state]?.label ?? bm.download.state}</span>
        <span class="pdir">last download {when(bm.download.last_download_at)}</span>
        <span class="pdir">{bm.download.remote_pending} remote changes pending</span>
        {#if bm.download.conflicts > 0}
          <span class="badge small bad">Conflict · {bm.download.conflicts}</span>
        {:else}
          <span class="pdir">0 conflicts</span>
        {/if}
        {#if bm.download.resolving > 0}<span class="badge small info">Applying {bm.download.resolving}</span>{/if}
        <button class="tiny-btn" onclick={() => toggleConflicts(m)}>
          {conflictsOpen[m.id] ? "Hide conflicts" : bm.download.conflicts > 0 ? `Review conflicts (${bm.download.conflicts})` : "Conflict history"}
        </button>
        {#if bm.download.error}{@const h = humanize(bm.download.error)}<span class="small bad">{h.text}{#if h.detail}<details class="tech"><summary>Technical details</summary><code>{h.detail}</code></details>{/if}</span>{/if}
      </div>
    {/if}
    {#if conflictsOpen[m.id]}{@render conflictPanel(m)}{/if}

  {/if}
  {#if m.display_status === "extension_confirmed" && m.extensions}
    {@const xs = m.extensions}
    <div class="bookmarks-row">
      <span class="small">Extensions</span>
      <span class="badge small {xs.conflicts ? 'bad' : xs.differences || xs.missing || xs.user_action ? 'info' : 'ok'}">{xs.conflicts ? `Conflict · ${xs.conflicts}` : xs.differences || xs.missing || xs.user_action ? "Differences" : "Synced"}</span>
      <span class="pdir">{xs.installed} installed · {xs.synced} synced · {xs.missing} missing · {xs.differences} differences · {xs.conflicts} conflicts{xs.user_action ? ` · ${xs.user_action} need you` : ""}</span>
      <button class="tiny-btn" onclick={() => toggleExtensions(m)}>{extOpen[m.id] ? "Hide extensions" : "Extensions"}</button>
    </div>
    {#if extOpen[m.id]}
      {@const xl = extList[m.id] ?? []}
      <div class="conflicts">
        {#if extMsg[m.id]}<p class="ok small" role="status">{extMsg[m.id]}</p>{/if}
        {#if xl.length === 0}<p class="muted small">No extensions yet (the Synchrome extension itself is never listed).</p>{/if}
        <table class="cmp">
          <thead><tr><th>Extension</th><th>This device</th><th>Synchrome</th><th></th></tr></thead>
          <tbody>
            {#each xl as x (x.ext_id)}
              <tr class={x.state === "synced" ? "" : "diff"}>
                <td>{x.name}{#if x.version}<span class="pdir"> · v{x.version}</span>{/if}</td>
                <td>{x.installed_here ? onOff(x.enabled_here) : "Missing"}</td>
                <td>{x.installed_synced ? onOff(x.enabled_synced) : "Removed"}
                  <br /><span class="badge small {X_STATE[x.state]?.cls ?? 'muted'}">{X_STATE[x.state]?.label ?? x.state}</span>
                  {#if x.state === "missing"}<span class="pdir"> {X_AVAIL[x.availability] ?? x.availability}</span>{/if}
                  {#if x.error}<span class="small bad"> {humanize(x.error).text}</span>{/if}
                </td>
                <td>
                  {#each x.actions as a}
                    {@const key = `${x.ext_id}:${a}`}
                    <button class="tiny-btn {a === 'remove' ? 'danger' : ''}" disabled={!!busyKey} onclick={() => extAct(m, x, a)}>
                      {extConfirm === key ? "Confirm: remove from this browser" : X_ACTION[a] ?? a}
                    </button>
                  {/each}
                  {#if x.state === "conflict"}
                    <button class="tiny-btn" onclick={() => { if (!conflictsOpen[m.id]) toggleConflicts(m); }}>Resolve conflict</button>
                  {/if}
                  {#if extConfirm?.startsWith(x.ext_id)}<button class="tiny-btn" onclick={() => (extConfirm = null)}>Cancel</button>{/if}
                </td>
              </tr>
              {#if x.installed_here && x.state !== "protected"}
                {@const sv = setList[m.id]?.[x.ext_id]}
                {@const key = `${m.id}:${x.ext_id}`}
                <tr class="settings-row"><td colspan="4">
                  <span class="small">Settings sync</span>
                  {#if sv}<span class="badge small {S_STATE[sv.state]?.cls ?? 'muted'}">{S_STATE[sv.state]?.label ?? sv.state}</span>{/if}
                  <select aria-label={`Settings sync for ${x.name}`} value={setMode[key] ?? sv?.mode ?? "off"} onchange={(e) => (setMode = { ...setMode, [key]: e.currentTarget.value })} disabled={!!busyKey}>
                    <option value="off">Off</option><option value="local">Local only</option><option value="local_sync">Local + Sync</option>
                  </select>
                  {#if (setMode[key] ?? sv?.mode ?? "off") !== (sv?.mode ?? "off")}
                    <button class="tiny-btn" onclick={() => applyMode(m, x, setMode[key])}>{(setMode[key] ?? "off") === "off" ? "Disable" : "Enable settings sync"}</button>
                  {/if}
                  {#if sv && sv.mode !== "off"}
                    <span class="pdir">{sv.keys_local} keys local · {sv.keys_sync} keys sync · {sv.pending} pending · {sv.conflicts} conflicts · {sv.excluded_sensitive} excluded sensitive</span>
                    {#if sv.state === "error" || sv.state === "waiting_for_browser_close" || sv.state === "version_mismatch"}
                      <button class="tiny-btn" onclick={() => settingsNow(m, x)} disabled={!!busyKey}>Retry</button>
                    {/if}
                    {#if sv.conflicts > 0}<button class="tiny-btn" onclick={() => { if (!conflictsOpen[m.id]) toggleConflicts(m); }}>Review conflicts</button>{/if}
                    {#if sv.error}<span class="small bad">{humanize(sv.error).text}</span>{/if}
                  {/if}
                  {#if (setMode[key] ?? sv?.mode ?? "off") !== "off"}
                    <span class="small warn">Extension settings may contain private data. Synchrome excludes keys that look sensitive (token, password, secret…), but cannot recognise every secret. Values are read and written only while this browser is closed.</span>
                  {/if}
                </td></tr>
              {/if}
            {/each}
          </tbody>
        </table>
        <p class="hint tiny">Installing opens the official store page; removing asks for confirmation. Nothing is installed or uninstalled automatically.</p>
      </div>
    {/if}
  {/if}
  {#if m.display_status === "extension_confirmed" && diag[m.id]}
    {@const dg = diag[m.id]}
    <div class="bookmarks-row sync-health">
      <span class="small">Sync health</span>
      <span class="badge small {dg.realtime.connected ? 'ok' : 'muted'}">{dg.realtime.connected ? "Realtime connected" : dg.realtime.enabled ? "Realtime disconnected" : "Realtime off"}</span>
      <span class="pdir">snapshot {dg.cloud?.current_generation ? `generation ${dg.cloud.current_generation}` : "none yet"} · {dg.snapshot_generations_kept} kept · last compacted {ago(serverSecs(dg.cloud?.current_ready_at, dg.cloud?.server_now) ?? dg.local.last_compaction_at)}{dg.local.last_gc_at ? ` · journals cleaned ${ago(dg.local.last_gc_at)}` : ""}</span>
      {#if dg.gc_blocked}<span class="small warn">Journal cleanup paused: {(dg.cloud?.gc_blockers ?? dg.local?.gc_blockers ?? 0)} device{(dg.cloud?.gc_blockers ?? dg.local?.gc_blockers ?? 0) === 1 ? "" : "s"} mapped to this profile cannot read snapshots yet (update Synchrome there, or remove the device).</span>{/if}
      {#if dg.local.snapshot_generation}<span class="small">This device started from snapshot generation {dg.local.snapshot_generation}{dg.local.bootstrap_fallback ? " (previous generation, the current one failed verification)" : ""}.</span>{/if}
      {#if dg.this_device_retired}<span class="small warn">You removed this device from this profile's compaction: it still syncs, but it no longer holds journals for others. <button class="tiny-btn" onclick={() => deviceAction(m, dg.devices.find((d) => d.is_current), "reactivate")} disabled={!!busyKey}>Reactivate</button></span>{/if}
      {#if dg.devices?.some((d) => (d.blocks_gc && !d.is_current) || (d.retired && !d.is_current))}
        <span class="small">Devices on this profile:</span>
        {#each dg.devices.filter((d) => !d.is_current && (d.blocks_gc || d.retired)) as d (d.device_id)}
          {@const key = `${m.profile_id}:${d.device_id}:retire`}
          <span class="pdir">{d.name} — {d.retired ? "removed from compaction" : "cannot read snapshots yet"}
            {#if d.retired}<button class="tiny-btn" onclick={() => deviceAction(m, d, "reactivate")} disabled={!!busyKey}>Reactivate</button>
            {:else}<button class="tiny-btn {deviceConfirm === key ? 'danger' : ''}" onclick={() => deviceAction(m, d, "retire")} disabled={!!busyKey}>{deviceConfirm === key ? "Confirm: remove from this profile's compaction" : "Remove from compaction"}</button>{/if}
          </span>
        {/each}
      {/if}
    </div>
  {/if}
  {#if m.display_status === "extension_confirmed"}
    {@const pv = prefList[m.id]}
    {@const chosen = prefMode[m.id] ?? pv?.mode ?? "off"}
    <div class="bookmarks-row prefs-row">
      <span class="small">Browser preferences</span>
      {#if pv}<span class="badge small {P_STATE[pv.state]?.cls ?? 'muted'}">{P_STATE[pv.state]?.label ?? pv.state}</span>{/if}
      <select aria-label="Browser preferences sync" value={chosen} onchange={(e) => (prefMode = { ...prefMode, [m.id]: e.currentTarget.value })} disabled={!!busyKey}>
        <option value="off">Off</option><option value="recommended">Sync recommended preferences</option>
      </select>
      {#if chosen !== (pv?.mode ?? "off")}
        <button class="tiny-btn" onclick={() => applyPrefsMode(m, chosen)} disabled={!!busyKey}>{chosen === "off" ? "Disable" : "Enable"}</button>
      {/if}
      {#if pv && pv.mode !== "off"}
        <span class="pdir">{pv.synced} preferences synced · {pv.unsupported} unsupported · {pv.pending} pending · {pv.conflicts} conflicts{pv.invalid ? ` · ${pv.invalid} invalid` : ""}</span>
        {#if ["error", "waiting_for_browser_close", "version_mismatch", "unsupported"].includes(pv.state)}
          <button class="tiny-btn" onclick={() => prefsNow(m)} disabled={!!busyKey}>Retry</button>
        {/if}
        {#if pv.conflicts > 0}<button class="tiny-btn" onclick={() => { if (!conflictsOpen[m.id]) toggleConflicts(m); }}>Review conflicts</button>{/if}
        {#if pv.error}{@const h = humanize(pv.error)}<span class="small bad">{h.text}{#if h.detail}<details class="tech"><summary>Technical details</summary><code>{h.detail}</code></details>{/if}</span>{/if}
      {/if}
      {#if chosen !== "off"}
        <span class="small warn">Browser preferences may affect how your browser behaves on this device.</span>
      {/if}
      {#if prefMsg[m.id]}<span class="ok small" role="status">{prefMsg[m.id]}</span>{/if}
    </div>
  {/if}
{/snippet}

{#snippet conflictPanel(m)}
      {@const list = conflictList[m.id] ?? []}
      {@const openList = list.filter((c) => c.status === "unresolved" || c.status === "resolving")}
      {@const closed = list.filter((c) => c.status === "resolved" || c.status === "superseded")}
      <div class="conflicts">
        {#if lastResolved[m.id]}
          <p class="small {lastResolved[m.id].ok ? 'ok' : 'bad'}" role="status">{lastResolved[m.id].text}</p>
        {/if}
        {#if openList.length === 0}
          <p class="muted small">No open conflicts. Everything else keeps syncing while a conflict is open; only the conflicting item waits.</p>
        {/if}
        {#each openList as c (c.conflict_id)}
          <div class="conflict">
            <div class="conflict-head">
              <span class="badge small {C_STATUS[c.status].cls}">{C_STATUS[c.status].label}</span>
              <strong>{c.item_kind === "folder" ? "Folder" : c.item_kind === "extension" ? "Extension" : c.item_kind === "setting" ? "Extension setting" : c.item_kind === "preference" ? "Browser preference" : "Bookmark"} “{c.name}”</strong>
              <span class="pdir">{C_SUBTYPE[c.subtype] ?? c.subtype}</span>
            </div>
            <div class="pdir">Since {when(c.created_at)} · other device: {deviceName(c.remote_device)}{c.held > 1 ? ` · ${c.held} remote changes held` : ""}{c.attempts > 1 ? ` · ${c.attempts} failed attempts` : ""}{c.detail && c.kind === "apply_failed" ? ` · browser: ${c.detail}` : ""}</div>
            <table class="cmp">
              <thead><tr><th></th><th>This device</th><th>{deviceName(c.remote_device)}</th>{#if c.base}<th class="muted">Before</th>{/if}</tr></thead>
              <tbody>
                {#each compareRows(c) as row}
                  <tr class={row.diff ? "diff" : ""}><td>{row.k}</td><td>{row.l}</td><td>{row.r}</td>{#if c.base}<td class="muted">{row.b ?? ""}</td>{/if}</tr>
                {/each}
              </tbody>
            </table>
            {#if c.subtype === "delete_not_empty" && c.local_items > 0}
              <p class="warn small">{c.local_items} item{c.local_items === 1 ? "" : "s"} in this folder exist only on this device. Nothing is deleted unless you choose so.</p>
            {/if}
            {#if c.status === "resolving"}
              <p class="info small">Decision saved ({RES_DONE[c.resolution] ?? c.resolution}). The browser is brought to it on the next sync.</p>
            {:else}
              <div class="conflict-actions">
                {#each c.options as o}
                  {@const key = `${c.conflict_id}:${o.resolution}`}
                  <button class="tiny-btn {o.destructive ? 'danger' : ''}" onclick={() => choose(m, c, o)} disabled={!!busyKey}>
                    {confirmKey === key ? `Confirm: ${optLabel(c, o.resolution)}` : optLabel(c, o.resolution)}
                  </button>
                {/each}
                {#if confirmKey?.startsWith(c.conflict_id)}
                  <span class="small warn">This deletes data that still exists on one side. Click again to confirm.</span>
                  <button class="tiny-btn" onclick={() => (confirmKey = null)}>Cancel</button>
                {/if}
              </div>
            {/if}
            {#if resolvedMsg[c.conflict_id]}<p class="ok small">{resolvedMsg[c.conflict_id]}</p>{/if}
          </div>
        {/each}
        {#if closed.length > 0}
          <button class="tiny-btn" onclick={() => (showHistory = { ...showHistory, [m.id]: !showHistory[m.id] })}>
            {showHistory[m.id] ? "Hide history" : `History (${closed.length})`}
          </button>
          {#if showHistory[m.id]}
            <ul class="plain">
              {#each closed as c (c.conflict_id)}
                <li class="row-item">
                  <span class="badge small {C_STATUS[c.status].cls}">{C_STATUS[c.status].label}</span>
                  <span class="pname">{c.item_kind === "folder" ? "Folder" : c.item_kind === "extension" ? "Extension" : c.item_kind === "setting" ? "Extension setting" : c.item_kind === "preference" ? "Browser preference" : "Bookmark"} “{c.name}”</span>
                  <span class="pdir">{c.status === "superseded" ? (c.detail ?? "no longer needed") : RES_DONE[c.resolution] ?? c.resolution}{c.resolution_ops ? ` · ${c.resolution_ops} change${c.resolution_ops === 1 ? "" : "s"} published${c.upload_pending ? " (uploading)" : ""}` : ""}</span>
                  <span class="pdir">{when(c.resolved_at)}</span>
                  {#if resolvedMsg[c.conflict_id]}<span class="ok small">{resolvedMsg[c.conflict_id]}</span>{/if}
                </li>
              {/each}
            </ul>
          {/if}
        {/if}
      </div>

{/snippet}

{#snippet profileMark(p, large=false)}
  <span class:large class="profile-mark" style={`--profile-hue: ${[164,217,33,282,8][profiles.findIndex(x=>x.id===p.id)%5]??164}`} aria-hidden="true">{(Array.from(p.name)[0]??'P').toLocaleUpperCase()}</span>
{/snippet}
{#snippet status(state)}<span class={`status ${state.tone}`}><span class="status-dot"></span>{state.label}</span>{/snippet}
{#snippet errorNotice(value, retry=false)}
  {@const friendly=humanize(value)}
  <div class="notice danger" role="alert"><Icon name="warning" size={19}/><div><strong>{friendly.text}</strong>{#if friendly.detail}<details class="tech"><summary>Technical details</summary><code>{friendly.detail}</code></details>{/if}</div>{#if retry}<button class="button secondary" onclick={refreshWorkspace} disabled={cloudDataBusy}>Retry</button>{/if}</div>
{/snippet}
{#snippet emptyProfiles()}
  <div class="empty-state"><span class="empty-symbol"><Icon name="profiles" size={30}/></span><h2>A place for every part of your browsing.</h2><p>Create Personal, Work, or any profile you need. Connect a browser on this Mac, then bring it to your other Macs.</p><div class="actions"><button use:dialogTrigger class="button primary" onclick={()=>openCreate('import')}><Icon name="download" size={17}/>Use a browser profile</button><button use:dialogTrigger class="button secondary" onclick={()=>openCreate('manual')}>Start fresh</button></div></div>
{/snippet}
{#snippet connection(m)}
  {@const state=stateFor(m)}
  <div class="connection" data-mapping-id={m.id}>
    <div class="connection-main"><span class="browser-symbol"><Icon name="browser" size={22}/></span><div class="grow"><strong>{browserName(m.browser_kind)} <span class="separator">/</span> {localName(m)}</strong><p>{state.description}</p></div>{@render status(state)}</div>
    {#if m.is_this_device}
      <div class="connection-actions">
        {#if state.action==='conflicts'}<button class="button primary small-button" onclick={()=>navigate('activity')}>Review conflicts</button>
        {:else if state.action==='setup'}<button use:dialogTrigger class="button primary small-button" onclick={()=>continueSetup(m)}>Finish setup</button>
        {:else if state.action==='extensions'}<button class="button secondary small-button" onclick={async()=>{await openConnectionDetails(m);if(!extOpen[m.id])await toggleExtensions(m);}}>Review extensions</button>
        {:else if state.action==='details'}<button class="button secondary small-button" onclick={()=>openConnectionDetails(m)}>View details</button>
        {:else if state.action==='retry'}<button class="button secondary small-button" onclick={()=>retryWrite(m)} disabled={!!busyKey}>Retry link</button>
        {:else}<button class="button secondary small-button" onclick={()=>uploadNow(m)} disabled={!!busyKey}>Sync now</button>{/if}
        <button use:dialogTrigger class="button quiet small-button" onclick={()=>openConnect(profiles.find(p=>p.id===m.profile_id),null,m)} disabled={!!busyKey}>Change browser</button><button use:dialogTrigger class="button quiet small-button" onclick={()=>requestDisconnect(m)} disabled={!!busyKey}>Disconnect</button>
      </div>
      <details class="connection-details" open={detailsOpen[m.id]} ontoggle={(event)=>{const opened=event.currentTarget.open;const changed=opened!==!!detailsOpen[m.id];detailsOpen={...detailsOpen,[m.id]:opened};if(opened&&changed){loadPrefs(m);loadDiag(m);}}}><summary>Connection details &amp; diagnostics</summary><div class="advanced-content">{@render mappingBlock(m)}</div></details>
    {:else}<div class="remote-note"><Icon name="info" size={15}/><span>Manage sync in Synchrome on {m.device_name??'that Mac'}. This connection does not show its live status.</span><button use:dialogTrigger class="button quiet small-button" onclick={()=>requestDisconnect(m)} disabled={!!busyKey}>Disconnect</button></div>{/if}
  </div>
{/snippet}

<svelte:head><title>Synchrome — {signedIn?nav.find(n=>n.id===route)?.name??'Profiles':'Welcome'}</title></svelte:head>
{#if previewEnabled}<div class="preview-banner" role="note">Development preview · {previewScenario} · sample data</div>{/if}
{#if booting}
  <main class="boot" aria-busy="true"><span class="brand-logo"><Icon name="logo" size={31}/></span><h1>Synchrome</h1><p>Opening your workspace…</p><span class="loading-line"></span></main>
{:else if !signedIn}
  <AuthView busy={authBusy} error={authError?humanize(authError):error?humanize(error):account?.error?humanize(account.error):null} {cloud} {account} onsubmit={submitAuth} onretry={loadAll}/>
{:else}
  <a class="skip-link" href="#main-content">Skip to content</a>
  <div class="workspace">
    <aside class="sidebar">
      <a href="#main-content" class="brand" onclick={(event)=>{event.preventDefault();navigate('overview');}} aria-label="Synchrome overview"><span class="brand-logo"><Icon name="logo" size={25}/></span><span>Synchrome</span></a>
      <nav aria-label="Main navigation">{#each nav as item}<button class:active={route===item.id} class="nav-item" aria-current={route===item.id?'page':undefined} aria-label={item.name} title={item.name} data-testid={`nav-${item.id}`} onclick={()=>navigate(item.id)}><Icon name={item.icon} size={20}/><span>{item.name}</span>{#if item.id==='activity'&&conflictCount}<span class="nav-count">{conflictCount}</span>{/if}</button>{/each}</nav>
      <div class="sidebar-bottom"><div class="service-status">{@render status(cloudState(cloud))}</div><button class="account-button" onclick={()=>navigate('settings')} title={account.email}><span class="avatar">{(account.email??'S').slice(0,1).toUpperCase()}</span><span class="account-text"><strong>{account.email?.split('@')[0]??'Your account'}</strong><span>{device?.device_name??'This Mac'}</span></span><Icon name="settings" size={16}/></button></div>
    </aside>
    <div class="work-area">
      <header class="topbar"><span>{device?.device_name??'This Mac'}<span class="topbar-divider">/</span>{nav.find(n=>n.id===route)?.name}</span><div class="topbar-actions">{#if cloudDataBusy}<span class="refresh-label" role="status">Updating…</span>{/if}<button class="icon-button" aria-label="Refresh workspace" title="Refresh workspace" onclick={refreshWorkspace} disabled={cloudDataBusy||!!busyKey}><Icon name="refresh" size={18}/></button><button class="icon-button" aria-label={dark?'Switch to light mode':'Switch to dark mode'} title={dark?'Switch to light mode':'Switch to dark mode'} onclick={(event)=>setAppearance(dark?'light':'dark',event)}><Icon name={dark?'sun':'moon'} size={18} morph animate={appearanceIconMotion}/></button></div></header>
      <main id="main-content" class="main-content" tabindex="-1">
        {#if cloud?.reachable===false}<div class="notice warning"><Icon name="cloud" size={19}/><div><strong>You’re offline. Your local changes are kept.</strong><p>Sync resumes when Synchrome Cloud is available.</p></div><button class="button secondary" onclick={refreshWorkspace} disabled={cloudDataBusy}>Try again</button></div>{/if}
        {#if account?.needs_refresh}<div class="notice warning"><Icon name="shield" size={19}/><div><strong>Your session needs to be refreshed.</strong><p>Reconnect your account to continue syncing.</p></div><button class="button secondary" onclick={refresh} disabled={authBusy||!!busyKey}>Refresh session</button></div>{/if}
        {#if error}{@render errorNotice(error,true)}{/if}
        {#if cloudDataError}{@render errorNotice(cloudDataError,true)}{/if}
        {#if actionError&&modal===null}{@render errorNotice(actionError)}{/if}
        {#if route==='overview'}
          <div class="page-heading"><div><h1>Your browsing, together.</h1><p>One home for the profiles you use across your Macs.</p></div><button use:dialogTrigger class="button primary" onclick={()=>openCreate()} disabled={cloudDataBusy||!!busyKey}><Icon name="plus" size={18}/>New profile</button></div>
          {#if profiles.length===0}{@render emptyProfiles()}
          {:else}
            {#if attention}
              <section class="attention-strip" aria-label="Needs attention"><span class="attention-icon"><Icon name="warning" size={21}/></span><div class="grow"><h2>{attention.count} connection{attention.count===1?'':'s'} need{attention.count===1?'s':''} your attention</h2><p>{attention.description}</p></div><button class="button secondary" onclick={()=>{if(attention.action==='conflicts')navigate('activity');else openProfile(profiles.find(p=>p.id===attention.profileId));}}>{attention.action==='conflicts'?'Review conflicts':'Review connection'}<Icon name="arrow-right" size={16}/></button></section>
            {/if}
            <section class="profile-overview"><div class="section-heading"><h2>Your profiles <span class="count">{profiles.length}</span></h2><button class="text-button" onclick={()=>navigate('profiles')}>Manage profiles<Icon name="arrow-right" size={16}/></button></div><div class="profile-table">{#each profiles as p}{@const connections=mappingsOf(p.id)}{@const state=profileView(p)}<button class="profile-overview-row" onclick={()=>openProfile(p)} aria-label={`Open profile ${p.name}`}>{@render profileMark(p)}<span class="profile-row-title"><strong>{p.name}</strong><span>{connections.length?`${new Set(connections.map(m=>m.device_id)).size} Mac${new Set(connections.map(m=>m.device_id)).size===1?'':'s'} · ${connections.length} browser connection${connections.length===1?'':'s'}`:'Ready for its first browser'}</span></span><span class="connection-trail">{#each groupConnections(connections).slice(0,2) as group}<span><Icon name="laptop" size={15}/>{group.name}</span>{/each}</span>{@render status(state)}<Icon name="chevron-right" size={18}/></button>{/each}</div></section>
            <div class="overview-bottom"><section><h2>On this Mac</h2><p>{confirmedMappings.length} connected browser{confirmedMappings.length===1?'':'s'} · {browsers.length} detected</p><button class="text-button" onclick={()=>navigate('devices')}>Manage browsers<Icon name="arrow-right" size={16}/></button></section><section><h2>What travels with you</h2><p>Bookmarks and extensions. Settings and supported preferences sync when you choose to enable them.</p><span class="privacy-note"><Icon name="shield" size={15}/>Passwords, cookies and sessions stay out of Synchrome.</span></section></div>
          {/if}
        {:else if route==='profiles'}
          <div class="page-heading"><div><h1>Profiles</h1><p>A shared profile. Your browsers, across devices.</p></div><button use:dialogTrigger class="button primary" onclick={()=>openCreate()} disabled={cloudDataBusy||!!busyKey}><Icon name="plus" size={18}/>New profile</button></div>
          {#if profiles.length===0}{@render emptyProfiles()}{:else}
            <div class="profiles-layout"><aside class="profile-list" aria-label="Synchrome profiles"><div class="search-field"><Icon name="search" size={16}/><input aria-label="Find a profile" placeholder="Find a profile" bind:value={profileSearch}/></div>{#each visibleProfiles as p}<button data-testid="profile-row" data-profile-name={p.name} data-has-local={mappingsOf(p.id).some(m=>m.is_this_device)} aria-label={`Open profile ${p.name}`} aria-pressed={selectedProfile?.id===p.id} class:chosen={selectedProfile?.id===p.id} class="profile-list-item" onclick={()=>selectedId=p.id}>{@render profileMark(p)}<span><strong>{p.name}</strong><small>{new Set(mappingsOf(p.id).map(m=>m.device_id)).size} Mac{new Set(mappingsOf(p.id).map(m=>m.device_id)).size===1?'':'s'}</small></span>{#if ['warning','danger'].includes(profileView(p).tone)}<span class="attention-dot" aria-label="Needs attention"></span>{/if}</button>{/each}{#if !visibleProfiles.length}<p class="muted padded">No profiles match “{profileSearch}”.</p>{/if}</aside>
              {#if selectedProfile}<section class="profile-detail" aria-label={`${selectedProfile.name} profile`}><div class="profile-detail-heading">{@render profileMark(selectedProfile,true)}<div class="grow"><h2>{selectedProfile.name}</h2>{@render status(profileView(selectedProfile))}</div><button use:dialogTrigger class="icon-button" aria-label={`Rename ${selectedProfile.name}`} title="Rename profile" onclick={()=>{modal='rename';modalProfile=selectedProfile;formName=selectedProfile.name;formError='';}} disabled={!!busyKey}><Icon name="edit" size={18}/></button><button use:dialogTrigger class="icon-button destructive" aria-label={`Delete ${selectedProfile.name}`} title="Delete profile" onclick={()=>requestDelete(selectedProfile)} disabled={!!busyKey}><Icon name="trash" size={18}/></button></div>
                <div class="section-heading"><h3>Connected browsers</h3><button use:dialogTrigger class="button secondary small-button" onclick={()=>openConnect(selectedProfile)} disabled={!!busyKey||cloudDataBusy||sameDeviceConflict(mappings,selectedProfile.id)} title={sameDeviceConflict(mappings,selectedProfile.id)?'This profile already has a browser on this Mac. Use Change browser below.':undefined}><Icon name="plus" size={15}/>Connect browser</button></div>
                {#if !selectedConnections.length}<div class="empty-connection"><Icon name="link" size={27}/><h3>Connect its first browser.</h3><p>Choose a local Chrome or Helium profile. Its data stays on this Mac when you disconnect.</p><button use:dialogTrigger class="button primary" onclick={()=>openConnect(selectedProfile)}>Connect browser</button></div>{/if}
                {#each groupConnections(selectedConnections) as group}<section class="device-group"><h3><Icon name="laptop" size={17}/>{group.name}{#if group.isCurrent}<span class="tag">This Mac</span>{/if}</h3>{#each group.connections as m}{@render connection(m)}{/each}</section>{/each}
                <p class="footnote"><Icon name="info" size={15}/>Each Synchrome profile supports one browser connection per Mac. To use another browser here, change the existing connection.</p>
              </section>{/if}
            </div>
          {/if}
        {:else if route==='devices'}
          <div class="page-heading"><div><h1>Devices &amp; browsers</h1><p>See where your profiles live. Connect a browser on this Mac.</p></div><button class="button secondary" onclick={loadBrowsers} disabled={loadingBrowsers}><Icon name="refresh" size={16}/>Find browsers</button></div>
          <section class="devices-section"><h2>Your Macs <span class="count">{devices.length}</span></h2><div class="device-list">{#each devices as d}<div class="device-row"><span class="device-symbol"><Icon name="laptop" size={25}/></span><div class="grow"><strong>{d.name}</strong><p>{new Set(mappings.filter(m=>m.device_id===d.device_id).map(m=>m.profile_id)).size} shared profiles · {d.is_current?'macOS on this Mac':`Last seen ${d.last_seen_at?new Date(d.last_seen_at).toLocaleDateString():'unknown'}`}</p></div>{#if d.is_current}<span class="tag">This Mac</span>{:else}<span class="muted small">Remote device</span>{/if}</div>{/each}</div></section>
          <section class="browsers-section"><div class="section-heading"><h2>Browsers on {device?.device_name??'this Mac'}</h2><span class="muted small">{totalProfiles} local profiles</span></div>
            {#if !browsers.length}<div class="empty-connection"><Icon name="browser" size={28}/><h3>No browsers found yet.</h3><p>Install Chrome stable or Helium, open it once, then choose Find browsers.</p></div>{/if}
            {#each browsers as b}<div class="browser-section"><div class="browser-heading"><Icon name="browser" size={24}/><div class="grow"><h3>{b.display_name}</h3><p>{b.access==='browser_running'?'Open · some changes wait until it closes':b.access==='requires_full_disk_access'?'macOS permission needed':'Available on this Mac'}</p></div>{#if b.access==='requires_full_disk_access'}<button class="button secondary small-button" onclick={openFdaSettings}>Grant access</button>{/if}</div>{#if b.access==='requires_full_disk_access'}<p class="permission-note">To connect {b.display_name}, allow Synchrome in System Settings → Privacy &amp; Security → Full Disk Access. Reopen Synchrome after granting access. Helium does not require this permission.</p>{/if}
              {#each b.profiles as p}{@const m=mappingFor(b,p)}<div class="local-profile-row"><Icon name="folder" size={18}/><div class="grow"><strong>{p.display_name??p.dir_name}</strong><p>{m?`Connected to ${m.profile_name}`:'Not connected to Synchrome'}</p></div>{#if m}<button class="button secondary small-button" onclick={()=>openProfile(profiles.find(p=>p.id===m.profile_id))}>View profile</button>{:else}<button use:dialogTrigger class="button secondary small-button" onclick={()=>openCreate('import',{key:localKey(b,p),b,p})} disabled={!!busyKey}>Import profile</button><button use:dialogTrigger class="button quiet small-button" onclick={()=>openConnect(null,{key:localKey(b,p),b,p})} disabled={!profiles.length||!!busyKey}>Connect</button>{/if}</div>{/each}
              <details class="browser-advanced"><summary>Browser setup &amp; location</summary><p class="muted small">{b.user_data_dir}</p><button class="button secondary small-button" onclick={()=>installConnector(b)} disabled={!!busyKey}>Install connector</button>{#if connectorMsg[b.kind]}<p class="small" role="status">{connectorMsg[b.kind]}</p>{/if}<button class="button quiet small-button" onclick={revealExtension}>Show companion folder</button></details>
            </div>{/each}
            {#if unmatched.length}<div class="notice warning"><Icon name="warning" size={19}/><div><strong>{unmatched.length} connected browser profile{unmatched.length===1?' is':'s are'} missing on this Mac.</strong><p>It may have been moved or removed. Open the Synchrome profile to change or disconnect its browser.</p></div></div>{/if}
          </section>
        {:else if route==='activity'}
          <div class="page-heading"><div><h1>Activity</h1><p>Review conflicts and the latest sync observed on this Mac.</p></div><button class="button secondary" onclick={refreshWorkspace} disabled={cloudDataBusy}><Icon name="refresh" size={16}/>Refresh</button></div>
          <section class="activity-conflicts"><div class="section-heading"><h2>Needs your decision</h2>{#if conflictCount}<span class="count danger">{conflictCount}</span>{/if}</div>{#if !confirmedMappings.length}<div class="empty-connection"><Icon name="check" size={27}/><h3>No conflicts to review.</h3><p>Connect a browser and finish its first sync to see activity here.</p></div>{:else}{#each confirmedMappings as m}<div class="activity-profile"><div class="section-heading"><h3>{m.profile_name} <span class="muted small">· {browserName(m.browser_kind)}</span></h3><button class="text-button" onclick={()=>openProfile(profiles.find(p=>p.id===m.profile_id))}>View connection<Icon name="arrow-right" size={15}/></button></div>{#if conflictsOpen[m.id]}{@render conflictPanel(m)}{:else}<button class="button secondary" onclick={()=>toggleConflicts(m)}>Load conflicts</button>{/if}</div>{/each}{/if}</section>
          <section class="recent-sync"><h2>Latest sync</h2><p class="muted">Last known results, rather than a complete event history.</p>{#each confirmedMappings as m}<div class="sync-event"><Icon name="refresh" size={18}/><div class="grow"><strong>{m.profile_name}</strong><p>{browserName(m.browser_kind)} · {m.bookmarks?.bookmarks??0} bookmarks</p></div><span class="muted small">{m.bookmarks?.last_upload_at?when(m.bookmarks.last_upload_at):'First sync pending'}</span>{@render status(stateFor(m))}</div>{/each}</section>
        {:else if route==='settings'}
          <div class="page-heading"><div><h1>Settings</h1><p>Make Synchrome yours. Keep control of your account and sync.</p></div></div>
          <section class="settings-section"><h2>Appearance</h2><p>Choose how Synchrome looks on this Mac.</p><div class="appearance-options" role="group" aria-label="Appearance">{#each [{id:'system',name:'System',icon:'system'},{id:'light',name:'Light',icon:'sun'},{id:'dark',name:'Dark',icon:'moon'}] as option}<button class:chosen={appearance===option.id} aria-pressed={appearance===option.id} onclick={()=>setAppearance(option.id)}><Icon name={option.icon} size={22}/><strong>{option.name}</strong>{#if appearance===option.id}<Icon name="check" size={17}/>{/if}</button>{/each}</div></section>
          <section class="settings-section"><div class="section-heading"><h2>Account</h2><span class="status positive"><Icon name="check" size={14}/>Signed in</span></div><div class="setting-row"><div><strong>{account.email}</strong><p>Session stored securely in macOS Keychain.</p></div><div class="actions"><button class="button secondary" onclick={refresh} disabled={authBusy||!!busyKey}>Refresh session</button><button use:dialogTrigger class="button secondary" onclick={()=>{modal='signout';formError='';}} disabled={authBusy||!!busyKey}>Sign out</button></div></div><p class="footnote">Signing out pauses sync on this Mac. Your browser data and cloud profiles are kept.</p></section>
          <section class="settings-section"><h2>About Synchrome</h2><div class="setting-row"><div><strong>{version||'Synchrome 1.0.0'}</strong><p>Chrome stable &amp; Helium · macOS</p></div><span class="tag">Manual updates</span></div><p class="muted">To update, download the next release and replace the app in Applications. Your account, connections and data stay.</p><details class="privacy-details"><summary>What Synchrome synchronizes</summary><p>Bookmarks and extension inventory and enabled state. Extension settings and supported browser preferences are optional for each connection, and are read and written with the browser closed.</p><p>Passwords, cookies, sessions, browsing history, IndexedDB and local storage are outside this version’s sync scope. Extension settings can contain private values; review their disclosure before enabling sync.</p></details></section>
          <section class="settings-section"><details class="diagnostics"><summary>Advanced diagnostics</summary><div class="diagnostic-grid"><strong>Cloud service</strong>{@render status(cloudState(cloud))}<span>Endpoint</span><code>{cloud?.endpoint??'Unavailable'}</code>{#each [['Authentication',cloud?.auth],['Database',cloud?.rest],['Storage',cloud?.storage]] as [label,probe]}<span>{label}</span><span>{probeBadge(probe).label}</span>{/each}<span>App location</span><code>{location?.path??'Unknown'}</code><span>Companion folder</span><code>{extFolder??extFolderError??'Unavailable'}</code><span>Session expires</span><span>{fmtExpiry(account.expires_at)}</span></div>{#if !location?.stable}<div class="notice warning"><Icon name="info" size={18}/><p>{location?.reason??'Install Synchrome in Applications before setting up the connector.'}</p></div>{/if}<p class="muted small">Realtime, snapshot and cleanup diagnostics are available inside each local profile connection.</p><button class="button secondary" onclick={loadCloud} disabled={loadingCloud}>Check cloud service</button></details></section>
        {/if}
      </main>
    </div>
  </div>
{/if}
<div class="toast-region" aria-live="polite" aria-atomic="true">{#if toast}<div class="toast"><Icon name="check" size={18}/><span>{toast}</span><button class="icon-button" aria-label="Dismiss notification" onclick={()=>toast=''}><Icon name="close" size={16}/></button></div>{/if}</div>

<Dialog open={modal==='profile'} title={profileMethod==='choose'?'Create a Synchrome profile':profileMethod==='manual'?'Start a new profile':profileMethod==='import'?'Use a browser profile':modalMapping?'Change connected browser':'Connect a browser'} description={profileMethod==='choose'?'Choose a starting point. You can connect more Macs later.':profileMethod==='import'?'Bring an existing browser profile into Synchrome. Its first sync supplies the initial data.':profileMethod==='manual'?'Create a shared space, then choose a browser to connect.':'Connect one local browser profile to a Synchrome profile.'} onclose={closeModal} dismissible={!busyKey}>
  {#if profileMethod==='choose'}<div class="method-options"><button onclick={()=>profileMethod='import'}><span class="method-icon"><Icon name="download" size={23}/></span><span><strong>Use a browser profile</strong><small>Start with the bookmarks and extensions you already have.</small></span><Icon name="chevron-right" size={18}/></button><button onclick={()=>profileMethod='manual'}><span class="method-icon"><Icon name="plus" size={23}/></span><span><strong>Start fresh</strong><small>Create a profile and connect its first browser when you’re ready.</small></span><Icon name="chevron-right" size={18}/></button></div>
  {:else}<form class="profile-form" onsubmit={submitProfile}>
    {#if profileMethod==='connect'&&!modalProfile}<div class="field"><label for="target-profile">Synchrome profile</label><select id="target-profile" bind:value={selectedTarget}><option value="">Choose a profile</option>{#each profiles as p}<option value={p.id} disabled={sameDeviceConflict(mappings,p.id,modalMapping?.id)}>{p.name}{sameDeviceConflict(mappings,p.id,modalMapping?.id)?' — already connected on this Mac':''}</option>{/each}</select></div>{/if}
    {#if profileMethod!=='manual'}<div class="field"><label for="local-profile">Browser profile</label><select id="local-profile" value={selectedLocal} onchange={event=>pickLocal(event.currentTarget.value)}><option value="">Choose a browser profile</option>{#each localOptions as option}<option value={option.key} disabled={!!option.mapping&&option.mapping.id!==modalMapping?.id}>{option.b.display_name} — {option.p.display_name??option.p.dir_name}{option.mapping?' (connected)':''}</option>{/each}</select><span class="field-hint">Choose from the browser profiles detected on this Mac.</span></div>
      {#if !localOptions.length}<div class="notice info"><Icon name="browser" size={18}/><div><strong>No available browser profiles.</strong><p>Open Chrome stable or Helium once, then find browsers again.</p></div></div>{/if}
      {#if needsFda}<div class="notice info"><Icon name="shield" size={18}/><div><strong>Chrome may need macOS permission.</strong><p>Helium can be used without Full Disk Access.</p><button type="button" class="text-button" onclick={openFdaSettings}>Open Full Disk Access settings<Icon name="external" size={15}/></button></div></div>{/if}
      {#if pickedLocal?.b.access==='browser_running'}<div class="notice warning"><Icon name="info" size={18}/><p>Close {pickedLocal.b.display_name} before connecting. If it remains open, the link will wait until you retry.</p></div>{/if}
      {#if modalMapping}<div class="notice warning"><Icon name="info" size={18}/><p>The existing browser is disconnected first. If the new connection fails, reconnect it from this profile. Both browsers keep their local data.</p></div>{/if}
    {/if}
    {#if profileMethod!=='connect'}<div class="field"><label for="profile-name">Profile name</label><input id="profile-name" bind:value={formName} placeholder="e.g. Work" maxlength="80" autocomplete="off" aria-invalid={!!formError} aria-describedby={formError?'profile-error':undefined}/><span class="field-hint">A name you’ll recognize on every Mac.</span></div>{/if}
    {#if formError}<p id="profile-error" class="form-error" role="alert">{formError}</p>{/if}
    <div class="dialog-footer"><button type="button" class="button secondary" onclick={closeModal} disabled={!!busyKey}>Cancel</button><button type="submit" class="button primary" disabled={!!busyKey||cloudDataBusy}>{busyKey==='profile-form'?(profileMethod==='manual'?'Creating…':'Connecting…'):profileMethod==='manual'?'Create profile':profileMethod==='import'?'Import profile':'Connect browser'}<Icon name="arrow-right" size={17}/></button></div>
  </form>{/if}
</Dialog>
<Dialog open={modal==='delete'} title={`Delete ${modalProfile?.name??'profile'}?`} description="This removes the shared Synchrome profile and its cloud sync data." onclose={closeModal} dismissible={!busyKey}>
  <div class="delete-scope"><div><Icon name="cloud" size={19}/><div><strong>Removed from Synchrome</strong><p>The shared profile, its {modalProfile?mappingsOf(modalProfile.id).length:0} connections, and associated cloud snapshots and journals.</p></div></div><div><Icon name="shield" size={19}/><div><strong>Your browser data stays</strong><p>Local Chrome and Helium profiles, bookmarks and files stay on every Mac.</p></div></div></div>
  <div class="field"><label for="delete-name">Type <strong>{modalProfile?.name}</strong> to confirm</label><input id="delete-name" bind:value={deleteName} autocomplete="off" spellcheck="false"/></div>{#if formError}<p class="form-error" role="alert">{formError}</p>{/if}
  <div class="dialog-footer"><button class="button secondary" onclick={closeModal} disabled={!!busyKey}>Cancel</button><button class="button danger-button" onclick={confirmDelete} disabled={deleteName!==modalProfile?.name||!!busyKey}>{busyKey==='delete'?'Deleting…':'Delete profile'}</button></div>
</Dialog>
<Dialog open={modal==='disconnect'} title="Disconnect this browser?" description={`${modalMapping?.profile_name??'This profile'} will stop syncing with ${browserName(modalMapping?.browser_kind)} on ${modalMapping?.device_name??'this Mac'}.`} onclose={closeModal} dismissible={!busyKey}>
  <p class="muted">The shared Synchrome profile and its other connections stay. This browser’s local profile, bookmarks and files are kept.</p>{#if formError}<p class="form-error" role="alert">{formError}</p>{/if}<div class="dialog-footer"><button class="button secondary" onclick={closeModal} disabled={!!busyKey}>Cancel</button><button class="button primary" onclick={confirmDisconnect} disabled={!!busyKey}>{busyKey?'Disconnecting…':'Disconnect browser'}</button></div>
</Dialog>
<Dialog open={modal==='rename'} title="Rename profile" onclose={closeModal} dismissible={!busyKey}><form onsubmit={saveProfileName}><div class="field"><label for="rename-profile">Profile name</label><input id="rename-profile" bind:value={formName} maxlength="80"/></div>{#if formError}<p class="form-error" role="alert">{formError}</p>{/if}<div class="dialog-footer"><button type="button" class="button secondary" onclick={closeModal} disabled={!!busyKey}>Cancel</button><button class="button primary" disabled={!!busyKey}>{busyKey?'Saving…':'Save name'}</button></div></form></Dialog>
<Dialog open={modal==='signout'} title="Sign out of Synchrome?" description="Sync pauses on this Mac until you sign in again." onclose={closeModal} dismissible={!authBusy}><p class="muted">Your browser data and shared profiles are kept.</p>{#if authError}{@render errorNotice(authError)}{/if}<div class="dialog-footer"><button class="button secondary" onclick={closeModal} disabled={authBusy}>Cancel</button><button class="button primary" disabled={authBusy} onclick={async()=>{await signOut();if(!authError){modal=null;route='overview';}}}>{authBusy?'Signing out…':'Sign out'}</button></div></Dialog>
<Dialog open={onboarding&&signedIn&&!modal} title={profiles.length?"Bring your profiles to this Mac.":"Bring your browsing together."} description={profiles.length?"Your shared profiles are already here. Choose one and connect a browser on this Mac.":"Start with one browser profile. We’ll guide you through connecting it, then confirm its first sync."} onclose={()=>{onboarding=false;onboardingDismissed=true;}}>
  <div class="welcome-flow"><span><Icon name="browser" size={25}/><strong>Your browser</strong></span><Icon name="arrow-right" size={20}/><span class="welcome-brand"><Icon name="logo" size={27}/><strong>Synchrome profile</strong></span><Icon name="arrow-right" size={20}/><span><Icon name="laptop" size={26}/><strong>Other Macs</strong></span></div><p class="muted">Synchrome keeps bookmarks and extensions together. Optional settings stay off until you choose them.</p><div class="dialog-footer"><button class="button quiet" onclick={()=>{onboarding=false;onboardingDismissed=true;}}>Explore workspace</button><button use:dialogTrigger class="button primary" onclick={()=>{onboarding=false;onboardingDismissed=true;if(profiles.length)openConnect(null);else openCreate('import');}}>{profiles.length?'Connect a profile':'Choose a browser'}<Icon name="arrow-right" size={16}/></button></div>
</Dialog>
<Dialog open={modal==='setup'} title={setupComplete?'Your first sync is complete.':'Finish connecting your browser'} description={setupComplete?`${setupMapping?.profile_name??'Your profile'} is ready on this Mac.`:'A connection becomes active when the companion confirms it and sends its first data.'} onclose={closeModal} dismissible={!busyKey}>
  {#if setupComplete}<div class="setup-success"><span><Icon name="check" size={29}/></span><h3>Your browsing has a new home.</h3><p>{setupMapping?.bookmarks?.bookmarks??0} bookmarks in {setupMapping?.profile_name}. Sign in on another Mac and connect a browser to the same profile.</p></div><button class="button primary full-width" onclick={closeModal}>View profile<Icon name="arrow-right" size={17}/></button>
  {:else if setupMapping}
    <div class="setup-context"><Icon name="browser" size={22}/><strong>{browserName(setupMapping.browser_kind)} / {localName(setupMapping)}</strong><Icon name="arrow-right" size={17}/><span>{setupMapping.profile_name}</span></div>
    {#if setupMapping.display_status==='pending_local_write'||setupMapping.display_status==='error'}<div class="setup-step"><span class="step-number">1</span><div><h3>Close the browser and write its connection.</h3><p>The link is stored only in the Synchrome companion’s own browser storage.</p><button class="button primary" onclick={()=>retryWrite(setupMapping)} disabled={!!busyKey}>Retry link</button></div></div>
    {:else if setupMapping.display_status!=='extension_confirmed'}
      <div class="setup-step"><span class="step-number">1</span><div><h3>Install the browser connector.</h3><p>Do this once for {browserName(setupMapping.browser_kind)}, then restart the browser.</p>{#if !location?.stable}<p class="form-error">{location?.reason??'Move Synchrome to Applications first.'}</p>{/if}<button class="button secondary" onclick={()=>installConnector(setupBrowser)} disabled={!setupBrowser||!!busyKey||!location?.stable}>Install connector</button>{#if connectorMsg[setupMapping.browser_kind]}<p class="small" role="status">{connectorMsg[setupMapping.browser_kind]}</p>{/if}</div></div>
      <div class="setup-step"><span class="step-number">2</span><div><h3>Load the Synchrome companion.</h3><p>In this browser, open <code>chrome://extensions</code>, turn on Developer mode, then choose Load unpacked and select the companion folder. Open its popup once to confirm the connection.</p><button class="button secondary" onclick={revealExtension} disabled={!extFolder}>Show companion folder<Icon name="external" size={15}/></button></div></div>
      <div class="setup-step"><span class="step-number">3</span><div><h3>Confirm your first sync.</h3><p>After the companion connects, Synchrome receives your browser data. This step stays pending until an actual sync completes.</p><button class="button primary" onclick={()=>checkExtension(setupMapping)} disabled={!!busyKey}>Check connection<Icon name="refresh" size={16}/></button></div></div>
    {:else}<div class="setup-step"><span class="step-number"><Icon name="check" size={18}/></span><div><h3>Companion connected. Waiting for first sync.</h3><p>Open the browser and its companion to send the first bookmarks. You can check again or sync now.</p><button class="button primary" onclick={()=>uploadNow(setupMapping)} disabled={!!busyKey}>Sync now</button><button class="button quiet" onclick={()=>checkExtension(setupMapping)} disabled={!!busyKey}>Check connection</button></div></div>{/if}
    {#if actionError}{@render errorNotice(actionError)}{/if}<div class="dialog-footer"><button class="button quiet" onclick={closeModal} disabled={!!busyKey}>Continue later</button></div>
  {:else}<p class="muted">This connection is no longer available. Refresh your profiles to see its current state.</p><button class="button secondary" onclick={closeModal}>Close</button>{/if}
</Dialog>
