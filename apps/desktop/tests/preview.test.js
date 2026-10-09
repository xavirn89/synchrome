import test from "node:test";
import assert from "node:assert/strict";
import { createPreviewBackend, PREVIEW_SCENARIOS } from "../src/lib/preview.js";
import { previewScenarioFor, previewEnabled } from "../src/lib/api.js";

const localArgs = { browserKind: "helium", browserProfileDir: "Profile 2", userDataDir: "/Preview/Helium" };

test("preview requires an explicit development flag and a known scenario", () => {
  for (const scenario of PREVIEW_SCENARIOS) {
    assert.equal(previewScenarioFor(true, `?preview=${scenario}`), scenario);
    assert.equal(previewScenarioFor(false, `?preview=${scenario}`), null);
  }
  assert.equal(previewScenarioFor(true), null);
  assert.equal(previewScenarioFor(true, "?preview=invalid"), null);
  assert.equal(previewEnabled, false, "plain Node never silently switches the production API to preview");
});

test("preview instances are isolated and return copies, including reset", async () => {
  const first = createPreviewBackend("empty");
  const second = createPreviewBackend("empty");
  const profiles = await first.invoke("list_profiles");
  profiles.push({ name: "not stored" });
  await first.invoke("create_profile", { name: "Travel" });
  assert.equal((await first.invoke("list_profiles")).length, 1);
  assert.equal((await second.invoke("list_profiles")).length, 0);
  first.reset("auth");
  assert.equal((await first.invoke("account_status")).signed_in, false);
  assert.throws(() => first.reset("not-a-scenario"), /Unknown preview/);
});

test("auth handles validation, wrong credentials, account creation and sign out without storing passwords", async () => {
  const api = createPreviewBackend("auth");
  await assert.rejects(api.invoke("list_profiles"), /not signed in/);
  await assert.rejects(api.invoke("sign_in", { email: "invalid", password: "example-password" }), /valid email/);
  await assert.rejects(api.invoke("sign_in", { email: "alex@example.test", password: "wrong-password" }), /invalid credentials/);
  await assert.rejects(api.invoke("sign_up", { email: "alex@example.test", password: "short" }), /8 characters/);
  const account = await api.invoke("sign_up", { email: " alex@example.test ", password: "example-password" });
  assert.equal(account.signed_in, true);
  assert.equal(account.email, "alex@example.test");
  assert.equal(JSON.stringify(api.snapshot()).includes("example-password"), false);
  assert.equal((await api.invoke("refresh_session")).needs_refresh, false);
  assert.equal((await api.invoke("sign_out")).signed_in, false);
  await assert.rejects(api.invoke("create_profile", { name: "Travel" }), /not signed in/);
});

test("manual creation trims names, preserves browser data and cannot claim to have synced", async () => {
  const api = createPreviewBackend("empty");
  const before = api.snapshot().browserData;
  const profile = await api.invoke("create_profile", { name: "  Travel  " });
  assert.equal(profile.name, "Travel");
  assert.equal((await api.invoke("list_mappings")).length, 0);
  assert.deepEqual(api.snapshot().browserData, before);
  await assert.rejects(api.invoke("create_profile", { name: "travel" }), /already exists/);
  await assert.rejects(api.invoke("create_profile", { name: " " }), /1–200/);
  await assert.rejects(api.invoke("create_profile", { name: "x".repeat(201) }), /1–200/);
});

test("import creates one profile and mapping; companion confirmation precedes the first sync", async () => {
  const api = createPreviewBackend("empty");
  const localData = api.snapshot().browserData;
  const mapping = await api.invoke("create_and_map_local_profile", { name: "Research", ...localArgs });
  assert.equal((await api.invoke("list_profiles")).length, 1);
  assert.equal((await api.invoke("list_mappings")).length, 1);
  assert.equal(mapping.display_status, "token_written");
  assert.equal(mapping.bookmarks, null);
  assert.equal(await api.invoke("publish_bookmarks", { mappingId: mapping.id }), null);
  const confirmed = api.confirmMapping(mapping.id);
  assert.equal(confirmed.display_status, "extension_confirmed");
  assert.equal(confirmed.bookmarks.state, "not_synced");
  assert.equal(confirmed.bookmarks.last_upload_at, null);
  const synced = await api.invoke("publish_bookmarks", { mappingId: mapping.id });
  assert.equal(synced.state, "up_to_date");
  assert.equal(synced.bookmarks, localData["/Preview/Helium/Profile 2"].bookmarks);
  assert.ok(synced.last_upload_at);
  assert.deepEqual(api.snapshot().browserData, localData);
});

test("duplicate mappings and unavailable import targets fail without leaking an empty profile", async () => {
  const api = createPreviewBackend("empty");
  const mapping = await api.invoke("create_and_map_local_profile", { name: "Research", ...localArgs });
  const before = api.snapshot();
  await assert.rejects(api.invoke("create_and_map_local_profile", { name: "Duplicate", ...localArgs }), /already connected/);
  await assert.rejects(api.invoke("map_local_profile", { profileId: mapping.profile_id, ...localArgs }), /already connected/);
  await assert.rejects(api.invoke("create_and_map_local_profile", { name: "Ghost", ...localArgs, browserProfileDir: "does-not-exist" }), /not detected/);
  await assert.rejects(api.invoke("map_local_profile", { profileId: "missing", ...localArgs, browserProfileDir: "Default" }), /profile not found/);
  assert.deepEqual(api.snapshot(), before);
});

test("v1 never connects two local browsers to the same Synchrome profile", async () => {
  const api = createPreviewBackend("workspace");
  const profile = (await api.invoke("list_profiles"))[0];
  const before = api.snapshot();
  await assert.rejects(api.invoke("map_local_profile", { profileId: profile.id, ...localArgs }), /already has a browser connected/);
  assert.deepEqual(api.snapshot(), before);
});

test("browser-open import is recoverable and never bypasses the pending connection state", async () => {
  const api = createPreviewBackend("empty");
  const mapping = await api.invoke("create_and_map_local_profile", {
    name: "Work", browserKind: "chrome", browserProfileDir: "Profile 1", userDataDir: "/Preview/Google/Chrome",
  });
  assert.equal(mapping.display_status, "error");
  assert.match(mapping.local_error, /browser is open/);
  assert.throws(() => api.confirmMapping(mapping.id), /Write the connection/);
  await assert.rejects(api.invoke("write_mapping_token_local", { mappingId: mapping.id }), /browser is open/);
  api.setBrowserAccess("chrome", "accessible");
  const written = await api.invoke("write_mapping_token_local", { mappingId: mapping.id });
  assert.equal(written.display_status, "token_written");
  assert.equal(written.local_error, null);
  assert.equal(written.bookmarks, null);
});

test("delete refuses connected profiles; disconnect plus safe deletion keeps local profiles and bookmarks intact", async () => {
  const api = createPreviewBackend("workspace");
  const profile = (await api.invoke("list_profiles"))[0];
  const before = api.snapshot();
  await assert.rejects(api.invoke("delete_profile", { profileId: profile.id }), /Unmap it on every device first/);
  assert.deepEqual(api.snapshot(), before, "a refused delete must leave all state intact");
  for (const mapping of before.mappings.filter((m) => m.profile_id === profile.id)) {
    await api.invoke("unmap_local_profile", { mappingId: mapping.id });
  }
  await api.invoke("delete_profile", { profileId: profile.id });
  const after = api.snapshot();
  assert.equal(after.profiles.some((p) => p.id === profile.id), false);
  assert.equal(after.mappings.some((m) => m.profile_id === profile.id), false);
  assert.equal(after.cloudObjects[profile.id], undefined);
  assert.deepEqual(after.browsers, before.browsers);
  assert.deepEqual(after.browserData, before.browserData);
  assert.deepEqual(after.devices, before.devices);
});

test("renaming updates every connection label while keeping stable identifiers", async () => {
  const api = createPreviewBackend("workspace");
  const profile = (await api.invoke("list_profiles"))[0];
  const originalIds = (await api.invoke("list_mappings")).map((m) => m.id);
  const renamed = await api.invoke("rename_profile", { profileId: profile.id, name: "  Home  " });
  assert.equal(renamed.id, profile.id);
  assert.equal(renamed.name, "Home");
  const mappings = await api.invoke("list_mappings");
  assert.deepEqual(mappings.map((m) => m.id), originalIds);
  assert.ok(mappings.filter((m) => m.profile_id === profile.id).every((m) => m.profile_name === "Home"));
});

test("offline keeps queued changes, supports local conflict decisions, and blocks cloud mutations", async () => {
  const api = createPreviewBackend("offline");
  assert.equal((await api.invoke("cloud_status")).reachable, false);
  const mapping = (await api.invoke("list_mappings")).find((m) => m.bookmarks?.download.conflicts);
  const pending = mapping.bookmarks.pending;
  await assert.rejects(api.invoke("create_profile", { name: "Travel" }), /offline/);
  await assert.rejects(api.invoke("publish_bookmarks", { mappingId: mapping.id }), /queued/);
  assert.equal((await api.invoke("list_mappings")).find((m) => m.id === mapping.id).bookmarks.pending, pending);
  const [conflict] = await api.invoke("list_conflicts", { mappingId: mapping.id });
  const [resolved] = await api.invoke("resolve_conflicts", { mappingId: mapping.id, items: [{ conflict_id: conflict.conflict_id, resolution: "keep_local" }] });
  assert.equal(resolved.outcome, "resolved");
  const [history] = await api.invoke("list_conflicts", { mappingId: mapping.id, includeClosed: true });
  assert.equal(history.upload_pending, true);
  api.setOnline(true);
  assert.equal((await api.invoke("publish_bookmarks", { mappingId: mapping.id })).pending, 0);
});

test("preference and extension-settings writes wait for browser close; opt-in and off states persist", async () => {
  const api = createPreviewBackend("workspace");
  const mapping = (await api.invoke("list_mappings")).find((m) => m.is_this_device && m.browser_kind === "chrome");
  const args = { mappingId: mapping.id };
  const [extension] = await api.invoke("list_extensions", args);
  await api.invoke("set_extension_settings_mode", { ...args, extId: extension.ext_id, mode: "local_sync" });
  assert.equal((await api.invoke("list_extension_settings", args))[0].state, "waiting_for_browser_close");
  assert.equal((await api.invoke("extension_settings_sync_now", args)).keys_written, 0);
  await api.invoke("set_browser_preferences_mode", { ...args, mode: "recommended" });
  assert.equal((await api.invoke("browser_preferences_sync_now", args)).paths_written, 0);
  api.setBrowserAccess("chrome", "accessible");
  assert.equal((await api.invoke("extension_settings_sync_now", args)).browser_running, false);
  assert.equal((await api.invoke("browser_preferences_sync_now", args)).browser_running, false);
  assert.equal((await api.invoke("list_extension_settings", args))[0].state, "synced");
  await api.invoke("set_browser_preferences_mode", { ...args, mode: "off" });
  assert.equal((await api.invoke("list_browser_preferences", args)).state, "off");
});

test("opening an extension store does not falsely install an extension", async () => {
  const api = createPreviewBackend("workspace");
  const mapping = (await api.invoke("list_mappings")).find((m) => m.bookmarks);
  const args = { mappingId: mapping.id };
  const missing = (await api.invoke("list_extensions", args)).find((x) => x.state === "missing");
  await api.invoke("extension_action", { ...args, extId: missing.ext_id, action: "install" });
  const opened = (await api.invoke("list_extensions", args)).find((x) => x.ext_id === missing.ext_id);
  assert.equal(opened.state, "install_opened");
  assert.equal(opened.installed_here, false);
});

test("diagnostics device retirement is reversible and remote local operations are rejected", async () => {
  const api = createPreviewBackend("workspace");
  const mappings = await api.invoke("list_mappings");
  const local = mappings.find((m) => m.bookmarks);
  const remote = mappings.find((m) => !m.is_this_device && m.profile_id === local.profile_id);
  await assert.rejects(api.invoke("list_extensions", { mappingId: remote.id }), /another device/);
  await api.invoke("retire_device", { profileId: local.profile_id, deviceId: remote.device_id });
  assert.equal((await api.invoke("sync_diagnostics", { mappingId: local.id })).devices.find((d) => d.device_id === remote.device_id).retired, true);
  await api.invoke("reactivate_device", { profileId: local.profile_id, deviceId: remote.device_id });
  assert.equal((await api.invoke("sync_diagnostics", { mappingId: local.id })).devices.find((d) => d.device_id === remote.device_id).retired, false);
});

test("onboarding FDA, error injection and unsupported commands remain explicit", async () => {
  const api = createPreviewBackend("onboarding");
  assert.equal((await api.invoke("list_browsers"))[0].access, "requires_full_disk_access");
  api.setBrowserAccess("chrome", "accessible");
  assert.ok((await api.invoke("list_browsers"))[0].profiles.length > 0);
  api.failNext("create_profile", "test network connection failed");
  await assert.rejects(api.invoke("create_profile", { name: "Travel" }), /test network/);
  assert.equal((await api.invoke("list_profiles")).length, 0);
  await api.invoke("create_profile", { name: "Travel" });
  await assert.rejects(api.invoke("unsupported_command"), /does not implement/);
});

test("the companion store page opens only for a connection on this Mac", async () => {
  const backend = createPreviewBackend("workspace");
  const mine = (await backend.invoke("list_mappings")).find((m) => m.is_this_device);
  assert.ok(mine, "the workspace scenario has a local connection");
  assert.equal(await backend.invoke("open_companion_store", { mappingId: mine.id }), null);
  await assert.rejects(() => backend.invoke("open_companion_store", { mappingId: "not-a-connection" }), /not on this Mac/);
});
