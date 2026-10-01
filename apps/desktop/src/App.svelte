<script>
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";

  let browsers = $state([]);
  let version = $state("");
  let loading = $state(true);
  let error = $state(null);

  const ACCESS = {
    accessible: { label: "Accessible", cls: "ok" },
    requires_full_disk_access: { label: "Requires Full Disk Access", cls: "warn" },
    browser_running: { label: "Browser running", cls: "info" },
    unknown: { label: "Unknown", cls: "muted" },
  };

  async function load() {
    loading = true;
    error = null;
    try {
      version = await invoke("app_version");
      browsers = await invoke("list_browsers");
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  onMount(load);
</script>

<main>
  <header>
    <h1>Synchrome</h1>
    <span class="ua">{version}</span>
  </header>

  <p class="lead">Detected Chromium browsers and profiles on this Mac. Read-only — nothing is modified.</p>

  <div class="actions">
    <button onclick={load} disabled={loading}>{loading ? "Scanning…" : "Rescan"}</button>
  </div>

  {#if error}
    <div class="error">Error: {error}</div>
  {:else if loading}
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
</main>

<style>
  :global(body) {
    margin: 0;
    background: #0f1720;
    color: #e6edf3;
    font: 14px/1.5 -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
  }
  main { max-width: 820px; margin: 0 auto; padding: 24px 20px 48px; }
  header { display: flex; align-items: baseline; gap: 12px; }
  h1 { font-size: 22px; margin: 0; color: #5ac8be; }
  .ua { color: #6b7988; font-size: 12px; }
  .lead { color: #9aa7b4; margin: 8px 0 16px; }
  .actions { margin-bottom: 16px; }
  button {
    background: #1b2733; color: #e6edf3; border: 1px solid #2b3a49;
    border-radius: 7px; padding: 7px 14px; cursor: pointer;
  }
  button:disabled { opacity: 0.6; cursor: default; }
  .browsers, .profiles { list-style: none; padding: 0; margin: 0; }
  .browser {
    background: #141f2a; border: 1px solid #22303d; border-radius: 10px;
    padding: 14px 16px; margin-bottom: 12px;
  }
  .browser-head { display: flex; align-items: center; gap: 10px; }
  .name { font-weight: 600; font-size: 15px; }
  .ver { color: #6b7988; font-size: 12px; }
  .path { color: #6b7988; font-size: 12px; margin: 4px 0 10px; word-break: break-all; }
  .profiles { border-top: 1px solid #22303d; }
  .profile {
    display: flex; align-items: center; gap: 10px;
    padding: 7px 0; border-bottom: 1px solid #1b2733;
  }
  .pname { flex: 1; }
  .pdir { color: #6b7988; font-size: 12px; }
  .profiles-note { font-size: 12px; padding-top: 8px; }
  .badge {
    margin-left: auto; font-size: 11px; padding: 2px 8px; border-radius: 999px;
    border: 1px solid transparent; white-space: nowrap;
  }
  .badge.small { font-size: 10px; margin-left: 0; }
  .badge.ok { background: #10301f; color: #5fd08a; border-color: #1c7a44; }
  .badge.warn { background: #3a2a10; color: #e0a94a; border-color: #8a6320; }
  .badge.info { background: #10263a; color: #5aa6e0; border-color: #1c5a8a; }
  .badge.muted { background: #1b2733; color: #8a97a4; border-color: #2b3a49; }
  .warn { color: #e0a94a; }
  .muted { color: #8a97a4; }
  .error { background: #3a1414; color: #f0a0a0; padding: 10px 12px; border-radius: 8px; }
</style>
