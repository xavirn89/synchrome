import test from "node:test";
import assert from "node:assert/strict";
import { localKey, validateName, connectionConflictCount, connectionState, profileState, groupConnections, disconnectAndDelete, sameDeviceConflict, cloudState, attentionSummary } from "../src/lib/product.js";
import { createPreviewBackend } from "../src/lib/preview.js";

const ready = (overrides = {}) => ({
  id: "m1", profile_id: "p1", device_id: "mac1", device_name: "MacBook Pro", is_this_device: true,
  display_status: "extension_confirmed", local_error: null,
  bookmarks: { state: "up_to_date", last_upload_at: 100, pending: 0, download: { state: "up_to_date", conflicts: 0, remote_pending: 0, resolving: 0 } },
  extensions: { conflicts: 0, user_action: 0, missing: 0, differences: 0 },
  ...overrides,
});

test("cloud health distinguishes pending probes, partial service failure and confirmed availability", () => {
  assert.equal(cloudState(null).tone, "info");
  assert.equal(cloudState({ configured: true, reachable: true }).label, "Checking cloud…");
  assert.equal(cloudState({ configured: false }).tone, "warning");
  assert.equal(cloudState({ configured: true, reachable: false }).label, "Cloud unavailable");
  const connected = { configured: true, reachable: true, auth_ok: true, rest_ok: true, storage_ok: true };
  assert.equal(cloudState(connected).tone, "positive");
  for (const field of ["auth_ok", "rest_ok", "storage_ok"]) {
    const partial = cloudState({ ...connected, [field]: false });
    assert.equal(partial.label, "Cloud partially available");
    assert.equal(partial.tone, "warning");
  }
});

test("mixed attention consistently describes and targets conflicts regardless of connection order", () => {
  const setup = ready({ id: "setup", profile_id: "setup-profile", display_status: "token_written", bookmarks: null });
  const conflict = ready({ id: "conflict", profile_id: "conflict-profile", bookmarks: { download: { conflicts: 1 } } });
  for (const mappings of [[setup, conflict], [conflict, setup]]) {
    const attention = attentionSummary(mappings);
    assert.equal(attention.count, 2);
    assert.equal(attention.action, "conflicts");
    assert.equal(attention.profileId, "conflict-profile");
    assert.match(attention.description, /Other connections also need setup or recovery/);
    assert.doesNotMatch(attention.description, /Everything else keeps syncing/);
  }
});

test("confirmed connection recovery is not described as initial setup", () => {
  assert.equal(attentionSummary([ready()]), null);
  const error = ready({ bookmarks: { state: "error" } });
  assert.match(attentionSummary([error]).description, /could not send or apply/);
  const offline = attentionSummary([ready()], false);
  assert.match(offline.description, /queued/);
  assert.equal(offline.action, "profile");
});

test("local picker identities include the user-data folder", () => {
  const profile = { dir_name: "Default" };
  assert.notEqual(localKey({ kind: "chrome", user_data_dir: "/Preview/A" }, profile), localKey({ kind: "chrome", user_data_dir: "/Preview/B" }, profile));
  assert.equal(localKey({ kind: "helium", user_data_dir: "/Preview/Helium" }, profile), "helium//Preview/Helium/Default");
});

test("profile names are trimmed, bounded, normalized and duplicate checked excluding the edited profile", () => {
  const profiles = [{ id: "p1", name: "Work" }, { id: "p2", name: "Café" }];
  assert.match(validateName("   ", profiles), /name/);
  assert.match(validateName("x".repeat(81), profiles), /80/);
  assert.equal(validateName("x".repeat(80), profiles), "");
  assert.match(validateName(" work ", profiles), /already have/);
  assert.match(validateName("Cafe\u0301", profiles), /already have/);
  assert.equal(validateName("Work", profiles, "p1"), "");
  assert.equal(validateName("Reading", profiles), "");
});

test("remote connections never imply live remote sync success or expose a local action", () => {
  const mapping = ready({ is_this_device: false, bookmarks: { state: "error" } });
  const result = connectionState(mapping, false);
  assert.equal(result.label, "Connected on another Mac");
  assert.equal(result.tone, "neutral");
  assert.equal(result.action, undefined);
  assert.match(result.description, /there to check/);
});

test("local conflicts from all datasets outrank ready and offline states", () => {
  const sources = [
    { bookmarks: { state: "up_to_date", download: { conflicts: 1 } } },
    { extensions: { conflicts: 1 } },
    { preferences: { mode: "recommended", conflicts: 1 } },
    { settings: [{ mode: "local", conflicts: 1 }] },
    { extension_settings: { ext: { mode: "local_sync", conflicts: 1 } } },
  ];
  for (const source of sources) {
    const result = connectionState(ready(source), false);
    assert.equal(result.label, "Review conflict");
    assert.equal(result.action, "conflicts");
    assert.equal(result.tone, "warning");
  }
});

test("profile-wide conflict totals do not count extension and preference subsets twice", () => {
  const mapping = ready({
    bookmarks: { state: "up_to_date", download: { conflicts: 3 } },
    extensions: { conflicts: 1 },
    preferences: { mode: "recommended", conflicts: 1 },
    extension_settings: [{ mode: "local_sync", conflicts: 1 }],
  });
  assert.equal(connectionConflictCount(mapping), 3);
  assert.match(connectionState(mapping).description, /^3 changes need/);
  const staleTotal = { ...mapping, bookmarks: { download: { conflicts: 0 } } };
  assert.equal(connectionConflictCount(staleTotal), 3, "fresh dataset states can expose a conflict before the next mapping refresh");
});

test("browser-open connection failures give a recovery step instead of a technical error", () => {
  for (const mapping of [ready({ display_status: "pending_local_write" }), ready({ display_status: "error", local_error: "browser is open — close it" })]) {
    const result = connectionState(mapping);
    assert.equal(result.label, "Close browser to connect");
    assert.equal(result.action, "retry");
  }
  assert.equal(connectionState(ready({ display_status: "error", local_error: "verification failed" })).label, "Needs attention");
});

test("token-written and confirmed connections distinguish companion setup, first sync and initial upload", () => {
  assert.equal(connectionState(ready({ display_status: "token_written", bookmarks: null })).label, "Finish setup");
  assert.equal(connectionState(ready({ bookmarks: null })).label, "First sync pending");
  assert.equal(connectionState(ready({ bookmarks: { state: "not_synced", last_upload_at: null } })).label, "First sync pending");
  assert.equal(connectionState(ready({ bookmarks: { state: "initial_upload", last_upload_at: null } })).label, "First sync in progress");
});

test("offline uses queued-change wording and never reports up to date", () => {
  assert.equal(connectionState(ready(), false).label, "Offline · changes saved");
  assert.match(connectionState(ready(), false).description, /queued/);
  assert.equal(connectionState(ready({ bookmarks: { state: "offline" } })).tone, "warning");
});

test("preferences and settings surface compatibility, waiting and errors without misdirecting to Retry link", () => {
  for (const source of [
    { prefs: { mode: "recommended", state: "version_mismatch" } },
    { extension_settings: [{ mode: "local_sync", state: "version_mismatch" }] },
    { settings: { ext: { mode: "local", state: "unsupported" } } },
  ]) {
    const result = connectionState(ready(source));
    assert.equal(result.label, "Check browser compatibility");
    assert.equal(result.action, "details");
  }
  const waiting = connectionState(ready({ preferences: { mode: "recommended", state: "waiting_for_browser_close" } }));
  assert.equal(waiting.label, "Close browser to finish sync");
  assert.equal(waiting.action, "details");
  assert.match(waiting.description, /Bookmarks can continue/);
  const failed = connectionState(ready({ settings: [{ mode: "local_sync", state: "error" }] }));
  assert.equal(failed.label, "Needs attention");
  assert.equal(failed.action, "details");
  assert.equal(connectionState(ready({ preferences: { mode: "off", state: "error" } })).label, "Up to date");
});

test("pending applies and browser user action remain visible", () => {
  assert.equal(connectionState(ready({ bookmarks: { state: "pending_changes", last_upload_at: 100, pending: 3 } })).label, "Syncing changes");
  assert.equal(connectionState(ready({ settings: [{ mode: "local_sync", state: "syncing", pending: 0 }] })).label, "Syncing changes");
  assert.equal(connectionState(ready({ extensions: { user_action: 1 } })).label, "Browser confirmation needed");
  assert.equal(connectionState(ready({ extensions: { missing: 1 } })).label, "Review extensions");
  assert.equal(connectionState(ready()).label, "Up to date");
});

test("bookmark sync failures retry syncing instead of rewriting the companion connection", () => {
  for (const bookmarks of [{ state: "error" }, { state: "up_to_date", download: { state: "error" } }]) {
    const result = connectionState(ready({ bookmarks }));
    assert.equal(result.label, "Needs attention");
    assert.equal(result.action, "sync");
  }
});

test("profile summaries prioritize this Mac's unresolved state and ignore other profiles", () => {
  const profile = { id: "p1" };
  const remote = ready({ id: "remote", is_this_device: false, device_id: "mac2" });
  const conflict = ready({ id: "conflict", extensions: { conflicts: 1 } });
  const unrelated = ready({ profile_id: "p2", display_status: "error" });
  assert.equal(profileState(profile, []).label, "Not connected");
  assert.equal(profileState(profile, [remote, unrelated]).label, "On another Mac");
  assert.equal(profileState(profile, [remote, ready(), conflict, unrelated]).label, "Review conflict");
  assert.equal(profileState(profile, [remote, ready()], false).label, "Offline · changes saved");
  assert.equal(profileState(profile, [ready({ display_status: "token_written", bookmarks: null }), ready()]).label, "Finish setup");
});

test("devices group by id rather than browser count or repeated names, with this Mac first", () => {
  const mappings = [ready({ id: "r1", device_id: "mac2", device_name: "Studio", is_this_device: false }), ready(), ready({ id: "m2" }), ready({ id: "r2", device_id: "mac3", device_name: "Studio", is_this_device: false })];
  const groups = groupConnections(mappings);
  assert.equal(groups.length, 3);
  assert.equal(groups[0].deviceId, "mac1");
  assert.equal(groups[0].isCurrent, true);
  assert.equal(groups[0].connections.length, 2);
  assert.equal(mappings[0].id, "r1", "grouping does not sort the caller's array");
  assert.equal(groups[1].name, groups[2].name);
  assert.notEqual(groups[1].deviceId, groups[2].deviceId);
});

test("v1 mapping rule allows a new Mac and excludes the mapping being changed", () => {
  assert.equal(sameDeviceConflict([ready()], "p1"), true);
  assert.equal(sameDeviceConflict([ready()], "p1", "m1"), false);
  assert.equal(sameDeviceConflict([ready({ is_this_device: false })], "p1"), false);
  assert.equal(sameDeviceConflict([ready()], "p2"), false);
});

test("delete disconnects only associated mappings sequentially before the safe delete command", async () => {
  const calls = [];
  let inFlight = 0;
  const invoke = async (command, args) => {
    assert.equal(inFlight, 0, "mutations must be awaited sequentially");
    inFlight++;
    calls.push([command, args]);
    await Promise.resolve();
    inFlight--;
  };
  const result = await disconnectAndDelete(invoke, "p1", [ready(), ready({ id: "m2", is_this_device: false }), ready({ profile_id: "p2", id: "other" }), ready()]);
  assert.deepEqual(calls, [["unmap_local_profile", { mappingId: "m1" }], ["unmap_local_profile", { mappingId: "m2" }], ["delete_profile", { profileId: "p1" }]]);
  assert.deepEqual(result, { profileId: "p1", profileDeleted: true, completedMappings: ["m1", "m2"] });
});

test("partial disconnect failure leaves the cloud profile intact and reports completed work", async () => {
  const calls = [];
  const cause = new Error("network connection failed");
  const invoke = async (command, args) => {
    calls.push(command);
    if (args.mappingId === "m2") throw cause;
  };
  await assert.rejects(disconnectAndDelete(invoke, "p1", [ready(), ready({ id: "m2" })]), (error) => {
    assert.deepEqual(error.completedMappings, ["m1"]);
    assert.equal(error.profileDeleted, false);
    assert.equal(error.cause, cause);
    assert.match(error.message, /profile was not deleted/);
    return true;
  });
  assert.deepEqual(calls, ["unmap_local_profile", "unmap_local_profile"]);
});

test("cloud deletion failure after disconnect does not report success", async () => {
  const cause = "network connection failed";
  const invoke = async (command) => { if (command === "delete_profile") throw cause; };
  await assert.rejects(disconnectAndDelete(invoke, "p1", [ready()]), (error) => {
    assert.deepEqual(error.completedMappings, ["m1"]);
    assert.equal(error.profileDeleted, false);
    assert.equal(error.cause, cause);
    assert.match(error.message, /could not finish removing/);
    return true;
  });
});

test("a deleted-profile cleanup failure preserves the backend recovery meaning", async () => {
  const cause = new Error("The Synchrome profile was deleted, but cleanup is incomplete. Your local browser profiles and data were not deleted. Storage cleanup failed.");
  const invoke = async (command) => { if (command === "delete_profile") throw cause; };
  await assert.rejects(disconnectAndDelete(invoke, "p1", [ready()]), (error) => {
    assert.equal(error.message, cause.message);
    assert.equal(error.profileDeleted, true);
    assert.deepEqual(error.completedMappings, ["m1"]);
    assert.equal(error.cause, cause);
    return true;
  });
});

test("profile deletion integration keeps local browser data and unrelated profile connections", async () => {
  const api = createPreviewBackend("workspace");
  const before = api.snapshot();
  const profile = before.profiles[0];
  const result = await disconnectAndDelete(api.invoke, profile.id, before.mappings);
  const after = api.snapshot();
  assert.equal(result.profileDeleted, true);
  assert.equal(after.profiles.some((p) => p.id === profile.id), false);
  assert.deepEqual(after.browserData, before.browserData);
  assert.deepEqual(after.browsers, before.browsers);
  assert.deepEqual(after.mappings, before.mappings.filter((m) => m.profile_id !== profile.id));
});
