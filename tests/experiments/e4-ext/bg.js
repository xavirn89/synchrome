// Write-once probe: only writes if values are absent, so a restored store is not clobbered.
// Always records a readback marker reflecting what it currently reads.
async function run() {
  const existing = await chrome.storage.local.get("e4_local_key");
  let stamp;
  if (!existing.e4_local_key) {
    stamp = "E4-" + Date.now();
    await chrome.storage.local.set({ e4_local_key: { v: 1, note: "local-value", stamp } });
    await chrome.storage.sync.set({ e4_sync_key: { v: 2, note: "sync-value", stamp } });
    await new Promise((resolve, reject) => {
      const req = indexedDB.open("e4db", 1);
      req.onupgradeneeded = () => req.result.createObjectStore("kv");
      req.onsuccess = () => {
        const tx = req.result.transaction("kv", "readwrite");
        tx.objectStore("kv").put({ v: 4, note: "idb-value", stamp }, "e4_idb_key");
        tx.oncomplete = () => resolve();
        tx.onerror = () => reject(tx.error);
      };
      req.onerror = () => reject(req.error);
    });
  } else {
    stamp = existing.e4_local_key.stamp;
  }
  await chrome.storage.session.set({ e4_session_key: { v: 3 } });
  const local = await chrome.storage.local.get("e4_local_key");
  const sync = await chrome.storage.sync.get("e4_sync_key");
  await chrome.storage.local.set({ e4_readback: { readStamp: local.e4_local_key && local.e4_local_key.stamp, syncStamp: sync.e4_sync_key && sync.e4_sync_key.stamp } });
  console.log("E4 readback stamp:", stamp);
}
chrome.runtime.onInstalled.addListener(run);
chrome.runtime.onStartup.addListener(run);
run();
