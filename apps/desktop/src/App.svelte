<script>
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";

  let browsers = $state([]);
  let cloud = $state(null);
  let version = $state("");
  let loadingBrowsers = $state(true);
  let loadingCloud = $state(true);
  let error = $state(null);

  const ACCESS = {
    accessible: { label: "Accessible", cls: "ok" },
    requires_full_disk_access: { label: "Requires Full Disk Access", cls: "warn" },
    browser_running: { label: "Browser running", cls: "info" },
    unknown: { label: "Unknown", cls: "muted" },
  };

  async function loadBrowsers() {
    loadingBrowsers = true;
    try {
      browsers = await invoke("list_browsers");
    } catch (e) {
      error = String(e);
    } finally {
      loadingBrowsers = false;
    }
  }

  async function loadCloud() {
    loadingCloud = true;
    try {
      cloud = await invoke("cloud_status");
    } catch (e) {
      error = String(e);
    } finally {
      loadingCloud = false;
    }
  }

  async function loadAll() {
    error = null;
    version = await invoke("app_version").catch(() => "");
    await Promise.all([loadBrowsers(), loadCloud()]);
  }

  onMount(loadAll);

  const needsFda = $derived(browsers.some((b) => b.access === "requires_full_disk_access"));
  const totalProfiles = $derived(browsers.reduce((n, b) => n + b.profiles.length, 0));

  function cloudSummary(c) {
    if (!c) return { label: "Checking…", cls: "muted" };
    if (!c.configured) return { label: "Not configured", cls: "warn" };
    if (!c.reachable) return { label: "Unreachable", cls: "bad" };
    if (c.auth_ok && c.rest_ok && c.storage_ok) return { label: "Connected", cls: "ok" };
    return { label: "Partially reachable", cls: "warn" };
  }
  function probeBadge(p, okWhen) {
    if (!p) return { label: "—", cls: "muted" };
    if (p.outcome === "unreachable") return { label: "unreachable", cls: "bad" };
    const ok = okWhen(p);
    return { label: `${p.status ?? "?"} · ${p.latency_ms} ms`, cls: ok ? "ok" : "warn" };
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

  <!-- ===== Onboarding: Cloud ===== -->
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
      <p class="hint">
        Add <code>SUPABASE_URL</code> and <code>SUPABASE_ANON_KEY</code> to a <code>.env</code>
        file next to the app (see <code>.env.example</code>). Only publishable values — never
        an administrative key.
      </p>
    {:else if cloud}
      {@const a = probeBadge(cloud.auth, (p) => p.outcome === "ok")}
      {@const r = probeBadge(cloud.rest, (p) => p.outcome === "ok")}
      {@const st = probeBadge(cloud.storage, (p) => p.outcome === "ok")}
      <div class="kv"><span>Endpoint</span><code>{cloud.endpoint}</code></div>
      <div class="kv"><span>Reachable</span><span class={cloud.reachable ? "ok" : "bad"}>{cloud.reachable ? "yes" : "no"}</span></div>
      <div class="services">
        <div class="svc"><span>Auth</span><span class="badge small {a.cls}">{a.label}</span></div>
        <div class="svc"><span>PostgREST</span><span class="badge small {r.cls}">{r.label}</span></div>
        <div class="svc"><span>Storage</span><span class="badge small {st.cls}">{st.label}</span></div>
      </div>
      {#if cloud.latency_ms != null}
        <div class="kv"><span>Max latency</span><span>{cloud.latency_ms} ms</span></div>
      {/if}
      {#if cloud.error}<p class="bad">{cloud.error}</p>{/if}
    {/if}

    <div class="actions">
      <button disabled title="Coming next">Sign in — coming next</button>
    </div>
  </section>

  <!-- ===== Onboarding: Browsers ===== -->
  <section class="card">
    <div class="card-head">
      <h2>Browsers on this Mac</h2>
      <span class="badge muted">{browsers.length} browsers · {totalProfiles} profiles</span>
    </div>
    <p class="lead">Read-only detection. Nothing is modified.</p>

    {#if needsFda}
      <div class="notice warn">
        Some browsers (Chrome, Brave, Edge) are protected by macOS. Synchrome will need
        <strong>Full Disk Access</strong> to read their profiles. Not requested yet.
      </div>
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
  .hint { color: #9aa7b4; font-size: 13px; }
  code { background: #1b2733; padding: 1px 5px; border-radius: 4px; font-size: 12px; }
  button { background: #1b2733; color: #e6edf3; border: 1px solid #2b3a49; border-radius: 7px;
    padding: 7px 14px; cursor: pointer; }
  button:disabled { opacity: 0.55; cursor: default; }
  .card { background: #141f2a; border: 1px solid #22303d; border-radius: 10px;
    padding: 14px 16px; margin-bottom: 14px; }
  .card-head { display: flex; align-items: center; gap: 10px; margin-bottom: 8px; }
  .kv { display: flex; gap: 12px; font-size: 13px; padding: 3px 0; }
  .kv > span:first-child { color: #6b7988; min-width: 100px; }
  .services { display: grid; grid-template-columns: repeat(3, 1fr); gap: 8px; margin: 8px 0; }
  .svc { display: flex; justify-content: space-between; align-items: center;
    background: #0f1720; border: 1px solid #22303d; border-radius: 8px; padding: 6px 10px; font-size: 13px; }
  .actions { margin-top: 10px; }
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
