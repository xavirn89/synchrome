<script>
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";

  let browsers = $state([]);
  let cloud = $state(null);
  let account = $state(null);
  let version = $state("");
  let loadingBrowsers = $state(true);
  let loadingCloud = $state(true);
  let error = $state(null);

  // Sign-in form (never stores anything; values go straight to the Rust command).
  let email = $state("");
  let password = $state("");
  let authBusy = $state(false);
  let authError = $state(null);

  const ACCESS = {
    accessible: { label: "Accessible", cls: "ok" },
    requires_full_disk_access: { label: "Requires Full Disk Access", cls: "warn" },
    browser_running: { label: "Browser running", cls: "info" },
    unknown: { label: "Unknown", cls: "muted" },
  };

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
  async function loadAll() {
    error = null;
    version = await invoke("app_version").catch(() => "");
    await Promise.all([loadBrowsers(), loadCloud(), loadAccount()]);
  }
  onMount(loadAll);

  async function signIn(ev) {
    ev.preventDefault();
    authError = null;
    authBusy = true;
    try {
      account = await invoke("sign_in", { email, password });
      password = "";
    } catch (e) {
      authError = String(e);
    } finally {
      authBusy = false;
    }
  }
  async function signOut() {
    authBusy = true;
    authError = null;
    try { account = await invoke("sign_out"); } catch (e) { authError = String(e); }
    finally { authBusy = false; }
  }
  async function refresh() {
    authBusy = true;
    authError = null;
    try { account = await invoke("refresh_session"); } catch (e) { authError = String(e); }
    finally { authBusy = false; }
  }

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
  function fmtExpiry(ts) {
    if (!ts) return "";
    return new Date(ts * 1000).toLocaleString();
  }
</script>

<main>
  <header>
    <h1>Synchrome</h1>
    <span class="ua">{version}</span>
    <button class="right" onclick={loadAll} disabled={loadingBrowsers || loadingCloud}>
      {loadingBrowsers || loadingCloud ? "Refreshing…" : "Refresh"}
    </button>
  </header>

  {#if error}<div class="error">Error: {error}</div>{/if}

  <!-- ===== Account ===== -->
  <section class="card">
    <div class="card-head">
      <h2>Account</h2>
      {#if account?.signed_in}
        <span class="badge ok">Signed in</span>
      {:else}
        <span class="badge muted">Signed out</span>
      {/if}
    </div>

    {#if account?.signed_in}
      <div class="kv"><span>Signed in as</span><strong>{account.email ?? account.user_id}</strong></div>
      <div class="kv"><span>Session expires</span><span>{fmtExpiry(account.expires_at)}{account.needs_refresh ? " · needs refresh" : ""}</span></div>
      <div class="actions row">
        <button onclick={refresh} disabled={authBusy}>Refresh session</button>
        <button class="danger" onclick={signOut} disabled={authBusy}>{authBusy ? "…" : "Sign out"}</button>
      </div>
      {#if account.error}<p class="warn small">{account.error}</p>{/if}
    {:else}
      <form class="login" onsubmit={signIn}>
        <label>Email
          <input type="email" bind:value={email} autocomplete="username" required disabled={authBusy || !cloudReady} />
        </label>
        <label>Password
          <input type="password" bind:value={password} autocomplete="current-password" required disabled={authBusy || !cloudReady} />
        </label>
        <button type="submit" disabled={authBusy || !cloudReady || !email || !password}>
          {authBusy ? "Signing in…" : "Sign in"}
        </button>
      </form>
      {#if !cloudReady}
        <p class="hint">Sign-in is enabled once Synchrome Cloud is configured and its Auth service is reachable.</p>
      {/if}
      {#if account?.error}<p class="warn small">{account.error}</p>{/if}
    {/if}
    {#if authError}<p class="bad small">{authError}</p>{/if}
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
      <h2>Browsers on this Mac</h2>
      <span class="badge muted">{browsers.length} browsers · {totalProfiles} profiles</span>
    </div>
    <p class="lead">Read-only detection. Nothing is modified.</p>
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
            {#if b.profiles.length > 0}
              <ul class="profiles">
                {#each b.profiles as p (p.dir_name)}
                  <li class="profile">
                    <span class="pname">{p.display_name ?? p.dir_name}</span>
                    <span class="pdir">{p.dir_name}</span>
                    <span class="badge small {ACCESS[p.access]?.cls ?? 'muted'}">{ACCESS[p.access]?.label ?? p.access}</span>
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
  main { max-width: 860px; margin: 0 auto; padding: 22px 20px 48px; }
  header { display: flex; align-items: baseline; gap: 12px; margin-bottom: 14px; }
  h1 { font-size: 22px; margin: 0; color: #5ac8be; }
  h2 { font-size: 15px; margin: 0; }
  .ua { color: #6b7988; font-size: 12px; }
  .right { margin-left: auto; }
  .lead { color: #9aa7b4; margin: 6px 0 12px; font-size: 13px; }
  .hint { color: #9aa7b4; font-size: 13px; margin: 8px 0 0; }
  .tiny { font-size: 11px; color: #6b7988; }
  .small { font-size: 13px; margin: 6px 0 0; }
  code { background: #1b2733; padding: 1px 5px; border-radius: 4px; font-size: 12px; }
  button { background: #1b2733; color: #e6edf3; border: 1px solid #2b3a49; border-radius: 7px;
    padding: 7px 14px; cursor: pointer; }
  button:disabled { opacity: 0.55; cursor: default; }
  button.danger { border-color: #8a2a2a; color: #f08a8a; }
  .card { background: #141f2a; border: 1px solid #22303d; border-radius: 10px;
    padding: 14px 16px; margin-bottom: 14px; }
  .card-head { display: flex; align-items: center; gap: 10px; margin-bottom: 8px; }
  .kv { display: flex; gap: 12px; font-size: 13px; padding: 3px 0; align-items: baseline; }
  .kv > span:first-child { color: #6b7988; min-width: 110px; }
  .login { display: grid; gap: 10px; max-width: 360px; margin-top: 6px; }
  .login label { display: grid; gap: 4px; font-size: 12px; color: #9aa7b4; }
  .login input { background: #0f1720; color: #e6edf3; border: 1px solid #2b3a49;
    border-radius: 7px; padding: 8px 10px; font-size: 14px; }
  .login input:disabled { opacity: 0.5; }
  .services { display: grid; grid-template-columns: repeat(3, 1fr); gap: 8px; margin: 8px 0; }
  .svc { display: flex; justify-content: space-between; align-items: center;
    background: #0f1720; border: 1px solid #22303d; border-radius: 8px; padding: 6px 10px; font-size: 13px; }
  .actions { margin-top: 10px; } .actions.row { display: flex; gap: 8px; }
  .notice { font-size: 13px; padding: 8px 10px; border-radius: 8px; margin-bottom: 10px;
    background: #3a2a10; border: 1px solid #8a6320; }
  .browsers, .profiles { list-style: none; padding: 0; margin: 0; }
  .browser { background: #0f1720; border: 1px solid #22303d; border-radius: 10px; padding: 12px 14px; margin-bottom: 10px; }
  .browser-head { display: flex; align-items: center; gap: 10px; }
  .name { font-weight: 600; font-size: 15px; }
  .ver { color: #6b7988; font-size: 12px; }
  .path { color: #6b7988; font-size: 12px; margin: 4px 0 8px; word-break: break-all; }
  .profiles { border-top: 1px solid #22303d; }
  .profile { display: flex; align-items: center; gap: 10px; padding: 6px 0; border-bottom: 1px solid #1b2733; }
  .pname { flex: 1; }
  .pdir { color: #6b7988; font-size: 12px; }
  .profiles-note { font-size: 12px; padding-top: 8px; }
  .badge { margin-left: auto; font-size: 11px; padding: 2px 8px; border-radius: 999px;
    border: 1px solid transparent; white-space: nowrap; }
  .badge.small { font-size: 10px; margin-left: 0; }
  .badge.ok, .ok { color: #5fd08a; } .badge.ok { background: #10301f; border-color: #1c7a44; }
  .badge.warn, .warn { color: #e0a94a; } .badge.warn { background: #3a2a10; border-color: #8a6320; }
  .badge.info { background: #10263a; color: #5aa6e0; border-color: #1c5a8a; }
  .badge.bad, .bad { color: #f08a8a; } .badge.bad { background: #3a1414; border-color: #8a2a2a; }
  .badge.muted, .muted { color: #8a97a4; } .badge.muted { background: #1b2733; border-color: #2b3a49; }
  .error { background: #3a1414; color: #f0a0a0; padding: 10px 12px; border-radius: 8px; margin-bottom: 12px; }
</style>
