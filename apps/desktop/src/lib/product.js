// Product rules shared by the shell, profile dialogs and connection views.
// These helpers never mutate the sync engine's models or browser data.

export const localKey = (browser, profile) =>
  `${browser.kind}/${browser.user_data_dir}/${profile.dir_name}`;

const foldName = (name) => String(name ?? "").trim().normalize("NFC").toLocaleLowerCase("en-US");

export function validateName(name, profiles = [], excludeId = null) {
  const trimmed = String(name ?? "").trim();
  if (!trimmed) return "Give this profile a name.";
  if (trimmed.length > 80) return "Keep the name to 80 characters or fewer.";
  if (profiles.some((p) => p.id !== excludeId && foldName(p.name) === foldName(trimmed))) {
    return "You already have a profile with this name. Choose another name.";
  }
  return "";
}

function settingsViews(mapping) {
  const settings = mapping.extension_settings ?? mapping.settings;
  if (!settings) return [];
  if (Array.isArray(settings)) return settings;
  return settings.state ? [settings] : Object.values(settings);
}

export function connectionConflictCount(mapping) {
  const settingsCount = settingsViews(mapping).reduce((total, item) => total + Number(item?.conflicts ?? 0), 0);
  const datasetCount = Number(mapping.extensions?.conflicts ?? 0)
    + Number(mapping.preferences?.conflicts ?? mapping.prefs?.conflicts ?? 0)
    + settingsCount;
  // DownloadStatus.conflicts is the profile-wide total (all collections), not
  // just bookmarks. Dataset counts can be newer after a settings-only refresh.
  return Math.max(Number(mapping.bookmarks?.download?.conflicts ?? 0), datasetCount);
}

const summary = (label, tone, description, action) =>
  ({ label, tone, description, ...(action ? { action } : {}) });

export function cloudState(cloud) {
  if (cloud?.configured === false) {
    return summary("Cloud not configured", "warning", "This copy of Synchrome needs its cloud connection configured.");
  }
  if (cloud?.reachable === false) {
    return summary("Cloud unavailable", "warning", "Captured local changes are kept until Synchrome Cloud is available.");
  }
  const probes = [cloud?.auth_ok, cloud?.rest_ok, cloud?.storage_ok];
  if (cloud?.reachable === true && probes.every(value => value === true)) {
    return summary("Cloud connected", "positive", "The account, database and cloud file services are reachable.");
  }
  if (probes.some(value => value === false)) {
    return summary("Cloud partially available", "warning", "Some cloud services are unavailable. Open diagnostics to check the connection.");
  }
  return summary("Checking cloud…", "info", "The cloud service check has not completed yet.");
}

export function connectionState(mapping, online = true) {
  if (!mapping.is_this_device) {
    return summary("Connected on another Mac", "neutral", "This connection belongs to another Mac. Open Synchrome there to check its current sync status.");
  }

  const conflicts = connectionConflictCount(mapping);
  if (conflicts > 0) {
    return summary("Review conflict", "warning", `${conflicts === 1 ? "One change needs" : `${conflicts} changes need`} your decision. Everything else can keep syncing.`, "conflicts");
  }

  const localError = String(mapping.local_error ?? "").toLowerCase();
  const preferences = mapping.preferences ?? mapping.prefs;
  const datasets = [preferences, ...settingsViews(mapping)].filter((view) => view && view.mode !== "off");
  if (mapping.display_status === "pending_local_write"
    || (mapping.display_status === "error" && /browser.*(open|running)|close.*browser|locked/.test(localError))) {
    return summary("Close browser to connect", "warning", "Quit this browser, then retry the connection. Your browser profile stays in place.", "retry");
  }

  if (mapping.display_status === "error" || mapping.local_error) {
    return summary("Needs attention", "danger", "Synchrome could not complete a change. Review this connection for the next step.", "retry");
  }

  if (mapping.bookmarks?.state === "error" || mapping.bookmarks?.download?.state === "error") {
    return summary("Needs attention", "danger", "Synchrome could not send or apply a bookmark change. Review the sync details, then try syncing again.", "sync");
  }

  if (datasets.some((view) => view.state === "error")) {
    return summary("Needs attention", "danger", "A browser preference or extension setting could not be applied. Other browser data can keep syncing.", "details");
  }

  if (datasets.some((view) => view.state === "version_mismatch" || view.state === "unsupported")) {
    return summary("Check browser compatibility", "warning", "Some preferences or extension settings need review after a browser update. Other browser data can keep syncing.", "details");
  }

  if (mapping.display_status === "token_written") {
    return summary("Finish setup", "info", "The browser is linked. Add the Synchrome companion and open its popup to confirm the connection.", "setup");
  }

  if (!online || mapping.bookmarks?.state === "offline" || mapping.bookmarks?.download?.state === "offline") {
    return summary("Offline · changes saved", "warning", "Changes captured on this Mac stay queued until Synchrome Cloud is available again.", "sync");
  }

  if (mapping.display_status !== "extension_confirmed") {
    return summary("Connection pending", "info", "Complete the connection in this browser before it can sync.", "setup");
  }

  const bookmarks = mapping.bookmarks;
  if (bookmarks?.state === "initial_upload") {
    return summary("First sync in progress", "info", "Your browser data is being added to this Synchrome profile.");
  }

  if (!bookmarks || bookmarks.state === "not_synced"
    || (!bookmarks.last_upload_at && bookmarks.state !== "up_to_date")) {
    return summary("First sync pending", "info", "The companion is connected. Keep Synchrome open while the first browser changes are captured and uploaded.", "sync");
  }

  if (datasets.some((view) => view.state === "waiting_for_browser_close")) {
    return summary("Close browser to finish sync", "info", "Quit this browser so Synchrome can safely apply its preferences and extension settings. Bookmarks can continue syncing while it is open.", "details");
  }

  if (bookmarks.pending > 0 || bookmarks.state === "pending_changes"
    || bookmarks.download?.remote_pending > 0 || ["remote_changes", "applying"].includes(bookmarks.download?.state)
    || bookmarks.download?.resolving > 0 || datasets.some((view) => view.state === "syncing" || view.pending > 0)) {
    return summary("Syncing changes", "info", "Synchrome is sending or applying browser changes. You can keep using the app.", "sync");
  }

  if (mapping.extensions?.user_action > 0) {
    return summary("Browser confirmation needed", "warning", "Open the companion popup to approve the extension change in your browser.", "setup");
  }

  if (mapping.extensions?.missing > 0 || mapping.extensions?.differences > 0) {
    return summary("Review extensions", "info", "Some extensions need to be installed or updated in this browser.", "extensions");
  }

  return summary("Up to date", "positive", "Captured bookmarks and extension changes are up to date. Preferences and extension settings apply when the browser is closed.");
}

// Priority is by the concrete action needed, not by the color of a badge. A ready
// remote connection cannot hide a problem on the Mac the user is looking at.
const priority = {
  "Review conflict": 0,
  "Needs attention": 1,
  "Check browser compatibility": 1,
  "Close browser to connect": 2,
  "Offline · changes saved": 3,
  "Browser confirmation needed": 4,
  "Finish setup": 5,
  "Connection pending": 6,
  "First sync pending": 7,
  "First sync in progress": 8,
  "Close browser to finish sync": 8,
  "Syncing changes": 9,
  "Review extensions": 10,
  "Up to date": 11,
};

export function attentionSummary(mappings = [], online = true) {
  const entries = mappings.filter(mapping => mapping.is_this_device)
    .map(mapping => ({ mapping, state: connectionState(mapping, online) }))
    .filter(({ state }) => ["warning", "danger"].includes(state.tone) || state.action === "setup")
    .sort((a, b) => (priority[a.state.label] ?? 12) - (priority[b.state.label] ?? 12));
  if (!entries.length) return null;
  const { mapping, state } = entries[0];
  const conflicts = entries.filter(entry => entry.state.action === "conflicts").length;
  const description = state.action === "conflicts"
    ? entries.length > conflicts
      ? "Review conflicting changes first. Other connections also need setup or recovery."
      : "Review changes made on different Macs before applying them."
    : state.description;
  return {
    count: entries.length,
    description,
    action: state.action === "conflicts" ? "conflicts" : "profile",
    profileId: mapping.profile_id,
    mappingId: mapping.id,
  };
}

export function profileState(profile, mappings = [], online = true) {
  const connections = mappings.filter((m) => m.profile_id === profile.id);
  if (connections.length === 0) {
    return summary("Not connected", "neutral", "Connect a browser profile on this Mac to start syncing.", "connect");
  }
  const local = connections.filter((m) => m.is_this_device);
  if (local.length === 0) {
    return summary("On another Mac", "neutral", "This profile has connections on your other Macs. Connect a browser here to bring it to this Mac.", "connect");
  }
  return local.map((m) => connectionState(m, online))
    .sort((a, b) => (priority[a.label] ?? 12) - (priority[b.label] ?? 12))[0];
}

export function groupConnections(mappings = []) {
  const groups = new Map();
  for (const connection of mappings) {
    const deviceId = connection.device_id ?? (connection.is_this_device ? "this-device" : connection.device_name ?? "unknown-device");
    if (!groups.has(deviceId)) {
      groups.set(deviceId, {
        deviceId,
        name: connection.device_name ?? (connection.is_this_device ? "This Mac" : "Another Mac"),
        isCurrent: !!connection.is_this_device,
        connections: [],
      });
    }
    const group = groups.get(deviceId);
    group.isCurrent ||= !!connection.is_this_device;
    group.connections.push(connection);
  }
  return [...groups.values()].sort((a, b) => Number(b.isCurrent) - Number(a.isCurrent));
}

export function sameDeviceConflict(mappings, profileId, exceptMappingId = null) {
  return mappings.some((m) => m.profile_id === profileId && m.is_this_device && m.id !== exceptMappingId);
}

// Uses the existing safe Rust delete command only after every associated cloud
// link has been disconnected. The caller owns confirmation and refreshes after
// both success and failure so partial progress remains visible and recoverable.
export async function disconnectAndDelete(invoke, profileId, mappings = []) {
  const completedMappings = [];
  const associated = [...new Map(mappings.filter((m) => m.profile_id === profileId).map((m) => [m.id, m])).values()];
  for (const mapping of associated) {
    try {
      await invoke("unmap_local_profile", { mappingId: mapping.id });
      completedMappings.push(mapping.id);
    } catch (cause) {
      const message = completedMappings.length
        ? "Some browsers were disconnected, but Synchrome could not disconnect every browser. The profile was not deleted. Refresh and try again; local browser data is unchanged."
        : "Synchrome could not disconnect this profile's browsers. The profile was not deleted. Try again; local browser data is unchanged.";
      const error = new Error(message, { cause });
      error.completedMappings = [...completedMappings];
      error.profileDeleted = false;
      throw error;
    }
  }
  try {
    await invoke("delete_profile", { profileId });
  } catch (cause) {
    const causeMessage = String(cause?.message ?? cause ?? "");
    const profileDeleted = causeMessage.toLowerCase().includes("profile was deleted, but cleanup is incomplete");
    const error = new Error(profileDeleted
      ? causeMessage
      : "The browsers are disconnected, but Synchrome could not finish removing the cloud profile. Refresh and try again; local browser data is unchanged.", { cause });
    error.completedMappings = [...completedMappings];
    error.profileDeleted = profileDeleted;
    throw error;
  }
  return { profileId, profileDeleted: true, completedMappings };
}
