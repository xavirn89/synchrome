// Shows the handshake state. Never reads or displays the mapping token.
const LABELS = {
  connected: "Connected",
  unmapped: "Unmapped",
  token_mismatch: "Token mismatch",
  app_unavailable: "App unavailable",
};
const DETAILS = {
  token_missing: "This profile is not linked yet. Map it in the Synchrome app.",
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
