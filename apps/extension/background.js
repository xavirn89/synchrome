// Synchrome companion extension — handshake only (no sync yet).
//
// Reads the mapping token the Synchrome app seeded into this profile's chrome.storage.local,
// sends it to the native host in a Hello frame, and stores ONLY the resolved status (never the
// token) under "synchromeStatus". The token is never logged or shown.

const HOST = "com.synchrome.host";
const TOKEN_KEY = "mappingToken";
const STATUS_KEY = "synchromeStatus";
const HANDSHAKE_TIMEOUT_MS = 15000;

let inFlight = null;

async function saveStatus(status) {
  await chrome.storage.local.set({ [STATUS_KEY]: { ...status, at: Date.now() } });
}

function handshake() {
  if (inFlight) return inFlight;
  inFlight = new Promise(async (resolve) => {
    const finish = async (status) => {
      await saveStatus(status);
      inFlight = null;
      resolve(status);
    };
    const stored = await chrome.storage.local.get(TOKEN_KEY);
    const token = typeof stored[TOKEN_KEY] === "string" ? stored[TOKEN_KEY] : null;

    let port;
    try {
      port = chrome.runtime.connectNative(HOST);
    } catch (e) {
      return finish({ ui: "app_unavailable", status: "host_unavailable", detail: "native host could not be started" });
    }

    let settled = false;
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      try { port.disconnect(); } catch (_) {}
      finish({ ui: "app_unavailable", status: "timeout", detail: "no answer from the Synchrome app" });
    }, HANDSHAKE_TIMEOUT_MS);

    port.onMessage.addListener((msg) => {
      if (settled || !msg || msg.type !== "hello_result") return;
      settled = true;
      clearTimeout(timer);
      try { port.disconnect(); } catch (_) {}
      finish({
        ui: msg.ui_state,
        status: msg.status,
        mapped: !!msg.mapped,
        profile_id: msg.profile_id ?? null,
        profile_name: msg.profile_name ?? null,
        device_id: msg.device_id ?? null,
        token_state: msg.token_state ?? null,
        detail: msg.detail ?? null,
      });
    });

    port.onDisconnect.addListener(() => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      // Reading lastError marks it handled; the message is generic, never the token.
      const err = chrome.runtime.lastError ? chrome.runtime.lastError.message : null;
      finish({ ui: "app_unavailable", status: "host_unavailable", detail: err || "native host disconnected" });
    });

    port.postMessage({
      type: "hello",
      ext_version: chrome.runtime.getManifest().version,
      browser: null,
      mapping_token: token,
    });
  });
  return inFlight;
}

// One automatic retry when the app did not answer in time (e.g. the first launch of a freshly
// installed/updated host binary can be slow while macOS verifies it).
async function handshakeWithRetry() {
  const s = await handshake();
  if (s && s.status === "timeout") {
    await new Promise((r) => setTimeout(r, 3000));
    return handshake();
  }
  return s;
}

chrome.runtime.onInstalled.addListener(() => { handshakeWithRetry(); });
chrome.runtime.onStartup.addListener(() => { handshakeWithRetry(); });
chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
  if (msg && msg.type === "reconnect") {
    handshake().then((s) => sendResponse(s));
    return true; // async response
  }
  if (msg && msg.type === "get_status") {
    chrome.storage.local.get(STATUS_KEY).then((r) => sendResponse(r[STATUS_KEY] ?? null));
    return true;
  }
  return false;
});

handshakeWithRetry();
