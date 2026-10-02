<script>
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";

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

  // sign-in form (values go straight to Rust; nothing is stored here)
  let email = $state("");
  let password = $state("");
  let authBusy = $state(false);
  let authError = $state(null);

  // cloud profile editing
  let newProfileName = $state("");
  let renamingId = $state(null);
  let renameValue = $state("");
  // per local profile: chosen existing cloud profile for "Map to existing"
  let mapChoice = $state({});

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
  let extOpen = $state({});
  let extList = $state({});
  let extConfirm = $state(null);
  let extMsg = $state({});
  async function loadExtensions(m) {
    try { extList = { ...extList, [m.id]: await invoke("list_extensions", { mappingId: m.id }) }; }
    catch (e) { actionError = String(e); }
  }
  async function toggleExtensions(m) {
    const open = !extOpen[m.id];
    extOpen = { ...extOpen, [m.id]: open };
    if (open) await loadExtensions(m);
  }
  async function extAct(m, x, action) {
    const key = `${x.ext_id}:${action}`;
    if (action === "remove" && extConfirm !== key) { extConfirm = key; return; }
    extConfirm = null; busyKey = `ext:${x.ext_id}`; actionError = null;
    try {
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
    try {
      conflictList = { ...conflictList, [m.id]: await invoke("list_conflicts", { mappingId: m.id, includeClosed: true }) };
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
    confirmKey = null; actionError = null; busyKey = `resolve:${c.conflict_id}`;
    try {
      const [r] = await invoke("resolve_conflicts", { mappingId: m.id, items: [{ conflict_id: c.conflict_id, resolution }] });
      const what = RES_DONE[resolution] ?? resolution;
      const msg = r.outcome === "resolved" ? `Resolved — ${what}.`
        : r.outcome === "resolving" ? `Decision saved — ${what}; the browser is updated on the next sync.`
        : r.outcome === "already_resolved" ? "Already resolved."
        : r.outcome === "closed" ? `No longer open (${r.status}).`
        : `Could not resolve: ${r.error}`;
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
    try { account = await invoke("account_status"); } catch (e) { error = String(e); }
  }
  async function loadDevice() {
    try { device = await invoke("device_identity"); } catch { device = null; }
  }
  /// Register this device, then fetch devices, profiles and mappings. Only when signed in.
  async function loadCloudData() {
    if (!account?.signed_in) { devices = []; profiles = []; mappings = []; return; }
    cloudDataBusy = true;
    cloudDataError = null;
    try {
      await invoke("register_device");
      [devices, profiles, mappings] = await Promise.all([
        invoke("list_devices"), invoke("list_profiles"), invoke("list_mappings"),
      ]);
      // open panels show fresh data after every reload
      for (const m of mappings) {
        if (conflictsOpen[m.id]) await loadConflicts(m);
        if (extOpen[m.id]) await loadExtensions(m);
      }
    } catch (e) {
      cloudDataError = String(e);
    } finally {
      cloudDataBusy = false;
    }
  }
  async function loadAll() {
    error = null;
    version = await invoke("app_version").catch(() => "");
    await Promise.all([loadBrowsers(), loadCloud(), loadAccount(), loadDevice()]);
    await loadCloudData();
  }
  onMount(loadAll);

  // ---- auth ----
  async function signIn(ev) {
    ev.preventDefault();
    authError = null; authBusy = true;
    try {
      account = await invoke("sign_in", { email, password });
      password = "";
      await loadCloudData();
    } catch (e) { authError = String(e); }
    finally { authBusy = false; }
  }
  async function signOut() {
    authBusy = true; authError = null;
    try {
      account = await invoke("sign_out");
      devices = []; profiles = []; mappings = [];
    } catch (e) { authError = String(e); }
    finally { authBusy = false; }
  }
  async function refresh() {
    authBusy = true; authError = null;
    try { account = await invoke("refresh_session"); } catch (e) { authError = String(e); }
    finally { authBusy = false; }
  }

  // ---- generic mutation wrapper: run, then reload cloud data ----
  async function act(key, fn) {
    actionError = null; busyKey = key;
    try { await fn(); await loadCloudData(); }
    catch (e) { actionError = String(e); }
    finally { busyKey = null; }
  }

  // ---- cloud profiles ----
  const createProfile = () => act("create", async () => {
    await invoke("create_profile", { name: newProfileName });
    newProfileName = "";
  });
  const startRename = (p) => { renamingId = p.id; renameValue = p.name; };
  const saveRename = (p) => act(`rename:${p.id}`, async () => {
    await invoke("rename_profile", { profileId: p.id, name: renameValue });
    renamingId = null;
  });
  const deleteProfile = (p) => act(`delete:${p.id}`, () =>
    invoke("delete_profile", { profileId: p.id }));

  // ---- local <-> cloud mapping ----
  const localKey = (b, p) => `${b.kind}/${p.dir_name}`;
  const mappingFor = (b, p) =>
    mappings.find((m) => m.is_this_device && m.browser_kind === b.kind && m.browser_profile_dir === p.dir_name);
  const mappingsOf = (profileId) => mappings.filter((m) => m.profile_id === profileId);
  // A cloud profile may map to several local profiles (even on this device), so all are offered.
  const mappableProfiles = $derived(profiles);

  const createAndMap = (b, p) => act(`map:${localKey(b, p)}`, () =>
    invoke("create_and_map_local_profile", {
      name: p.display_name ?? p.dir_name, browserKind: b.kind, browserProfileDir: p.dir_name,
    }));
  const mapExisting = (b, p) => {
    const profileId = mapChoice[localKey(b, p)];
    if (!profileId) return;
    return act(`map:${localKey(b, p)}`, () =>
      invoke("map_local_profile", { profileId, browserKind: b.kind, browserProfileDir: p.dir_name }));
  };
  const unmap = (m) => act(`unmap:${m.id}`, () => invoke("unmap_local_profile", { mappingId: m.id }));
  const retryWrite = (m) => act(`write:${m.id}`, () => invoke("write_mapping_token_local", { mappingId: m.id }));
  const checkExtension = (m) => act(`check:${m.id}`, async () => {});
  const uploadNow = (m) => act(`upload:${m.id}`, () => invoke("publish_bookmarks", { mappingId: m.id }));
  async function installConnector(b) {
    connectorMsg = { ...connectorMsg, [b.kind]: "Installing…" };
    try {
      await invoke("install_native_host", { browserKind: b.kind });
      connectorMsg = { ...connectorMsg, [b.kind]: "Connector installed. Restart the browser." };
    } catch (e) {
      connectorMsg = { ...connectorMsg, [b.kind]: `Connector install failed: ${e}` };
    }
  }

  // ---- derived ----
  const signedIn = $derived(!!account?.signed_in);
  const needsFda = $derived(browsers.some((b) => b.access === "requires_full_disk_access"));
  const totalProfiles = $derived(browsers.reduce((n, b) => n + b.profiles.length, 0));
  // this device's mappings whose local profile is not among the detected ones
  const unmatched = $derived(mappings.filter((m) => m.is_this_device
    && !browsers.some((b) => b.kind === m.browser_kind && b.profiles.some((p) => p.dir_name === m.browser_profile_dir))));
  const cloudReady = $derived(!!cloud && cloud.configured && cloud.auth_ok);

  function cloudSummary(c) {
    if (!c) return { label: "Checking…", cls: "muted" };
    if (!c.configured) return { label: "Not configured", cls: "warn" };
    if (!c.reachable) return { label: "Unreachable", cls: "bad" };
    if (c.auth_ok && c.rest_ok && c.storage_ok) return { label: "Connected", cls: "ok" };
    return { label: "Partially reachable", cls: "warn" };
  }
  function probeBadge(p) {
    if (!p) return { label: "—", cls: "muted" };
    if (p.outcome === "unreachable") return { label: "unreachable", cls: "bad" };
    return { label: `${p.status ?? "?"} · ${p.latency_ms} ms`, cls: p.outcome === "ok" ? "ok" : "warn" };
  }
  const fmtExpiry = (ts) => (ts ? new Date(ts * 1000).toLocaleString() : "");
</script>

{#snippet mappingBlock(m)}
  <span class="arrow">→</span>
  <strong>{m.profile_name ?? "(unknown profile)"}</strong>
  <span class="badge small {MAP_STATUS[m.display_status]?.cls ?? 'muted'}">{MAP_STATUS[m.display_status]?.label ?? m.display_status}</span>
  <span class="pdir">{m.device_name ?? "this device"}</span>
  {#if m.display_status !== "extension_confirmed"}
    <button class="tiny-btn" onclick={() => retryWrite(m)} disabled={busyKey === `write:${m.id}`}
      title="Writes the link into this browser profile. The browser must be closed.">Retry local write</button>
  {/if}
  <button class="tiny-btn" onclick={() => checkExtension(m)} disabled={busyKey === `check:${m.id}`}
    title="Re-reads the link status. The extension reconnects when the browser starts, or from its popup.">Check extension</button>
  <button class="tiny-btn" onclick={() => unmap(m)} disabled={busyKey === `unmap:${m.id}`}>Unmap</button>
  {#if m.local_error}<span class="small bad">{m.local_error}</span>{/if}
  {#if m.display_status === "extension_confirmed"}
    {@const bm = m.bookmarks ?? { state: "not_synced", bookmarks: 0, folders: 0, pending: 0 }}
    <div class="bookmarks-row">
      <span class="small">Bookmarks</span>
      <span class="badge small {BM_STATUS[bm.state]?.cls ?? 'muted'}">{BM_STATUS[bm.state]?.label ?? bm.state}</span>
      <span class="pdir">{bm.bookmarks} bookmarks · {bm.folders} folders</span>
      <span class="pdir">last upload {when(bm.last_upload_at)}</span>
      <span class="pdir">{bm.pending} pending</span>
      {#if bm.pending > 0}
        <button class="tiny-btn" onclick={() => uploadNow(m)} disabled={busyKey === `upload:${m.id}`}
          title="Downloads remote changes and uploads the queued local ones now.">Sync now</button>
      {/if}
      {#if bm.error}<span class="small bad">{bm.error}</span>{/if}
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
        {#if bm.download.error}<span class="small bad">{bm.download.error}</span>{/if}
      </div>
    {/if}
    {#if conflictsOpen[m.id]}
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
              <strong>{c.item_kind === "folder" ? "Folder" : "Bookmark"} “{c.name}”</strong>
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
                  <button class="tiny-btn {o.destructive ? 'danger' : ''}" onclick={() => choose(m, c, o)} disabled={busyKey === `resolve:${c.conflict_id}`}>
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
                  <span class="pname">{c.item_kind === "folder" ? "Folder" : c.item_kind === "extension" ? "Extension" : "Bookmark"} “{c.name}”</span>
                  <span class="pdir">{c.status === "superseded" ? (c.detail ?? "no longer needed") : RES_DONE[c.resolution] ?? c.resolution}{c.resolution_ops ? ` · ${c.resolution_ops} change${c.resolution_ops === 1 ? "" : "s"} published${c.upload_pending ? " (uploading)" : ""}` : ""}</span>
                  <span class="pdir">{when(c.resolved_at)}</span>
                  {#if resolvedMsg[c.conflict_id]}<span class="ok small">{resolvedMsg[c.conflict_id]}</span>{/if}
                </li>
              {/each}
            </ul>
          {/if}
        {/if}
      </div>
    {/if}
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
                  {#if x.error}<span class="small bad"> {x.error}</span>{/if}
                </td>
                <td>
                  {#each x.actions as a}
                    {@const key = `${x.ext_id}:${a}`}
                    <button class="tiny-btn {a === 'remove' ? 'danger' : ''}" disabled={busyKey === `ext:${x.ext_id}`} onclick={() => extAct(m, x, a)}>
                      {extConfirm === key ? "Confirm: remove from this browser" : X_ACTION[a] ?? a}
                    </button>
                  {/each}
                  {#if x.state === "conflict"}
                    <button class="tiny-btn" onclick={() => { if (!conflictsOpen[m.id]) toggleConflicts(m); }}>Resolve conflict</button>
                  {/if}
                  {#if extConfirm?.startsWith(x.ext_id)}<button class="tiny-btn" onclick={() => (extConfirm = null)}>Cancel</button>{/if}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
        <p class="hint tiny">Installing opens the official store page; removing asks for confirmation. Nothing is installed or uninstalled automatically.</p>
      </div>
    {/if}
  {/if}
{/snippet}

<main>
  <header>
    <h1>Synchrome</h1>
    <span class="ua">{version}</span>
    <button class="right" onclick={loadAll} disabled={loadingBrowsers || loadingCloud || cloudDataBusy}>
      {loadingBrowsers || loadingCloud || cloudDataBusy ? "Refreshing…" : "Refresh"}
    </button>
  </header>

  {#if error}<div class="error">Error: {error}</div>{/if}
  {#if actionError}<div class="error">{actionError} <button class="tiny-btn" onclick={() => (actionError = null)}>Dismiss</button></div>{/if}

  <!-- ===== Account ===== -->
  <section class="card">
    <div class="card-head">
      <h2>Account</h2>
      <span class="badge {signedIn ? 'ok' : 'muted'}">{signedIn ? "Signed in" : "Signed out"}</span>
    </div>

    {#if signedIn}
      <div class="kv"><span>Signed in as</span><strong>{account.email ?? account.user_id}</strong></div>
      <div class="kv"><span>Session expires</span><span>{fmtExpiry(account.expires_at)}{account.needs_refresh ? " · needs refresh" : ""}</span></div>
      <div class="actions row">
        <button onclick={refresh} disabled={authBusy}>Refresh session</button>
        <button class="danger" onclick={signOut} disabled={authBusy}>{authBusy ? "…" : "Sign out"}</button>
      </div>
      {#if account.error}<p class="warn small">{account.error}</p>{/if}

      <!-- Devices -->
      <div class="sub">
        <div class="sub-head"><h3>Devices</h3>
          <button class="tiny-btn" onclick={loadCloudData} disabled={cloudDataBusy}>{cloudDataBusy ? "…" : "Reload"}</button>
        </div>
        {#if cloudDataError}
          <p class="warn small">{cloudDataError}</p>
        {:else if devices.length === 0}
          <p class="muted small">{cloudDataBusy ? "Registering this device…" : "No devices registered yet."}</p>
        {:else}
          <ul class="plain">
            {#each devices as d (d.device_id)}
              <li class="row-item">
                <span class="pname">{d.name}</span>
                <span class="pdir">{d.platform}{d.os_version ? ` · ${d.os_version}` : ""} · v{d.app_version}</span>
                {#if d.is_current}<span class="badge small ok">This device</span>{/if}
              </li>
            {/each}
          </ul>
        {/if}
      </div>

      <!-- Cloud profiles -->
      <div class="sub">
        <div class="sub-head"><h3>Synchrome profiles</h3><span class="badge muted small">{profiles.length}</span></div>
        <form class="inline" onsubmit={(e) => { e.preventDefault(); createProfile(); }}>
          <input placeholder="New profile name" bind:value={newProfileName} maxlength="200" disabled={busyKey === "create"} />
          <button type="submit" disabled={!newProfileName.trim() || busyKey === "create"}>{busyKey === "create" ? "Creating…" : "Create profile"}</button>
        </form>
        {#if profiles.length === 0}
          <p class="muted small">No Synchrome profiles yet. Create one here or from a browser profile below.</p>
        {:else}
          <ul class="plain">
            {#each profiles as p (p.id)}
              {@const used = mappingsOf(p.id)}
              <li class="row-item">
                {#if renamingId === p.id}
                  <input class="grow" bind:value={renameValue} maxlength="200" />
                  <button class="tiny-btn" onclick={() => saveRename(p)} disabled={!renameValue.trim() || busyKey === `rename:${p.id}`}>Save</button>
                  <button class="tiny-btn" onclick={() => (renamingId = null)}>Cancel</button>
                {:else}
                  <span class="pname">{p.name}</span>
                  <span class="pdir">{used.length === 0 ? "not mapped" : `mapped on ${used.length} device${used.length > 1 ? "s" : ""}`}</span>
                  <button class="tiny-btn" onclick={() => startRename(p)}>Rename</button>
                  <button class="tiny-btn danger" onclick={() => deleteProfile(p)}
                    disabled={used.length > 0 || busyKey === `delete:${p.id}`}
                    title={used.length > 0 ? "Unmap it on every device first" : "Delete this Synchrome profile"}>Delete</button>
                {/if}
              </li>
            {/each}
          </ul>
        {/if}
      </div>
    {:else}
      <form class="login" onsubmit={signIn}>
        <label>Email
          <input type="email" bind:value={email} autocomplete="username" required disabled={authBusy || !cloudReady} />
        </label>
        <label>Password
          <input type="password" bind:value={password} autocomplete="current-password" required disabled={authBusy || !cloudReady} />
        </label>
        <button type="submit" disabled={authBusy || !cloudReady || !email || !password}>{authBusy ? "Signing in…" : "Sign in"}</button>
      </form>
      {#if !cloudReady}<p class="hint">Sign-in is enabled once Synchrome Cloud is configured and its Auth service is reachable.</p>{/if}
      {#if account?.error}<p class="warn small">{account.error}</p>{/if}
    {/if}
    {#if authError}<p class="bad small">{authError}</p>{/if}
    {#if device}
      <p class="hint tiny">This device: <strong>{device.device_name}</strong> · {device.platform}{device.os_version ? ` · ${device.os_version}` : ""} · id {device.device_id.slice(0, 8)}…</p>
    {/if}
    <p class="hint tiny">Your session is stored only in the macOS Keychain. Tokens never reach this window.</p>
  </section>

  <!-- ===== Cloud ===== -->
  <section class="card">
    <div class="card-head">
      <h2>Synchrome Cloud</h2>
      {#if cloud}
        {@const s = cloudSummary(cloud)}
        <span class="badge {s.cls}">{s.label}</span>
      {:else}
        <span class="badge muted">Checking…</span>
      {/if}
    </div>
    {#if loadingCloud && !cloud}
      <div class="muted">Probing endpoint…</div>
    {:else if cloud && !cloud.configured}
      <p class="warn">{cloud.error}</p>
      <p class="hint">Add <code>SUPABASE_URL</code> and <code>SUPABASE_ANON_KEY</code> to a <code>.env</code> file (see <code>.env.example</code>). Only publishable values.</p>
    {:else if cloud}
      {@const a = probeBadge(cloud.auth)}
      {@const r = probeBadge(cloud.rest)}
      {@const st = probeBadge(cloud.storage)}
      <div class="kv"><span>Endpoint</span><code>{cloud.endpoint}</code></div>
      <div class="services">
        <div class="svc"><span>Auth</span><span class="badge small {a.cls}">{a.label}</span></div>
        <div class="svc"><span>PostgREST</span><span class="badge small {r.cls}">{r.label}</span></div>
        <div class="svc"><span>Storage</span><span class="badge small {st.cls}">{st.label}</span></div>
      </div>
      {#if cloud.error}<p class="bad">{cloud.error}</p>{/if}
    {/if}
  </section>

  <!-- ===== Browsers ===== -->
  <section class="card">
    <div class="card-head">
      <h2>Browsers on this device</h2>
      <span class="badge muted">{browsers.length} browsers · {totalProfiles} profiles</span>
    </div>
    <p class="lead">Mapping only links profiles in your account. Nothing is written into the browser yet.</p>
    {#if needsFda}
      <div class="notice warn">Some browsers (Chrome, Brave, Edge) are protected by macOS. Synchrome will need <strong>Full Disk Access</strong> to read their profiles. Not requested yet.</div>
    {/if}
    {#if loadingBrowsers}
      <div class="muted">Scanning…</div>
    {:else if browsers.length === 0}
      <div class="muted">No Chromium-family browsers found.</div>
    {:else}
      <ul class="browsers">
        {#each browsers as b (b.user_data_dir)}
          <li class="browser">
            <div class="browser-head">
              <span class="name">{b.display_name}</span>
              {#if b.version}<span class="ver">v{b.version}</span>{/if}
              <span class="badge {ACCESS[b.access]?.cls ?? 'muted'}">{ACCESS[b.access]?.label ?? b.access}</span>
            </div>
            <div class="path">{b.user_data_dir}</div>
            <div class="connector">
              <button class="tiny-btn" onclick={() => installConnector(b)}
                title="Registers Synchrome's native messaging connector for this browser (not a profile write).">Install connector</button>
              {#if connectorMsg[b.kind]}<span class="pdir">{connectorMsg[b.kind]}</span>{/if}
            </div>
            {#if b.profiles.length > 0}
              <ul class="profiles">
                {#each b.profiles as p (p.dir_name)}
                  {@const m = mappingFor(b, p)}
                  {@const key = localKey(b, p)}
                  <li class="profile">
                    <div class="profile-main">
                      <span class="pname">{p.display_name ?? p.dir_name}</span>
                      <span class="pdir">{p.dir_name}</span>
                      <span class="badge small {ACCESS[p.access]?.cls ?? 'muted'}">{ACCESS[p.access]?.label ?? p.access}</span>
                    </div>
                    {#if signedIn}
                      <div class="profile-map">
                        {#if m}
                          {@render mappingBlock(m)}
                        {:else}
                          <button class="tiny-btn" onclick={() => createAndMap(b, p)} disabled={busyKey === `map:${key}`}>
                            {busyKey === `map:${key}` ? "…" : "Create Synchrome profile"}
                          </button>
                          {#if mappableProfiles.length > 0}
                            <select bind:value={mapChoice[key]} disabled={busyKey === `map:${key}`}>
                              <option value={undefined}>Map to existing…</option>
                              {#each mappableProfiles as cp (cp.id)}<option value={cp.id}>{cp.name}</option>{/each}
                            </select>
                            <button class="tiny-btn" onclick={() => mapExisting(b, p)} disabled={!mapChoice[key] || busyKey === `map:${key}`}>Map</button>
                          {/if}
                        {/if}
                      </div>
                    {/if}
                  </li>
                {/each}
              </ul>
            {:else if b.access === "requires_full_disk_access"}
              <div class="profiles-note warn">Profiles are hidden until Full Disk Access is granted.</div>
            {:else}
              <div class="profiles-note muted">No profiles found.</div>
            {/if}
          </li>
        {/each}
      </ul>
    {/if}
  </section>

  {#if signedIn && unmatched.length > 0}
    <section class="card">
      <div class="card-head">
        <h2>Mapped profiles not detected here</h2>
        <span class="badge muted">{unmatched.length}</span>
      </div>
      <p class="lead">Mappings of this device whose browser profile is not in the list above (e.g. a separate user-data directory). Their sync state and conflicts are still shown.</p>
      <ul class="plain">
        {#each unmatched as m (m.id)}
          <li class="profile">
            <div class="profile-main"><span class="pname">{m.browser_kind} · {m.browser_profile_dir}</span></div>
            <div class="profile-map">{@render mappingBlock(m)}</div>
          </li>
        {/each}
      </ul>
    </section>
  {/if}
</main>

<style>
  :global(body) { margin: 0; background: #0f1720; color: #e6edf3;
    font: 14px/1.5 -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }
  main { max-width: 900px; margin: 0 auto; padding: 22px 20px 48px; }
  header { display: flex; align-items: baseline; gap: 12px; margin-bottom: 14px; }
  h1 { font-size: 22px; margin: 0; color: #5ac8be; }
  h2 { font-size: 15px; margin: 0; }
  h3 { font-size: 12px; margin: 0; color: #9aa7b4; text-transform: uppercase; letter-spacing: .04em; }
  .ua { color: #6b7988; font-size: 12px; }
  .right { margin-left: auto; }
  .lead { color: #9aa7b4; margin: 6px 0 12px; font-size: 13px; }
  .hint { color: #9aa7b4; font-size: 13px; margin: 8px 0 0; }
  .tiny { font-size: 11px; color: #6b7988; }
  .small { font-size: 13px; margin: 6px 0 0; }
  code { background: #1b2733; padding: 1px 5px; border-radius: 4px; font-size: 12px; }
  button, select, input { font: inherit; }
  button { background: #1b2733; color: #e6edf3; border: 1px solid #2b3a49; border-radius: 7px; padding: 7px 14px; cursor: pointer; }
  button:disabled { opacity: 0.5; cursor: default; }
  button.danger { border-color: #8a2a2a; color: #f08a8a; }
  .tiny-btn { padding: 2px 9px; font-size: 12px; }
  input, select { background: #0f1720; color: #e6edf3; border: 1px solid #2b3a49; border-radius: 7px; padding: 6px 9px; font-size: 13px; }
  .card { background: #141f2a; border: 1px solid #22303d; border-radius: 10px; padding: 14px 16px; margin-bottom: 14px; }
  .card-head { display: flex; align-items: center; gap: 10px; margin-bottom: 8px; }
  .kv { display: flex; gap: 12px; font-size: 13px; padding: 3px 0; align-items: baseline; }
  .kv > span:first-child { color: #6b7988; min-width: 110px; }
  .login { display: grid; gap: 10px; max-width: 360px; margin-top: 6px; }
  .login label { display: grid; gap: 4px; font-size: 12px; color: #9aa7b4; }
  .inline { display: flex; gap: 8px; margin: 6px 0 8px; }
  .inline input { flex: 1; }
  .grow { flex: 1; }
  .services { display: grid; grid-template-columns: repeat(3, 1fr); gap: 8px; margin: 8px 0; }
  .svc { display: flex; justify-content: space-between; align-items: center; background: #0f1720; border: 1px solid #22303d; border-radius: 8px; padding: 6px 10px; font-size: 13px; }
  .actions { margin-top: 10px; } .actions.row { display: flex; gap: 8px; }
  .notice { font-size: 13px; padding: 8px 10px; border-radius: 8px; margin-bottom: 10px; background: #3a2a10; border: 1px solid #8a6320; }
  .sub { margin-top: 12px; padding-top: 10px; border-top: 1px solid #22303d; }
  .sub-head { display: flex; align-items: center; gap: 10px; margin-bottom: 4px; }
  .sub-head .tiny-btn { margin-left: auto; }
  .plain, .browsers, .profiles { list-style: none; padding: 0; margin: 0; }
  .row-item { display: flex; align-items: center; gap: 8px; padding: 6px 0; border-bottom: 1px solid #1b2733; font-size: 13px; }
  .browser { background: #0f1720; border: 1px solid #22303d; border-radius: 10px; padding: 12px 14px; margin-bottom: 10px; }
  .browser-head { display: flex; align-items: center; gap: 10px; }
  .name { font-weight: 600; font-size: 15px; }
  .ver { color: #6b7988; font-size: 12px; }
  .path { color: #6b7988; font-size: 12px; margin: 4px 0 8px; word-break: break-all; }
  .profiles { border-top: 1px solid #22303d; }
  .profile { padding: 7px 0; border-bottom: 1px solid #1b2733; }
  .profile-main { display: flex; align-items: center; gap: 10px; }
  .profile-map { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; margin-top: 6px; padding-left: 12px; font-size: 13px; }
  .arrow { color: #5ac8be; }
  .connector { display: flex; gap: 8px; align-items: center; margin: -2px 0 8px; }
  .pname { flex: 1; }
  .pdir { color: #6b7988; font-size: 12px; }
  .profiles-note { font-size: 12px; padding-top: 8px; }
  .badge { margin-left: auto; font-size: 11px; padding: 2px 8px; border-radius: 999px; border: 1px solid transparent; white-space: nowrap; }
  .badge.small { font-size: 10px; margin-left: 0; }
  .badge.ok, .ok { color: #5fd08a; } .badge.ok { background: #10301f; border-color: #1c7a44; }
  .badge.warn, .warn { color: #e0a94a; } .badge.warn { background: #3a2a10; border-color: #8a6320; }
  .badge.info { background: #10263a; color: #5aa6e0; border-color: #1c5a8a; }
  .badge.bad, .bad { color: #f08a8a; } .badge.bad { background: #3a1414; border-color: #8a2a2a; }
  .badge.muted, .muted { color: #8a97a4; } .badge.muted { background: #1b2733; border-color: #2b3a49; }
  .error { background: #3a1414; color: #f0a0a0; padding: 10px 12px; border-radius: 8px; margin-bottom: 12px; display: flex; gap: 10px; align-items: center; }
  .bookmarks-row { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; width: 100%; margin-top: 4px; }
  .conflicts { width: 100%; margin-top: 6px; display: grid; gap: 8px; }
  .conflict { background: #141f2a; border: 1px solid #3a2a2a; border-radius: 8px; padding: 10px 12px; }
  .conflict-head { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .conflict-actions { display: flex; flex-wrap: wrap; gap: 6px; align-items: center; margin-top: 8px; }
  .cmp { width: 100%; border-collapse: collapse; margin-top: 6px; font-size: 12px; table-layout: fixed; }
  .cmp th { text-align: left; color: #9aa7b4; font-weight: 600; padding: 3px 6px; border-bottom: 1px solid #22303d; }
  .cmp td { padding: 3px 6px; border-bottom: 1px solid #1b2733; word-break: break-all; vertical-align: top; }
  .cmp td:first-child { color: #6b7988; width: 70px; }
  .cmp tr.diff td:not(:first-child) { color: #e0a94a; }
  .info { color: #5aa6e0; }
</style>
