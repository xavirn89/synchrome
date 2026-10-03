# Native UI verification on macOS

Use the actual Tauri window for desktop acceptance. The explicit DEV adapter makes profile/auth/product scenarios reproducible without a live account; native command probes and Rust tests complement it. It does not simulate native browser-file writes or prove live cross-device convergence.

## Start an isolated development window

From `apps/desktop`, start `pnpm dev --host 127.0.0.1`. Create a temporary Tauri config with a distinct product name and identifier, the normal main window and this development URL:

```json
{
  "productName": "Synchrome UI Preview",
  "identifier": "com.synchrome.ui-preview",
  "build": { "devUrl": "http://127.0.0.1:5173/?preview=workspace" },
  "app": { "windows": [{ "label": "main", "title": "Synchrome — UI Preview", "width": 1100, "height": 780, "minWidth": 720, "minHeight": 520 }] },
  "bundle": { "active": false }
}
```

Run the debug app with isolated paths and the existing development-only guards:

```sh
SUPABASE_URL='' SUPABASE_ANON_KEY='' \
SYNCHROME_DATA_DIR=/private/tmp/synchrome-ui-native/data \
SYNCHROME_DEV_EXTRA_DIRS_ONLY=1 SYNCHROME_DEV_REALTIME=off \
SYNCHROME_DEV_UI_DRIVER=/private/tmp/synchrome-ui-native/driver \
pnpm tauri dev --no-watch --config /private/tmp/synchrome-ui-native/tauri.preview.json
```

Empty cloud environment values deliberately make native auth/cloud unavailable, so this instance does not access the normal Keychain session. The DEV frontend adapter handles the visibly labelled sample account and product data. Native discovery is restricted to the temporary data directory's extra browsers.

For Computer Use identification, a temporary `.app` wrapper with the distinct bundle identifier can contain the built debug executable, its icon and an Info.plist. Launch that wrapper with the same environment guards. Stop the initial unbundled process first so one app consumes the driver. Never replace the installed `/Applications/Synchrome.app` to check the redesign.

## Run deterministic checks

Once the native window is ready and its driver's `in`/`out` directories exist:

```sh
python3 tests/experiments/ui_redesign_native.py \
  --driver /private/tmp/synchrome-ui-native/driver
```

The harness verifies the native bridge, isolated device identity, signed-out/unconfigured native services and empty native browser discovery before acting on sample data. Its report is `docs/ui/native-validation.json`. It checks the real Svelte DOM inside WKWebView, not Chrome.

To repeat at the supported narrow width, set the temporary window to 720 pixels wide and use `--report docs/ui/native-validation-narrow.json`. The report records actual `innerWidth` and `innerHeight`; the native title bar is excluded. The narrow run also verifies that account access remains inside the viewport below the explicit preview banner.

Also use macOS accessibility/pointer/keyboard interactions to check navigation, form entry, Escape, focus, appearance and actual narrow window geometry. Capture the native window including its title bar. The minimum supported outer window is 720×520; browser viewport dimensions exclude native title-bar height.

WKWebView does not focus mouse-clicked buttons by default. Open a dialog from a previously unfocused control, use Tab and Escape, and verify that focus returns to that control. The deterministic harness includes this regression without pre-focusing its New profile button. `use:dialogTrigger` captures the invoking control before the dialog opens.

The final runs used actual 720×528 and 1512×917 interior viewports, changed with the native window zoom action. Their reports are `native-validation-narrow.json` and `native-validation-wide.json`; `native-manual-validation.json` records the complementary macOS interactions.

## Check the packaged frontend

Build from `apps/desktop` with `pnpm tauri build --debug --bundles app`. This produces `target/debug/bundle/macos/Synchrome.app` at the repository root. To check it safely, copy this bundle into a temporary directory, give the copy a distinct bundle identifier, and launch it with the same empty cloud environment, restricted discovery and a separate isolated data/driver directory. Do not replace the installed application.

The package loads `tauri://localhost` resources rather than the Vite URL. Confirm that neither the preview object nor sample-data banner exists and that native account/cloud probes are signed out and unconfigured. With cloud explicitly disabled, the configuration warning and gated authentication controls are expected. `native-packaged-auth.jpg` records this clean boot; normal form entry and validation are exercised in the labelled native DEV run. The debug package is for local review, not a signed/notarized release.

## Other checks

`pnpm test` checks product/transport rules. `pnpm build` must produce no preview fixture chunk. Run cargo fmt, clippy and workspace tests. Existing live native/cloud E2E harnesses continue to require their explicitly authorized test accounts and disposable browser profiles; sample UI acceptance does not replace those integration scenarios.
