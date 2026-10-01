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
</style>
