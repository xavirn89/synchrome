// Shows the handshake state. Never reads or displays the mapping token.
const LABELS = {
  connected: "Connected",
  unmapped: "Unmapped",
  token_mismatch: "Token mismatch",
  app_unavailable: "App unavailable",
};
const BOOKMARKS = {
  not_synced: "Not synced",
  initial_upload: "Initial upload",
  up_to_date: "Up to date",
  pending_changes: "Pending changes",
  offline: "Offline",
  error: "Error",
};
const DOWNLOAD = {
  up_to_date: "Up to date",
  remote_changes: "Remote changes",
  applying: "Applying",
  conflict: "Conflict",
  offline: "Offline",
  error: "Error",
};
const DETAILS = {
  token_missing: "This profile is not linked yet. Map it in the Synchrome app.",
  connected_offline: "Synchrome Cloud is unreachable; bookmark changes are queued in the app.",
  token_unknown: "The link token is not known to your Synchrome account.",
  token_other_account: "This profile was linked while signed in to another account.",
  mapping_deleted: "This profile's mapping was removed in Synchrome.",
  cloud_profile_deleted: "The Synchrome profile this was linked to no longer exists.",
  wrong_device: "This link belongs to another device.",
  not_signed_in: "Sign in to the Synchrome app.",
  cloud_unavailable: "Synchrome Cloud is not reachable right now.",
  host_unavailable: "The Synchrome app connector is not installed or not running.",
  timeout: "The Synchrome app did not answer.",
};

function render(s) {
  const el = document.getElementById("state");
  const ui = (s && s.ui) || "unmapped";
  el.className = `state ${ui}`;
  el.textContent = LABELS[ui] || ui;
  document.getElementById("profile").innerHTML =
    s && s.profile_name ? `Profile: <strong></strong>` : "";
  if (s && s.profile_name) document.querySelector("#profile strong").textContent = s.profile_name;
  document.getElementById("detail").textContent = s ? DETAILS[s.status] || "" : "";
  const b = s && s.bookmarks;
  const el2 = document.getElementById("bookmarks");
  if (!b) {
    el2.textContent = "";
    return;
  }
  const when = b.last_upload_at ? new Date(b.last_upload_at * 1000).toLocaleString() : "never";
  el2.textContent = `Bookmarks: ${BOOKMARKS[b.state] || b.state} · ${b.bookmarks} bookmarks · ${b.pending} pending · last upload ${when}`;
  const d = b.download;
  const el3 = document.getElementById("download");
  if (!d) {
    el3.textContent = "";
    return;
  }
  const dwhen = d.last_download_at ? new Date(d.last_download_at * 1000).toLocaleString() : "never";
  el3.textContent = `Download: ${DOWNLOAD[d.state] || d.state} · ${d.remote_pending} remote changes · ${d.conflicts} conflicts · last download ${dwhen}`;
}

chrome.runtime.sendMessage({ type: "get_status" }, render);
document.getElementById("reconnect").addEventListener("click", () => {
  const b = document.getElementById("reconnect");
  b.disabled = true;
  b.textContent = "Connecting…";
  chrome.runtime.sendMessage({ type: "reconnect" }, (s) => {
    render(s);
    b.disabled = false;
    b.textContent = "Reconnect";
  });
});
