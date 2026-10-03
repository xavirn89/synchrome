# 07 — Release checklist (macOS, Synchrome 1.0)

One row per gate. "Blocker" rows stop a release; "External" rows are credentials or actions
that live outside the repository (they are not Synchrome bugs). Tick with the date and the
commit when a gate passes. Script: `scripts/release-macos.sh` (build + sign + bundle + audit).

## Code gates

| Gate | Command | Status |
|---|---|---|
| Formatting | `cargo fmt --check` | ✅ 2026-10-03 |
| Lints | `cargo clippy --workspace --all-targets -- -D warnings` | ✅ 2026-10-03 |
| Unit + integration tests | `cargo test --workspace` | ✅ 2026-10-03 (280) |
| Frontend | `cd apps/desktop && pnpm build` | ✅ 2026-10-03 |
| Admin-secret names absent from the product tree | `scripts/check-no-admin-secrets.sh` | ✅ 2026-10-03 |
| Smoke E2E of every changed area (`tests/experiments/e2e_*.py`, disposable profiles only) | per area | ✅ 2026-10-03: compaction 87/87 (retire/reactivate), Chrome 23/23 + 7/7, FDA 9/9, release smoke |

## Release build (`scripts/release-macos.sh`)

| Gate | How it is checked | Status |
|---|---|---|
| Public cloud values baked in (`SUPABASE_URL`, `SUPABASE_ANON_KEY`) — no `.env` in the bundle | script refuses to build without them; `find Synchrome.app -name '.env*'` empty | ✅ 2026-10-03 |
| No administrative secret in the build environment | the script refuses to build when the service-role key, the JWT secret or the database password is present in its environment | ✅ 2026-10-03 |
| Frontend embedded, no dev server URL | `strings` has `sync-health` and no `localhost:5173`; `tauri.conf.json` has no `devUrl` | ✅ 2026-10-03 |
| No dev CLI, UI driver, debug hooks, data-dir override | `strings` has no `--dev-`, `SYNCHROME_DEV_*`, `SYNCHROME_DATA_DIR` (all `cfg(debug_assertions)`) | ✅ 2026-10-03 |
| No developer paths | `--remap-path-prefix` for `$HOME` and the repo; `strings` has no `$HOME` | ✅ 2026-10-03 |
| Companion extension bundled | `Contents/Resources/extension/manifest.json` present (id fixed by its `key`) | ✅ 2026-10-03 |
| Signature | `codesign --verify --deep --strict`; Authority = Developer ID Application, hardened runtime, timestamp | ✅ 2026-10-03 |
| Notarization + staple | `xcrun stapler validate`; `spctl --assess --type execute` accepts | ⛔ **EXTERNAL RELEASE CREDENTIAL BLOCKER**: needs notary credentials in the environment (`APPLE_API_KEY`/`APPLE_API_ISSUER`/`APPLE_API_KEY_PATH` or `APPLE_ID`/`APPLE_PASSWORD`/`APPLE_TEAM_ID`) |
| Artefacts | `.app` + `.dmg` under `target/release/bundle/`, SHA-256 printed | ✅ 2026-10-03 |

## Install and first run

| Gate | How it is checked | Status |
|---|---|---|
| Clean install from the DMG into `/Applications`, first run with an empty data dir and an empty Keychain | manual + `e2e` clean-install smoke | ✅ 2026-10-03 (`e2e_release_smoke.py` 16/16 on the final DMG; DMG-copy refusal verified) |
| App refuses to register the native host from a DMG / translocated / Documents location | "Getting started" step 1; `install_native_host` error | ✅ 2026-10-03 (`e2e_release_smoke.py` 16/16 on the final DMG; DMG-copy refusal verified) |
| Native messaging host manifest points at `/Applications/Synchrome.app/Contents/MacOS/synchrome-desktop` | `<user-data-dir>/NativeMessagingHosts/com.synchrome.host.json` | ✅ 2026-10-03 (`e2e_release_smoke.py` 16/16 on the final DMG; DMG-copy refusal verified) |
| Companion extension loads from `Resources/extension` (Load unpacked) and confirms the link | mapping status "Extension confirmed" | ✅ 2026-10-03 (`e2e_release_smoke.py` 16/16 on the final DMG; DMG-copy refusal verified) |
| Restart of the app keeps the registration and the session (a Mac reboot only relaunches the same files: manifest path in /Applications, Keychain session) | relaunch → still Connected | ✅ 2026-10-03 (`e2e_release_smoke.py` 16/16 on the final DMG; DMG-copy refusal verified) |
| Logout / login keeps working (Realtime: exactly one connection per account) | Sync health row | ✅ 2026-10-03 (`e2e_release_smoke.py` 16/16 on the final DMG; DMG-copy refusal verified) |

## macOS permissions

| Gate | How it is checked | Status |
|---|---|---|
| Without Full Disk Access: Chrome / Brave / Edge shown as "Requires Full Disk Access", no profile names read, Helium accessible | Browsers card | ✅ 2026-10-03 (`e2e_fda.py`) |
| With Full Disk Access granted to `/Applications/Synchrome.app` (its Developer ID + bundle id): profiles listed; Preferences, History (read-only) and extension settings of a disposable profile readable | positive FDA test | ✅ 2026-10-03 (`e2e_fda.py`) |
| The grant survives a rebuild signed with the same identity | re-sign, relaunch | ✅ 2026-10-03 (`e2e_fda.py`) |

## Browsers (real matrix)

| Browser | Detection | Mapping + connector | Bookmarks | Extensions inventory | setEnabled | Uninstall | Extension settings | Preferences |
|---|---|---|---|---|---|---|---|---|
| Helium 0.18 (Chromium 154) | ✅ | ✅ | ✅ | ✅ | ✅ applied | ✅ gesture → User action required | ✅ | ✅ |
| Google Chrome 154 (stable) | ✅ | ✅ | ✅ | ✅ | ✅ applied (no gesture needed) | ✅ refused w/o gesture → User action required | ✅ | ✅ |

Evidence: slices 7–13 E2E on Helium; `e2e_chrome.py` (23/23 cloud + 7/7 local) on Chrome stable.

## Documentation

| Gate | Status |
|---|---|
| `docs/06` "SYNCHROME V1 — DONE" (scope, exclusions, requirements, limitations, post-v1) | ✅ 2026-10-03 |
| README: install, requirements, FDA, extension, updates (manual) | ✅ 2026-10-03 |
| Version 1.0.0 in workspace `Cargo.toml`, `tauri.conf.json`, `package.json`, extension manifest | ✅ 2026-10-03 |

## Server (edoras) — only after everything above is green

| Step | Status |
|---|---|
| Public service health check of the Supabase instance | ✅ 2026-10-03: authentication, database and storage returned HTTP 200; read-only anonymous probes |
| Remove temporary Auth users (slice E2E) | ☐ |
| Remove the pre-0005 backup and the temporary SQL files | ☐ |

## UI overhaul delivery — 2026-10-03

This delivery rebuilds application commit `665054d` with the current UI overhaul. Earlier
live browser/cloud E2E results above are historical integration evidence; they were not rerun
against this installer. No real cloud account was created during the UI acceptance work:
its sample account existed only in the explicit, token-free development preview.

| Gate | Current delivery evidence |
|---|---|
| Product and native safety checks | Formatting, workspace clippy and workspace tests passed, including seven deletion safety tests; [UI overhaul report](ui/OVERHAUL.md) |
| Frontend checks | 37 Node tests and production build passed; 46 Playwright UI assertions passed |
| Native UI acceptance | WKWebView: 63/63 assertions at each of 720 × 528 and 1512 × 917; additional native pointer, keyboard, dialog focus and resize checks passed |
| Production packaging | `scripts/release-macos.sh --local-only` completed; optimized arm64 `.app` and DMG for macOS 12+ |
| Production bundle audit | Embedded frontend and companion; no preview fixtures, development hooks, developer paths, administrative secrets or bundled `.env` |
| Signature and DMG integrity | Developer ID Application signature verified deep/strict, hardened runtime and timestamp; `hdiutil verify` passed |
| Production first launch | Actual app inside the mounted DMG opened in WKWebView at `tauri://localhost`, without configuration environment variables: enabled auth fields, signed out, no missing-cloud-configuration warning or preview UI |
| Live service health | Authentication, database and storage each returned HTTP 200 with public anonymous probes; no login or cloud mutation attempted |
| Cleanup | Preview processes and server stopped; sample accounts/data, isolated test folders and app runtime caches removed; no Keychain session; all 17 local state tables empty before removing the fresh app data directory; personal browser data unchanged |
| Desktop handoff | `~/Desktop/Synchrome-1.0.0-macOS-AppleSilicon.dmg`, 6,832,175 bytes; hash matches the built DMG |
| Notarization | Not performed. Automatic approval review rejected a script capable of uploading the package to Apple; the approved local-only build prevents that upload. Notary credentials are also absent. Gatekeeper reports `Unnotarized Developer ID`; successful local launch does not establish notarized distribution approval. |

Installer SHA-256:

```text
202349de333020a29296aad49659aa9b78b3c39bbde52ae5ad5a672b87d863a7
```

Machine-readable delivery evidence: [release-package-validation.json](ui/release-package-validation.json).
Native production first launch: [release-first-launch.jpg](ui/release-first-launch.jpg).
Install by dragging the app into Applications and replacing the previous copy before configuring
browser connectors. The delivery contains no signed-in account or personal browser data.
The older server-maintenance tasks above require their original server/account context and
remain separate from removal of this UI preview's fictitious account.
