// In-memory, token-free Tauri fixtures. Loaded only by an explicit development
// preview; these paths and accounts do not refer to anything on the host machine.
export const PREVIEW_SCENARIOS = ["workspace", "onboarding", "auth", "offline", "empty"];

const clone = (value) => structuredClone(value);
const uid = (n) => `00000000-0000-4000-8000-${n.toString(16).padStart(12, "0")}`;
const ME = uid(1);
const OTHER = uid(2);
const PERSONAL = uid(10);
const WORK = uid(11);
const RESEARCH = uid(12);
const UBLOCK = "cjpalhdlnbpafiamejdnhcphjbkeiagm";
const BITWARDEN = "nngceckbapebfimnlniiiahkandclblb";
const DARK_READER = "eimadpbcbfnmbkopoojfekhnkhdbieeh";

function account(signedIn = true, email = "alex@example.test") {
  return {
    signed_in: signedIn,
    user_id: signedIn ? uid(90) : null,
    email: signedIn ? email : null,
    expires_at: signedIn ? Math.floor(Date.now() / 1000) + 3600 : null,
    needs_refresh: false,
    error: null,
  };
}

function bookmarkStatus(count, state = "up_to_date", conflicts = 0) {
  const now = Math.floor(Date.now() / 1000);
  return {
    state, bookmarks: count, folders: Math.ceil(count / 24),
    pending: state === "offline" ? 3 : 0,
    last_upload_at: state === "not_synced" ? null : now - 32,
    error: null,
    download: {
      state: conflicts ? "conflict" : state === "offline" ? "offline" : "up_to_date",
      last_download_at: state === "not_synced" ? null : now - 32,
      remote_pending: 0, conflicts, resolving: 0, error: null,
    },
  };
}

function extensions() {
  return [
    { ext_id: UBLOCK, name: "uBlock Origin", version: "1.67.0", source: "webstore", state: "synced", installed_here: true, enabled_here: true, installed_synced: true, enabled_synced: true, availability: "available", actions: ["disable"], error: null },
    { ext_id: BITWARDEN, name: "Bitwarden Password Manager", version: "2026.9.0", source: "webstore", state: "synced", installed_here: true, enabled_here: true, installed_synced: true, enabled_synced: true, availability: "available", actions: ["disable"], error: null },
    { ext_id: DARK_READER, name: "Dark Reader", version: "4.9.116", source: "webstore", state: "missing", installed_here: false, enabled_here: null, installed_synced: true, enabled_synced: true, availability: "available", actions: ["install"], error: null },
  ];
}

function settings(extId, enabled = false) {
  const now = Math.floor(Date.now() / 1000);
  return {
    ext_id: extId, mode: enabled ? "local" : "off", state: enabled ? "synced" : "off",
    keys_local: enabled ? 18 : 0, keys_sync: 0, pending: 0, conflicts: 0,
    excluded_sensitive: enabled ? 2 : 0, excluded_other: 0, error: null,
    last_capture_at: enabled ? now - 120 : null, last_apply_at: enabled ? now - 120 : null,
  };
}

function prefs(kind, enabled = false, running = false) {
  const now = Math.floor(Date.now() / 1000);
  return {
    mode: enabled ? "recommended" : "off",
    state: enabled ? running ? "waiting_for_browser_close" : "synced" : "off",
    synced: enabled ? 34 : 0, unsupported: 0, pending: enabled && running ? 2 : 0,
    conflicts: 0, invalid: 0, browser_kind: kind, browser_major: 140, error: null,
    last_capture_at: enabled ? now - 120 : null, last_apply_at: enabled ? now - 120 : null,
  };
}

function conflict() {
  const now = Math.floor(Date.now() / 1000);
  const side = (title) => ({ exists: true, enabled: null, value: null, title, url: "https://example.test/handbook", folder: ["Bookmarks bar", "Work"], position: 1, siblings: 3, changed: ["title"], changed_at: (now - 300) * 1000 });
  return {
    conflict_id: uid(80), kind: "local_pending", subtype: "edit_edit", status: "unresolved",
    created_at: now - 300, updated_at: now - 300, item_kind: "url", name: "Project handbook",
    local: side("Project handbook"), remote: side("Team handbook"), base: side("Handbook"),
    remote_device: OTHER, remote_seq: 18,
    options: [{ resolution: "keep_local", destructive: false }, { resolution: "keep_remote", destructive: false }],
    resolution: null, resolution_ops: 0, resolved_at: null, held: 1, detail: null,
    attempts: 0, local_items: 0, upload_pending: false,
  };
}

function fixture(scenario) {
  const now = new Date();
  const iso = now.toISOString();
  const browsers = [
    { kind: "chrome", display_name: "Google Chrome", version: "140.0.7339.80", user_data_dir: "/Preview/Google/Chrome", access: "browser_running", profiles: [
      { dir_name: "Default", display_name: "Personal", access: "browser_running" },
      { dir_name: "Profile 1", display_name: "Work", access: "browser_running" },
      { dir_name: "Profile 2", display_name: "Client studio", access: "browser_running" },
    ] },
    { kind: "helium", display_name: "Helium", version: "140.0.7339.80", user_data_dir: "/Preview/Helium", access: "accessible", profiles: [
      { dir_name: "Default", display_name: "Personal", access: "accessible" },
      { dir_name: "Profile 1", display_name: "Research", access: "accessible" },
      { dir_name: "Profile 2", display_name: "Reading", access: "accessible" },
    ] },
  ];
  const devices = [
    { device_id: ME, name: "MacBook Pro", platform: "macos", os_version: "15.7", app_version: "1.0.0", is_current: true, created_at: iso, last_seen_at: iso },
    { device_id: OTHER, name: "MacBook Air", platform: "macos", os_version: "15.7", app_version: "1.0.0", is_current: false, created_at: iso, last_seen_at: new Date(now.getTime() - 7200000).toISOString() },
  ];
  const profiles = [
    { id: PERSONAL, name: "Personal", created_at: iso, updated_at: iso },
    { id: WORK, name: "Work", created_at: iso, updated_at: iso },
    { id: RESEARCH, name: "Research", created_at: iso, updated_at: iso },
  ];
  const makeMapping = (id, profileId, deviceId, kind, dir, confirmed = true) => ({
    id: uid(id), mapped: true, profile_id: profileId, profile_name: profiles.find((p) => p.id === profileId).name,
    device_id: deviceId, device_name: devices.find((d) => d.device_id === deviceId).name,
    is_this_device: deviceId === ME, browser_kind: kind, browser_profile_dir: dir,
    status: confirmed ? "token_confirmed" : "token_pending_local_write",
    display_status: confirmed ? "extension_confirmed" : "token_written", local_error: null,
    token_hint: "preview", token_confirmed_at: confirmed ? iso : null, created_at: iso,
    bookmarks: deviceId === ME && confirmed ? bookmarkStatus(profileId === PERSONAL ? 248 : 86, "up_to_date", profileId === WORK ? 1 : 0) : null,
    extensions: deviceId === ME && confirmed ? { synced: 2, installed: 2, missing: 1, differences: 1, conflicts: 0, user_action: 0 } : null,
  });
  const state = {
    scenario, online: scenario !== "offline", account: account(scenario !== "auth"),
    device: { device_id: ME, device_name: "MacBook Pro", platform: "macos", os_version: "15.7" },
    browsers, devices, profiles,
    mappings: [makeMapping(20, PERSONAL, ME, "helium", "Default"), makeMapping(21, PERSONAL, OTHER, "chrome", "Default"), makeMapping(22, WORK, ME, "chrome", "Profile 1"), makeMapping(23, RESEARCH, ME, "helium", "Profile 1", false), makeMapping(24, WORK, OTHER, "chrome", "Profile 2")],
    extensions: {}, settings: {}, prefs: {}, conflicts: {}, retired: {}, connectors: [],
    browserData: Object.fromEntries(browsers.flatMap((b) => b.profiles.map((p) => [
      `${b.user_data_dir}/${p.dir_name}`,
      { bookmarks: p.dir_name === "Default" ? 248 : 86, folders: 8, extension_ids: [UBLOCK, BITWARDEN] },
    ]))),
    cloudObjects: { [PERSONAL]: ["snapshot", "journal"], [WORK]: ["snapshot", "journal"], [RESEARCH]: [] },
    nextId: 100,
  };
  for (const m of state.mappings.filter((m) => m.is_this_device)) {
    state.extensions[m.id] = extensions();
    state.settings[m.id] = extensions().map((x) => settings(x.ext_id, x.ext_id === UBLOCK));
    state.prefs[m.id] = prefs(m.browser_kind, m.display_status === "extension_confirmed", m.browser_kind === "chrome");
    state.conflicts[m.id] = m.profile_id === WORK && m.display_status === "extension_confirmed" ? [conflict()] : [];
  }
  if (["auth", "empty", "onboarding"].includes(scenario)) {
    state.profiles = []; state.mappings = []; state.devices = [devices[0]];
    state.extensions = {}; state.settings = {}; state.prefs = {}; state.conflicts = {}; state.cloudObjects = {};
  }
  if (scenario === "onboarding") {
    browsers[0].access = "requires_full_disk_access";
    browsers[0].profiles = [];
  }
  if (scenario === "offline") {
    for (const m of state.mappings.filter((m) => m.bookmarks)) m.bookmarks = bookmarkStatus(m.bookmarks.bookmarks, "offline", m.bookmarks.download.conflicts);
  }
  return state;
}

export function createPreviewBackend(scenario = "workspace", { delayMs = 0 } = {}) {
  if (!PREVIEW_SCENARIOS.includes(scenario)) throw new Error(`Unknown preview scenario: ${scenario}`);
  let state = fixture(scenario);
  const failures = new Map();
  const now = () => Math.floor(Date.now() / 1000);
  const signedIn = () => { if (!state.account.signed_in) throw new Error("not signed in"); };
  const online = () => { if (!state.online) throw new Error("offline: Synchrome Cloud is not reachable"); };
  const findProfile = (id) => {
    const p = state.profiles.find((p) => p.id === id);
    if (!p) throw new Error("profile not found");
    return p;
  };
  const findMapping = (id, localOnly = false) => {
    const m = state.mappings.find((m) => m.id === id);
    if (!m) throw new Error("mapping not found");
    if (localOnly && !m.is_this_device) throw new Error("this mapping belongs to another device");
    return m;
  };
  const browserFor = (m) => state.browsers.find((b) => b.kind === m.browser_kind && b.profiles.some((p) => p.dir_name === m.browser_profile_dir));
  const detected = ({ browserKind, browserProfileDir, userDataDir }) => {
    const b = state.browsers.find((b) => b.kind === browserKind && (!userDataDir || b.user_data_dir === userDataDir) && b.profiles.some((p) => p.dir_name === browserProfileDir));
    if (!b) throw new Error("The selected browser profile was not detected on this device");
    if (b.access === "requires_full_disk_access") throw new Error("Full Disk Access required");
    return b;
  };
  const uniqueLocalMapping = (args) => {
    if (state.mappings.some((m) => m.is_this_device && m.browser_kind === args.browserKind && m.browser_profile_dir === args.browserProfileDir)) {
      throw new Error("This browser profile is already connected. Disconnect it before connecting another profile.");
    }
  };
  const validName = (name) => {
    const trimmed = String(name ?? "").trim();
    if (!trimmed || trimmed.length > 200) throw new Error("Profile name must contain 1–200 characters");
    return trimmed;
  };
  const addProfile = (name) => {
    name = validName(name);
    if (state.profiles.some((p) => p.name.toLowerCase() === name.toLowerCase())) throw new Error("A profile with this name already exists");
    const iso = new Date().toISOString();
    const p = { id: uid(state.nextId++), name, created_at: iso, updated_at: iso };
    state.profiles.push(p); state.cloudObjects[p.id] = [];
    return p;
  };
  const addMapping = (profileId, args, b) => {
    const p = findProfile(profileId);
    if (state.mappings.some((m) => m.is_this_device && m.profile_id === profileId)) {
      throw new Error("This Synchrome profile already has a browser connected on this Mac. Disconnect it before choosing a different browser.");
    }
    const running = b.access === "browser_running";
    const m = {
      id: uid(state.nextId++), mapped: true, profile_id: profileId, profile_name: p.name,
      device_id: ME, device_name: state.device.device_name, is_this_device: true,
      browser_kind: args.browserKind, browser_profile_dir: args.browserProfileDir,
      status: "token_pending_local_write", display_status: running ? "error" : "token_written",
      local_error: running ? "browser is open — close it, then Retry local write" : null,
      token_hint: "preview", token_confirmed_at: null, created_at: new Date().toISOString(),
      bookmarks: null, extensions: null,
    };
    state.mappings.push(m); state.extensions[m.id] = extensions();
    state.settings[m.id] = extensions().map((x) => settings(x.ext_id));
    state.prefs[m.id] = prefs(m.browser_kind); state.conflicts[m.id] = [];
    return m;
  };
  const recalculateExtensions = (m) => {
    const list = state.extensions[m.id];
    m.extensions = {
      installed: list.filter((x) => x.installed_here).length,
      synced: list.filter((x) => x.state === "synced").length,
      missing: list.filter((x) => !x.installed_here && x.installed_synced).length,
      differences: list.filter((x) => x.state !== "synced").length,
      conflicts: list.filter((x) => x.state === "conflict").length,
      user_action: list.filter((x) => x.state === "user_action_required").length,
    };
  };
  const diagnostics = (m) => {
    const ids = [...new Set(state.mappings.filter((x) => x.profile_id === m.profile_id).map((x) => x.device_id))];
    const devices = ids.map((id) => ({ device_id: id, name: state.devices.find((d) => d.device_id === id)?.name ?? "Another device", is_current: id === ME, snapshot_capable: true, retired: !!state.retired[`${m.profile_id}:${id}`], blocks_gc: false }));
    return {
      realtime: { enabled: true, account_set: state.account.signed_in, connected: state.online, connects: 1, reconnects: 0, events: 18, own_events: 4, ignored_events: 0, coalesced: 2, pulls: 12, last_event_ms: (now() - 32) * 1000, last_pull_started_ms: (now() - 32) * 1000, last_error: state.online ? null : "offline", inflight: 0, max_inflight: 1 },
      local: { snapshot_generation: 3, bootstrapped_at: now() - 86400, bootstrap_ms: 240, bootstrap_fallback: false, cloud_generation: 3, last_compaction_at: now() - 3600, last_compaction_generation: 3, last_compaction_ms: 190, last_compaction_error: null, last_gc_at: now() - 1800, last_gc_events: 24, last_gc_objects: 4, gc_blockers: 0 },
      cloud: state.online ? { server_now: new Date().toISOString(), current_generation: 3, current_ready_at: new Date((now() - 3600) * 1000).toISOString(), previous_generation: 2, journal_gc_watermarks: {}, gc_blockers: 0, journal_gc_ok: true, generation_cleanup_ok: true, claim_allowed: false } : null,
      gc_blocked: false, snapshot_generations_kept: 2, this_device_retired: !!state.retired[`${m.profile_id}:${ME}`], devices,
    };
  };

  function dispatch(command, args) {
    switch (command) {
      case "app_version": return "Synchrome/1.0.0";
      case "list_browsers": return state.browsers;
      case "account_status": return state.account;
      case "device_identity": return state.device;
      case "install_location": return { path: "/Applications/Synchrome.app/Contents/MacOS/synchrome-desktop", stable: true, reason: null };
      case "extension_folder": return "/Applications/Synchrome.app/Contents/Resources/extension";
      case "reveal_extension_folder": return null;
      case "open_full_disk_access_settings": return null;
      case "cloud_status": {
        const probe = { status: state.online ? 200 : null, latency_ms: state.online ? 42 : 0, outcome: state.online ? "ok" : "unreachable", error: state.online ? null : "offline" };
        return { configured: true, endpoint: "https://preview.synchrome.example", reachable: state.online, auth_ok: state.online, rest_ok: state.online, storage_ok: state.online, auth: probe, rest: probe, storage: probe, latency_ms: state.online ? 42 : null, error: state.online ? null : "offline: Synchrome Cloud is not reachable" };
      }
      case "sign_in":
      case "sign_up": {
        online();
        const email = String(args.email ?? "").trim();
        if (!/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email)) throw new Error("Enter a valid email address");
        if (!args.password || args.password === "wrong-password") throw new Error("invalid credentials");
        if (command === "sign_up" && args.password.length < 8) throw new Error("Password must contain at least 8 characters");
        state.account = account(true, email);
        return state.account;
      }
      case "sign_out": state.account = account(false); return state.account;
      case "refresh_session": signedIn(); online(); state.account = account(true, state.account.email); return state.account;
      case "register_device": signedIn(); return state.devices.find((d) => d.is_current);
      case "list_devices": signedIn(); return state.devices;
      case "list_profiles": signedIn(); return state.profiles;
      case "list_mappings": signedIn(); return state.mappings;
      case "create_profile": signedIn(); online(); return addProfile(args.name);
      case "rename_profile": {
        signedIn(); online(); const p = findProfile(args.profileId); const name = validName(args.name);
        if (state.profiles.some((x) => x.id !== p.id && x.name.toLowerCase() === name.toLowerCase())) throw new Error("A profile with this name already exists");
        p.name = name; p.updated_at = new Date().toISOString();
        for (const m of state.mappings.filter((m) => m.profile_id === p.id)) m.profile_name = name;
        return p;
      }
      case "delete_profile": {
        signedIn(); online(); findProfile(args.profileId);
        if (state.mappings.some((m) => m.profile_id === args.profileId)) throw new Error("Unmap it on every device first");
        state.profiles = state.profiles.filter((p) => p.id !== args.profileId);
        delete state.cloudObjects[args.profileId];
        for (const key of Object.keys(state.retired)) if (key.startsWith(`${args.profileId}:`)) delete state.retired[key];
        return null;
      }
      case "create_and_map_local_profile": {
        signedIn(); online(); const b = detected(args); uniqueLocalMapping(args);
        const p = addProfile(args.name);
        return addMapping(p.id, args, b);
      }
      case "map_local_profile": {
        signedIn(); online(); const b = detected(args); uniqueLocalMapping(args);
        return addMapping(args.profileId, args, b);
      }
      case "mapping_for_local_profile": signedIn(); return state.mappings.find((m) => m.is_this_device && m.browser_kind === args.browserKind && m.browser_profile_dir === args.browserProfileDir) ?? null;
      case "mapping_for_cloud_profile": signedIn(); return state.mappings.filter((m) => m.profile_id === args.profileId);
      case "unmap_local_profile": {
        signedIn(); online(); const m = findMapping(args.mappingId);
        state.mappings = state.mappings.filter((x) => x.id !== m.id);
        delete state.extensions[m.id]; delete state.settings[m.id]; delete state.prefs[m.id]; delete state.conflicts[m.id];
        return null;
      }
      case "write_mapping_token_local": {
        signedIn(); const m = findMapping(args.mappingId, true);
        if (browserFor(m)?.access === "browser_running") throw new Error("browser is open — close it, then Retry local write");
        m.display_status = "token_written"; m.local_error = null;
        return m;
      }
      case "install_native_host": {
        const b = state.browsers.find((b) => b.kind === args.browserKind && (!args.userDataDir || b.user_data_dir === args.userDataDir));
        if (!b) throw new Error("Browser was not detected on this device");
        if (!state.connectors.includes(b.kind)) state.connectors.push(b.kind);
        return `${b.user_data_dir}/NativeMessagingHosts/com.synchrome.host.json`;
      }
      case "publish_bookmarks": {
        signedIn(); const m = findMapping(args.mappingId, true);
        if (m.display_status !== "extension_confirmed") return null;
        if (!state.online) {
          m.bookmarks.state = "offline"; m.bookmarks.download.state = "offline";
          throw new Error("offline: changes are queued for the next connection");
        }
        m.bookmarks.state = "up_to_date"; m.bookmarks.pending = 0; m.bookmarks.last_upload_at = now();
        const b = browserFor(m);
        const data = b && state.browserData[`${b.user_data_dir}/${m.browser_profile_dir}`];
        if (data) { m.bookmarks.bookmarks = data.bookmarks; m.bookmarks.folders = data.folders; }
        m.bookmarks.download.last_download_at = now();
        return m.bookmarks;
      }
      case "list_conflicts": {
        signedIn(); const m = findMapping(args.mappingId, true);
        return (state.conflicts[m.id] ?? []).filter((c) => args.includeClosed || ["unresolved", "resolving"].includes(c.status));
      }
      case "resolve_conflicts": {
        signedIn(); const m = findMapping(args.mappingId, true);
        const list = state.conflicts[m.id] ?? [];
        return (args.items ?? []).map((item) => {
          const c = list.find((c) => c.conflict_id === item.conflict_id);
          if (!c) return { conflict_id: item.conflict_id, outcome: "error", status: null, ops: 0, error: "conflict not found" };
          if (c.status === "resolved") return { conflict_id: c.conflict_id, outcome: "already_resolved", status: "resolved", ops: 0, error: null };
          if (!c.options.some((o) => o.resolution === item.resolution)) return { conflict_id: c.conflict_id, outcome: "error", status: c.status, ops: 0, error: "resolution not available" };
          c.status = "resolved"; c.resolution = item.resolution; c.resolved_at = now(); c.resolution_ops = 1; c.upload_pending = !state.online;
          if (m.bookmarks?.download) {
            m.bookmarks.download.conflicts = list.filter((x) => x.status === "unresolved").length;
            m.bookmarks.download.state = m.bookmarks.download.conflicts ? "conflict" : state.online ? "up_to_date" : "offline";
          }
          return { conflict_id: c.conflict_id, outcome: "resolved", status: "resolved", ops: 1, error: null };
        });
      }
      case "list_extensions": signedIn(); return state.extensions[findMapping(args.mappingId, true).id] ?? [];
      case "extension_action": {
        signedIn(); const m = findMapping(args.mappingId, true); const x = state.extensions[m.id]?.find((x) => x.ext_id === args.extId);
        if (!x || !x.actions.includes(args.action)) throw new Error("Extension action is not available");
        if (args.action === "install") { x.state = "install_opened"; x.actions = ["install"]; }
        else if (["enable", "disable"].includes(args.action)) {
          x.enabled_here = x.enabled_synced = args.action === "enable"; x.state = "synced"; x.actions = [args.action === "enable" ? "disable" : "enable"];
        } else if (args.action === "remove") { x.installed_here = x.installed_synced = false; x.state = "synced"; x.actions = []; }
        else { x.state = "synced"; x.actions = x.enabled_here ? ["disable"] : ["enable"]; }
        recalculateExtensions(m); return null;
      }
      case "list_extension_settings": signedIn(); return state.settings[findMapping(args.mappingId, true).id] ?? [];
      case "set_extension_settings_mode": {
        signedIn(); const m = findMapping(args.mappingId, true);
        if (!["off", "local", "local_sync"].includes(args.mode)) throw new Error("Invalid extension settings mode");
        const s = state.settings[m.id]?.find((s) => s.ext_id === args.extId);
        if (!s) throw new Error("Extension was not detected on this device");
        s.mode = args.mode; s.state = args.mode === "off" ? "off" : browserFor(m)?.access === "browser_running" ? "waiting_for_browser_close" : "syncing";
        return null;
      }
      case "extension_settings_sync_now": {
        signedIn(); const m = findMapping(args.mappingId, true); const running = browserFor(m)?.access === "browser_running";
        for (const s of state.settings[m.id].filter((s) => s.mode !== "off")) { s.state = running ? "waiting_for_browser_close" : "synced"; if (!running) { s.last_capture_at = s.last_apply_at = now(); s.keys_local = 18; s.keys_sync = s.mode === "local_sync" ? 6 : 0; s.pending = 0; } }
        return { browser_running: running, captured_ops: running ? 0 : 2, keys_written: running ? 0 : 2, excluded_sensitive: 2, errors: 0 };
      }
      case "list_browser_preferences": signedIn(); return state.prefs[findMapping(args.mappingId, true).id];
      case "set_browser_preferences_mode": {
        signedIn(); const m = findMapping(args.mappingId, true);
        if (!["off", "recommended"].includes(args.mode)) throw new Error("Invalid browser preferences mode");
        const p = state.prefs[m.id]; p.mode = args.mode; p.state = args.mode === "off" ? "off" : browserFor(m)?.access === "browser_running" ? "waiting_for_browser_close" : "syncing";
        return null;
      }
      case "browser_preferences_sync_now": {
        signedIn(); const m = findMapping(args.mappingId, true); const running = browserFor(m)?.access === "browser_running"; const p = state.prefs[m.id];
        if (p.mode !== "off") { p.state = running ? "waiting_for_browser_close" : "synced"; if (!running) { p.synced = 34; p.pending = 0; p.last_capture_at = p.last_apply_at = now(); } }
        return { browser_running: running, captured_ops: running ? 0 : 2, paths_written: running ? 0 : 2, unsupported: 0, version_mismatch: 0, invalid: 0, errors: 0 };
      }
      case "sync_diagnostics": signedIn(); return diagnostics(findMapping(args.mappingId, true));
      case "retire_device":
      case "reactivate_device": signedIn(); online(); findProfile(args.profileId); state.retired[`${args.profileId}:${args.deviceId}`] = command === "retire_device"; return null;
      default: throw new Error(`Preview does not implement command: ${command}`);
    }
  }

  return {
    async invoke(command, args = {}) {
      if (delayMs > 0) await new Promise((resolve) => setTimeout(resolve, delayMs));
      if (failures.has(command)) { const message = failures.get(command); failures.delete(command); throw new Error(message); }
      return clone(dispatch(command, args));
    },
    snapshot: () => clone(state),
    reset(nextScenario = scenario) {
      if (!PREVIEW_SCENARIOS.includes(nextScenario)) throw new Error(`Unknown preview scenario: ${nextScenario}`);
      state = fixture(nextScenario); failures.clear(); return clone(state);
    },
    failNext(command, message = "network connection failed") { failures.set(command, message); },
    setOnline(value) {
      state.online = !!value;
      for (const m of state.mappings.filter((m) => m.bookmarks)) {
        m.bookmarks.state = value ? m.bookmarks.pending ? "pending_changes" : "up_to_date" : "offline";
        m.bookmarks.download.state = value ? m.bookmarks.download.conflicts ? "conflict" : "up_to_date" : "offline";
      }
    },
    setBrowserAccess(kind, access) {
      if (!["accessible", "browser_running", "requires_full_disk_access", "unknown"].includes(access)) throw new Error("Unknown access state");
      const b = state.browsers.find((b) => b.kind === kind);
      if (!b) throw new Error("Browser not found");
      if (b.profiles.length === 0 && access === "accessible") b.profiles = clone(fixture("empty").browsers.find((b) => b.kind === kind).profiles);
      b.access = access; for (const p of b.profiles) p.access = access;
    },
    confirmMapping(mappingId) {
      const m = findMapping(mappingId, true);
      if (m.display_status !== "token_written") throw new Error("Write the connection locally before confirming the companion");
      m.display_status = "extension_confirmed"; m.status = "token_confirmed"; m.local_error = null; m.token_confirmed_at = new Date().toISOString();
      m.bookmarks = bookmarkStatus(0, "not_synced"); recalculateExtensions(m);
      return clone(m);
    },
  };
}
