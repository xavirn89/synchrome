// Records results of E6/E7/E8 probes into chrome.storage.local (read off-disk by the test).
const DUMMY_ID = "DUMMY_ID_PLACEHOLDER";
const results = { started: Date.now() };

async function e8() {
  // E8: read a token the app may have pre-seeded into our own storage.local.
  const r = await chrome.storage.local.get("mappingToken");
  results.e8_mappingToken = r.mappingToken ?? null;
}

async function e7() {
  // E7: try to enable the (disabled) dummy extension from the SW with no user gesture.
  try {
    const before = await chrome.management.get(DUMMY_ID).catch(() => null);
    results.e7_dummy_found = !!before;
    results.e7_enabled_before = before && before.enabled;
    await chrome.management.setEnabled(DUMMY_ID, true);
    const after = await chrome.management.get(DUMMY_ID).catch(() => null);
    results.e7_enabled_after = after && after.enabled;
    results.e7_setEnabled_error = null;
  } catch (e) {
    results.e7_setEnabled_error = String(e && e.message || e);
  }
}

function e6() {
  // E6: record any onVisitRemoved events (expiry vs deletion) during this run.
  results.e6_removed_events = [];
  chrome.history.onVisitRemoved.addListener((rm) => {
    results.e6_removed_events.push({ allHistory: rm.allHistory, urls: (rm.urls || []).slice(0, 5) });
    chrome.storage.local.set({ e678: results });
  });
}

async function run() {
  e6();
  await e8();
  await e7();
  // snapshot of current history size for E6 cross-check
  try {
    const items = await chrome.history.search({ text: "", startTime: 0, maxResults: 100000 });
    results.e6_history_count = items.length;
    results.e6_oldest = items.reduce((m, i) => Math.min(m, i.lastVisitTime || Infinity), Infinity);
  } catch (e) { results.e6_history_err = String(e); }
  await chrome.storage.local.set({ e678: results });
}
chrome.runtime.onStartup.addListener(run);
chrome.runtime.onInstalled.addListener(run);
run();
