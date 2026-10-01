// Records results of E6/E7/E8 probes into chrome.storage.local (read off-disk by the test).
const results = { started: Date.now() };

async function e8() {
  // E8: read a token the app may have pre-seeded into our own storage.local.
  const r = await chrome.storage.local.get("mappingToken");
  results.e8_mappingToken = r.mappingToken ?? null;
}

async function e7() {
  try {
    const all = await chrome.management.getAll();
    const dummy = all.find(e => e.name === "Synchrome Dummy");
    results.e7_dummy_found = !!dummy;
    if (!dummy) return;
    results.e7_dummy_id = dummy.id;
    results.e7_enabled_initial = dummy.enabled;
    results.e7_mayDisable = dummy.mayDisable;
    // try to DISABLE from the SW (no user gesture)
    try { await chrome.management.setEnabled(dummy.id, false); results.e7_disable_error = null; }
    catch (e) { results.e7_disable_error = String(e && e.message || e); }
    const d1 = await chrome.management.get(dummy.id).catch(() => null);
    results.e7_enabled_after_disable = d1 && d1.enabled;
    // try to RE-ENABLE from the SW (no user gesture)
    try { await chrome.management.setEnabled(dummy.id, true); results.e7_enable_error = null; }
    catch (e) { results.e7_enable_error = String(e && e.message || e); }
    const d2 = await chrome.management.get(dummy.id).catch(() => null);
    results.e7_enabled_after_enable = d2 && d2.enabled;
  } catch (e) { results.e7_fatal = String(e && e.message || e); }
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
