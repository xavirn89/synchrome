// Synchrome companion extension: handshake + bookmarks capture (upload only).
//
// - Reads the mapping token the Synchrome app seeded into chrome.storage.local and sends it
//   in `hello`; bookmark messages are sent only after the app answers connected (or
//   connected_offline). The token is never logged or shown.
// - Captures chrome.bookmarks: a full snapshot at every service-worker start (reconciles
//   anything missed while not running) and numbered live events (epoch = this worker's
//   lifetime). Events are batched (debounce) and dropped only after the app acks them, i.e.
//   after they are durable in the app's queue. The extension holds no credentials and never
//   talks to Synchrome Cloud.
// - Captures chrome.management (other extensions: id, name, version, enabled, type,
//   installType, mayDisable; updateUrl only so the app can classify the store, never
//   published) the same way, and executes only what the app plans: setEnabled, an uninstall
//   the user confirmed in the app, or opening an official store page the user asked for.
//   Conflict decisions live in the app; this extension never decides anything.
// - Stores ONLY status (no token, no titles, no URLs) under "synchromeStatus".

const HOST = "com.synchrome.host";
const TOKEN_KEY = "mappingToken";
const STATUS_KEY = "synchromeStatus";
const HELLO_TIMEOUT_MS = 30000;
const ACK_TIMEOUT_MS = 45000; // the app may publish (≤ 20 s) before reading the next frame
const DEBOUNCE_MS = 2000;
const MAX_PART_BYTES = 192 * 1024; // app frame limit is 256 KiB
const MAX_EVENTS_PER_BATCH = 500; // app limit 1000
const MAX_QUEUE = 5000; // beyond this a snapshot is cheaper than the events
const MAX_URL_BYTES = 64 * 1024; // app does not capture longer URLs
const MAX_TITLE_CHARS = 4096;
const ALARM = "synchrome-sync";
const SNAPSHOT_HASH_KEY = "lastAckedTreeHash"; // chrome.storage.session; not sensitive

const epoch = crypto.randomUUID();
let counter = 0; // event number within this epoch
let queue = []; // [{ n, kind, ... }]
let needSnapshot = true; // every worker start reconciles the full tree
let importing = false;
let suppressUntilSnapshot = false; // see push() and executeApply()
let suppressedEvents = 0;
let flushTimer = null;
let running = null;
let lastStatus = null;

async function saveStatus(patch) {
  lastStatus = { ...(lastStatus || {}), ...patch, at: Date.now() };
  await chrome.storage.local.set({ [STATUS_KEY]: lastStatus });
  return lastStatus;
}

// ---------------------------------------------------------------- capture
function utf8Len(s) {
  return s ? new TextEncoder().encode(s).length : 0;
}
function flat(node) {
  const title = (node.title || "").length > MAX_TITLE_CHARS ? node.title.slice(0, MAX_TITLE_CHARS) : node.title || "";
  return {
    id: node.id,
    parent_id: node.parentId ?? null,
    index: node.index ?? null,
    title,
    url: node.url ?? null,
    date_added: node.dateAdded ?? null,
    folder_type: node.folderType ?? null,
    syncing: node.syncing ?? null,
    unmodifiable: node.unmodifiable ?? null,
  };
}
function capturable(node) {
  return !(node.url && utf8Len(node.url) > MAX_URL_BYTES);
}

function push(ev) {
  // Events while an apply is in progress (and until the reconcile snapshot that follows it
  // is taken) are dropped: that snapshot captures the resulting browser state — the applied
  // remote changes (echoes, no diff) and any concurrent user edit (a real diff) alike.
  if (suppressUntilSnapshot) {
    suppressedEvents += 1; // diagnostics: echoes of applies (and anything covered by the next snapshot)
    return;
  }
  if (importing) return;
  counter += 1;
  queue.push({ n: counter, ...ev });
  if (queue.length > MAX_QUEUE) {
    queue = [];
    needSnapshot = true;
  }
  schedule(DEBOUNCE_MS);
}

chrome.bookmarks.onCreated.addListener((id, node) => {
  if (capturable(node)) push({ kind: "created", id, node: flat(node) });
});
chrome.bookmarks.onChanged.addListener((id, info) => {
  push({ kind: "changed", id, title: info.title ?? null, url: info.url ?? null });
});
chrome.bookmarks.onMoved.addListener((id, info) => {
  push({ kind: "moved", id, parent_id: info.parentId, index: info.index });
});
chrome.bookmarks.onRemoved.addListener((id) => {
  push({ kind: "removed", id });
});
chrome.bookmarks.onChildrenReordered.addListener((id, info) => {
  push({ kind: "reordered", id, child_ids: info.childIds });
});
chrome.bookmarks.onImportBegan.addListener(() => {
  importing = true;
});
chrome.bookmarks.onImportEnded.addListener(() => {
  importing = false;
  queue = [];
  needSnapshot = true; // one reconcile instead of thousands of events
  schedule(DEBOUNCE_MS);
});

async function snapshotParts() {
  // From here on events are queued again; anything earlier is covered by this tree.
  suppressUntilSnapshot = false;
  const covers = counter;
  const [root] = await chrome.bookmarks.getTree();
  const nodes = [];
  const walk = (n) => {
    if (n.id !== root.id && capturable(n)) nodes.push(flat(n));
    for (const c of n.children || []) walk(c);
  };
  walk(root);
  const parts = [];
  let cur = [];
  let bytes = 0;
  for (const n of nodes) {
    const sz = utf8Len(JSON.stringify(n)) + 1;
    if (cur.length && bytes + sz > MAX_PART_BYTES) {
      parts.push(cur);
      cur = [];
      bytes = 0;
    }
    cur.push(n);
    bytes += sz;
  }
  parts.push(cur);
  return { covers, parts };
}

function nextBatch() {
  const out = [];
  let bytes = 0;
  for (const ev of queue) {
    const sz = utf8Len(JSON.stringify(ev)) + 1;
    if (out.length && (out.length >= MAX_EVENTS_PER_BATCH || bytes + sz > MAX_PART_BYTES)) break;
    out.push(ev);
    bytes += sz;
  }
  return out;
}

// ---------------------------------------------------------------- extensions capture
const EXT_HASH_KEY = "lastAckedExtHash"; // chrome.storage.session
const EXT_EVENTS_PER_BATCH = 200; // app limit 500
const EXT_ITEMS_PER_PART = 400; // app limit 1000
const SELF_ID = chrome.runtime.id;
let extCounter = 0;
let extQueue = [];
let extNeedSnapshot = true;
let extSuppress = false; // from the start of an extensions apply until the reconcile snapshot

function extInfo(e) {
  return {
    id: e.id,
    name: (e.name || "").slice(0, 256),
    version: (e.version || "").slice(0, 64),
    enabled: !!e.enabled,
    type: e.type,
    install_type: e.installType,
    may_disable: !!e.mayDisable,
    update_url: e.updateUrl ? e.updateUrl.slice(0, 512) : null,
    disabled_reason: e.disabledReason ?? null,
  };
}
// The app classifies again (apps, policy-installed, ids); never report ourselves.
const extRelevant = (e) => !!e && e.id !== SELF_ID && e.type === "extension";

function pushExt(ev) {
  if (extSuppress) {
    suppressedEvents += 1; // echoes of an extensions apply (the reconcile snapshot covers them)
    return;
  }
  extCounter += 1;
  extQueue.push({ n: extCounter, ...ev });
  if (extQueue.length > 1000) {
    extQueue = [];
    extNeedSnapshot = true;
  }
  schedule(DEBOUNCE_MS);
}
chrome.management.onInstalled.addListener((e) => {
  if (extRelevant(e)) pushExt({ kind: "installed", info: extInfo(e) });
});
chrome.management.onUninstalled.addListener((id) => {
  if (id !== SELF_ID) pushExt({ kind: "uninstalled", id });
});
chrome.management.onEnabled.addListener((e) => {
  if (extRelevant(e)) pushExt({ kind: "enabled", info: extInfo(e) });
});
chrome.management.onDisabled.addListener((e) => {
  if (extRelevant(e)) pushExt({ kind: "disabled", info: extInfo(e) });
});

// ---------------------------------------------------------------- native port
function openPort() {
  const port = chrome.runtime.connectNative(HOST);
  const waiters = [];
  const state = { closed: false, reason: null, errors: [] };
  port.onMessage.addListener((msg) => {
    if (!msg) return;
    if (msg.type === "sync_status" && msg.bookmarks) {
      saveStatus({ bookmarks: msg.bookmarks, bookmarks_at: Date.now() });
      if (msg.bookmarks.state === "not_synced") {
        // the app has no capture for this profile (new mapping, local state reset)
        needSnapshot = true;
        extNeedSnapshot = true;
        chrome.storage.session.remove([SNAPSHOT_HASH_KEY, EXT_HASH_KEY]);
      }
    }
    if (msg.type === "sync_status" && msg.extensions) {
      saveStatus({ extensions: msg.extensions });
    }
    if (msg.type === "error") state.errors.push(msg.code);
    const i = waiters.findIndex((w) => w.match(msg));
    if (i >= 0) waiters.splice(i, 1)[0].resolve(msg);
  });
  port.onDisconnect.addListener(() => {
    state.closed = true;
    // Reading lastError marks it handled; the message is generic, never the token.
    state.reason = chrome.runtime.lastError ? chrome.runtime.lastError.message : null;
    waiters.splice(0).forEach((w) => w.resolve(null));
  });
  return {
    state,
    post(m) {
      if (!state.closed) port.postMessage(m);
    },
    wait(match, ms) {
      if (state.closed) return Promise.resolve(null);
      return new Promise((resolve) => {
        const w = { match, resolve };
        waiters.push(w);
        setTimeout(() => {
          const i = waiters.indexOf(w);
          if (i >= 0) {
            waiters.splice(i, 1);
            resolve(null);
          }
        }, ms);
      });
    },
    close() {
      try {
        port.disconnect();
      } catch (_) {}
    },
  };
}

const isAck = (id) => (m) => m.type === "bookmarks_ack" && m.batch_id === id;
const isExtAck = (id) => (m) => m.type === "extensions_ack" && m.batch_id === id;

async function treeHash(parts) {
  const bytes = new TextEncoder().encode(JSON.stringify(parts));
  const d = await crypto.subtle.digest("SHA-256", bytes);
  return Array.from(new Uint8Array(d), (b) => b.toString(16).padStart(2, "0")).join("");
}

// Skips the transfer when the tree is identical to the last one the app acknowledged in this
// browser session (MV3 workers restart often; chrome.storage.session survives that, not a
// browser restart). force = the app asked for a resync.
async function sendSnapshot(p, force = false) {
  const { covers, parts } = await snapshotParts();
  const hash = await treeHash(parts);
  const prev = (await chrome.storage.session.get(SNAPSHOT_HASH_KEY))[SNAPSHOT_HASH_KEY];
  if (!force && prev === hash) {
    needSnapshot = false;
    queue = queue.filter((e) => e.n > covers);
    return true;
  }
  const sid = crypto.randomUUID();
  let last = null;
  let status = null;
  for (let i = 0; i < parts.length; i++) {
    // Register the status waiter BEFORE sending the last part: the app may answer the ack and
    // the status back to back, and a waiter registered later would miss it.
    if (i === parts.length - 1) status = statusAfter(p);
    p.post({ type: "bookmarks_snapshot", snapshot_id: sid, epoch, covers_through: covers, part: i, parts: parts.length, nodes: parts[i] });
    last = await p.wait(isAck(`${sid}:${i}`), ACK_TIMEOUT_MS);
    if (!last || last.result === "rejected") return false;
  }
  if (last.result !== "applied") return false;
  await chrome.storage.session.set({ [SNAPSHOT_HASH_KEY]: hash });
  needSnapshot = false;
  queue = queue.filter((e) => e.n > covers); // already reflected in the tree
  await status;
  return true;
}

// After an applied/duplicate ack the app publishes (bounded) and then sends sync_status.
// Keep the port open until then: the browser terminates the host shortly after a disconnect.
const STATUS_TIMEOUT_MS = 30000;
function statusAfter(p) {
  return p.wait((m) => m.type === "sync_status", STATUS_TIMEOUT_MS);
}

async function sendEvents(p) {
  let resynced = false;
  while (queue.length) {
    const batch = nextBatch();
    const id = `${epoch}:${batch[0].n}`;
    const status = statusAfter(p); // before posting, see sendSnapshot
    p.post({ type: "bookmarks_events", epoch, batch_id: id, events: batch });
    const ack = await p.wait(isAck(id), ACK_TIMEOUT_MS);
    if (!ack || ack.result === "rejected") return false;
    if (ack.result === "resync_required") {
      if (resynced) return false;
      resynced = true;
      needSnapshot = true;
      if (!(await sendSnapshot(p, true))) return false;
      continue;
    }
    const last = batch[batch.length - 1].n;
    queue = queue.filter((e) => e.n > last);
    await status;
  }
  return true;
}

// ---------------------------------------------------------------- extensions transport
async function sendExtSnapshot(p, force = false) {
  extSuppress = false;
  const covers = extCounter;
  const items = (await chrome.management.getAll()).filter(extRelevant).map(extInfo);
  items.sort((a, b) => (a.id < b.id ? -1 : 1));
  const hash = await treeHash(items);
  const prev = (await chrome.storage.session.get(EXT_HASH_KEY))[EXT_HASH_KEY];
  if (!force && prev === hash) {
    extNeedSnapshot = false;
    extQueue = extQueue.filter((e) => e.n > covers);
    return true;
  }
  const parts = [];
  for (let i = 0; i < items.length; i += EXT_ITEMS_PER_PART) parts.push(items.slice(i, i + EXT_ITEMS_PER_PART));
  if (!parts.length) parts.push([]);
  const sid = crypto.randomUUID();
  let last = null;
  let status = null;
  for (let i = 0; i < parts.length; i++) {
    if (i === parts.length - 1) status = statusAfter(p);
    p.post({ type: "extensions_snapshot", snapshot_id: sid, epoch, covers_through: covers, part: i, parts: parts.length, items: parts[i] });
    last = await p.wait(isExtAck(`${sid}:${i}`), ACK_TIMEOUT_MS);
    if (!last || last.result === "rejected") return false;
  }
  if (last.result !== "applied") return false;
  await chrome.storage.session.set({ [EXT_HASH_KEY]: hash });
  extNeedSnapshot = false;
  extQueue = extQueue.filter((e) => e.n > covers);
  await status;
  return true;
}

async function sendExtEvents(p) {
  while (extQueue.length) {
    const batch = extQueue.slice(0, EXT_EVENTS_PER_BATCH);
    const id = `x${epoch}:${batch[0].n}`;
    const status = statusAfter(p);
    p.post({ type: "extensions_events", epoch, batch_id: id, events: batch });
    const ack = await p.wait(isExtAck(id), ACK_TIMEOUT_MS);
    if (!ack || ack.result === "rejected") return false;
    const last = batch[batch.length - 1].n;
    extQueue = extQueue.filter((e) => e.n > last);
    await status;
  }
  return true;
}

const STORE_PAGES = ["https://chromewebstore.google.com/detail/", "https://microsoftedge.microsoft.com/addons/detail/"];

function extError(e) {
  const m = String((e && e.message) || e);
  if (/gesture/i.test(m)) return "gesture_required";
  if (/no extension|failed to find|not found|doesn't exist/i.test(m)) return "not_found";
  if (/cannot be (modified|disabled)|not allowed|policy/i.test(m)) return "protected";
  return "failed";
}

// Execute only the explicit operations the app planned. Each is idempotent against the live
// state (setEnabled compares first; an extension already gone is "not_found").
async function executeExtApply(ops) {
  extSuppress = true;
  extNeedSnapshot = true;
  const results = [];
  for (const op of ops) {
    try {
      switch (op.action) {
        case "set_enabled": {
          const info = await chrome.management.get(op.id);
          if (info.enabled !== op.enabled) {
            if (!info.mayDisable) throw new Error("cannot be modified");
            await chrome.management.setEnabled(op.id, op.enabled);
          }
          break;
        }
        case "uninstall":
          // the user confirmed "Remove here" in the Synchrome app
          await chrome.management.uninstall(op.id, { showConfirmDialog: false });
          break;
        case "open_install":
          if (!STORE_PAGES.some((p) => op.url.startsWith(p) && op.url.slice(p.length) === op.id)) throw new Error("bad store url");
          await chrome.tabs.create({ url: op.url, active: true });
          break;
        default:
          throw new Error("unknown action");
      }
      results.push({ op_id: op.op_id, ok: true });
    } catch (e) {
      results.push({ op_id: op.op_id, ok: false, error: extError(e) });
    }
  }
  return results;
}

async function extPullAndApply(p) {
  for (let round = 0; round < 5; round++) {
    const pullId = crypto.randomUUID();
    const status = statusAfter(p);
    p.post({ type: "extensions_pull", pull_id: pullId });
    const plan = await p.wait((m) => m.type === "extensions_apply" && m.pull_id === pullId, ACK_TIMEOUT_MS);
    if (!plan) return false;
    await status;
    // what Chromium refused without a gesture: the popup offers it as a click
    await saveStatus({ ext_user_actions: plan.user_actions || [] });
    if (!plan.ops || !plan.ops.length) return true;
    const results = await executeExtApply(plan.ops);
    const st = statusAfter(p);
    p.post({ type: "extensions_applied", apply_id: plan.apply_id, results });
    const ack = await p.wait(isExtAck(plan.apply_id), ACK_TIMEOUT_MS);
    if (!ack) return false;
    await st;
    if (!(await sendExtSnapshot(p, true))) return false;
  }
  return true;
}

// One connection: hello, then snapshot/events, or sync_now when there is nothing to send.
async function runOnce(syncNow) {
  const stored = await chrome.storage.local.get(TOKEN_KEY);
  const token = typeof stored[TOKEN_KEY] === "string" ? stored[TOKEN_KEY] : null;
  let p;
  try {
    p = openPort();
  } catch (e) {
    return saveStatus({ ui: "app_unavailable", status: "host_unavailable", detail: "native host could not be started" });
  }
  try {
    p.post({ type: "hello", ext_version: chrome.runtime.getManifest().version, browser: null, mapping_token: token });
    const hello = await p.wait((m) => m.type === "hello_result", HELLO_TIMEOUT_MS);
    if (!hello) {
      const gone = p.state.closed;
      return saveStatus({
        ui: "app_unavailable",
        status: gone ? "host_unavailable" : "timeout",
        detail: gone ? p.state.reason || "native host disconnected" : "no answer from the Synchrome app",
      });
    }
    await saveStatus({
      ui: hello.ui_state,
      status: hello.status,
      mapped: !!hello.mapped,
      profile_id: hello.profile_id ?? null,
      profile_name: hello.profile_name ?? null,
      device_id: hello.device_id ?? null,
      token_state: hello.token_state ?? null,
      detail: hello.detail ?? null,
    });
    if (hello.status !== "connected" && hello.status !== "connected_offline") {
      queue = []; // not linked: the snapshot after linking covers everything
      needSnapshot = true;
      extQueue = [];
      extNeedSnapshot = true;
      await chrome.storage.session.remove([SNAPSHOT_HASH_KEY, EXT_HASH_KEY]);
      return lastStatus;
    }
    let sent = false;
    if (needSnapshot) {
      if (!(await sendSnapshot(p))) return lastStatus;
      sent = true;
    }
    if (queue.length) {
      if (!(await sendEvents(p))) return lastStatus;
      sent = true;
    }
    if (extNeedSnapshot) {
      if (!(await sendExtSnapshot(p))) return lastStatus;
      sent = true;
    }
    if (extQueue.length) {
      if (!(await sendExtEvents(p))) return lastStatus;
      sent = true;
    }
    if (hello.status === "connected") {
      // local changes are already with the app; now download and apply remote ones
      await pullAndApply(p);
      await extPullAndApply(p);
    } else if (!sent && syncNow) {
      p.post({ type: "sync_now" });
      await p.wait((m) => m.type === "sync_status", ACK_TIMEOUT_MS);
    }
    return lastStatus;
  } finally {
    p.close();
  }
}

// ---------------------------------------------------------------- remote apply
const MAX_PULL_ROUNDS = 20;

function applyError(e) {
  const m = String((e && e.message) || e);
  if (/can't find/i.test(m)) return "not_found";
  if (/non-empty/i.test(m)) return "not_empty";
  return "failed";
}

// Execute explicit operations from the app, in order. Each is idempotent against the live
// tree (rename/URL/move/reorder compare first; a missing node on delete is "not_found",
// which the app treats as done). Folder deletes are NOT recursive: a folder that still holds
// something the app did not delete fails with "not_empty" and becomes a conflict.
async function executeApply(ops) {
  suppressUntilSnapshot = true;
  needSnapshot = true;
  const made = new Map();
  const ref = (r) => (r && r.id) || made.get(r && r.op) || null;
  const results = [];
  for (const op of ops) {
    try {
      let id = null;
      switch (op.action) {
        case "create_folder":
        case "create_bookmark": {
          const parentId = ref(op.parent);
          if (!parentId) throw new Error("Can't find parent");
          const spec = { parentId, title: op.title };
          if (op.action === "create_bookmark") spec.url = op.url;
          const n = await chrome.bookmarks.create(spec);
          id = n.id;
          made.set(op.op_id, id);
          break;
        }
        case "rename": {
          const [n] = await chrome.bookmarks.get(op.id);
          if (n.title !== op.title) await chrome.bookmarks.update(op.id, { title: op.title });
          break;
        }
        case "set_url": {
          const [n] = await chrome.bookmarks.get(op.id);
          if (n.url !== op.url) await chrome.bookmarks.update(op.id, { url: op.url });
          break;
        }
        case "move": {
          const [n] = await chrome.bookmarks.get(op.id);
          const parentId = ref(op.parent);
          if (!parentId) throw new Error("Can't find parent");
          if (n.parentId !== parentId) await chrome.bookmarks.move(op.id, { parentId });
          break;
        }
        case "reorder": {
          const parentId = ref(op.parent);
          if (!parentId) throw new Error("Can't find parent");
          for (let i = 0; i < op.children.length; i++) {
            const c = ref(op.children[i]);
            if (!c) continue;
            const kids = await chrome.bookmarks.getChildren(parentId);
            const cur = kids.findIndex((k) => k.id === c);
            if (cur < 0 || cur === i) continue;
            // chrome.bookmarks.move counts the index before removing the node
            await chrome.bookmarks.move(c, { parentId, index: cur < i ? i + 1 : i });
          }
          break;
        }
        case "delete":
          await chrome.bookmarks.remove(op.id); // non-recursive by design
          break;
        default:
          throw new Error("unknown action");
      }
      results.push({ op_id: op.op_id, ok: true, id });
    } catch (e) {
      results.push({ op_id: op.op_id, ok: false, error: applyError(e) });
    }
  }
  return results;
}

async function pullAndApply(p) {
  for (let round = 0; round < MAX_PULL_ROUNDS; round++) {
    const pullId = crypto.randomUUID();
    const status = statusAfter(p);
    p.post({ type: "bookmarks_pull", pull_id: pullId });
    const plan = await p.wait((m) => m.type === "bookmarks_apply" && m.pull_id === pullId, ACK_TIMEOUT_MS);
    if (!plan) return false;
    await status;
    if (!plan.ops || !plan.ops.length) return true;
    const results = await executeApply(plan.ops);
    const st = statusAfter(p);
    p.post({ type: "bookmarks_applied", apply_id: plan.apply_id, results });
    const ack = await p.wait(isAck(plan.apply_id), ACK_TIMEOUT_MS);
    if (!ack) return false;
    await st;
    // reconcile right away: the snapshot shows the app the browser as it now is
    if (!(await sendSnapshot(p, true))) return false;
    if (!plan.more) return true;
  }
  return true;
}

function run(syncNow = false) {
  if (running) return running;
  running = (async () => {
    try {
      return await runOnce(syncNow);
    } finally {
      running = null;
      // events that arrived while we were sending
      if (queue.length || needSnapshot || extQueue.length || extNeedSnapshot) schedule(DEBOUNCE_MS, true);
    }
  })();
  return running;
}

let backoffMs = 0;
function schedule(ms, afterRun = false) {
  if (flushTimer) clearTimeout(flushTimer);
  flushTimer = setTimeout(async () => {
    flushTimer = null;
    const pendingWork = () => queue.length + extQueue.length + (needSnapshot ? 1 : 0) + (extNeedSnapshot ? 1 : 0);
    const before = pendingWork();
    await run(false);
    const after = pendingWork();
    // nothing moved (app unavailable / not linked): back off; the alarm keeps retrying
    if (after && after >= before) {
      backoffMs = Math.min(backoffMs ? backoffMs * 2 : 30000, 300000);
      schedule(backoffMs);
    } else {
      backoffMs = 0;
    }
  }, afterRun ? Math.max(ms, 500) : ms);
}

// Remote changes are polled: the alarm is the wake-up (no Realtime yet); the app's own
// background loop downloads and materialises in between, so this only applies.
chrome.alarms.create(ALARM, { periodInMinutes: 1 });
chrome.alarms.onAlarm.addListener((a) => {
  if (a.name === ALARM) run(true);
});
chrome.runtime.onInstalled.addListener(() => schedule(0));
chrome.runtime.onStartup.addListener(() => schedule(0));
chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
  if (msg && msg.type === "reconnect") {
    run(true).then((s) => sendResponse(s));
    return true; // async response
  }
  if (msg && msg.type === "get_status") {
    chrome.storage.local.get(STATUS_KEY).then((r) => sendResponse(r[STATUS_KEY] ?? null));
    return true;
  }
  return false;
});

schedule(0);
