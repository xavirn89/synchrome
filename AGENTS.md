# Working on Synchrome

Read `PRODUCT.md` and `DESIGN.md` before changing product behavior or UI. These documents are the durable product and design contract. Keep them aligned with implemented behavior rather than proposed features.

## Architecture and scope

- Synchrome is a macOS Tauri application. `apps/desktop/src` is Svelte 5; native commands are in `apps/desktop/src-tauri`; the shared engine is in `crates`.
- Reuse existing native commands and services. Do not duplicate synchronization logic in the frontend or change cloud contracts merely to improve a screen.
- UI work must preserve the engine’s safety constraints. Obtain explicit authorization before irreversible cloud migrations, real-data deletion or a new contract. Do not introduce history, IndexedDB, local-storage synchronization, Windows or additional datasets as a side effect.
- Keep authentication secrets in the native service and Keychain. The frontend only holds form input transiently; do not log passwords, expose tokens or put credentials in browser storage.

## Product invariants

- The visible primary entity is a **Synchrome profile**. A local browser profile has a **connection** to it. Everyday UI does not call connections mappings, identify profiles by UUIDs or require knowledge of cloud tables.
- Organize connected browsers as profile → Mac → browser connection. Current v1 supports one local browser connection per Synchrome profile per Mac. Enforce and explain this when selecting a connection. Do not promise multiple same-profile local origins until the engine has been changed and tested.
- Remote connections are recorded relationships. Show that they are on another Mac without claiming a live remote sync state.
- Manual profile creation produces an unconnected profile. Import uses the existing create-and-map command and treats browser data as the initial source only after companion confirmation and actual first sync. A stored link alone is not an import or sync completion.
- Extension settings and supported preferences are opt-in. Respect closed-browser requirements, version compatibility and the allowlist. Never imply passwords, cookies, sessions or arbitrary browser files are synchronized.
- Disconnect keeps local browser profiles, bookmarks and files. Shared-profile deletion keeps that local data too. Use the existing safe cloud delete command after disconnecting associated cloud connections; require exact-name confirmation and explain deletion scope. If a step fails, refresh and show partial progress truthfully.
- Device retirement concerns compaction membership; do not present it as disconnecting the device or stopping its sync.

## UI system

- Navigation is Overview, Profiles, Devices, Activity and Settings. Account access opens Settings. Keep contextual actions close to the selected profile or browser connection.
- Preserve shared tokens and reusable controls in `apps/desktop/src/styles.css`. New components consume CSS variables; do not introduce independent palettes or hardcoded theme colors.
- Use the native system font, 14px body text, restrained heading hierarchy and the existing 4px/2px spacing rhythm. Use original SVG icons from `Icon.svelte`, never Unicode glyphs or emoji as UI icons. Brand assets and their source are documented in `docs/brand.md`.
- Support System, Light and Dark. Appearance state belongs to the shell and sets root `data-theme` and `color-scheme`. Persist only this local presentation preference under `synchrome.appearance`; do not store account or sync data there.
- Use `.button`, `.field`, `.notice`, `.status` and the existing style variants. Colors convey meaning alongside text and icons. Reserve teal for primary/current/positive meaning, amber for attention, red for failures/destructive actions and blue for pending information.
- Summarize local sync state with the shared helpers in `src/lib/product.js`. Keep technical diagnostics expandable. A healthy remote link cannot hide a local conflict or incomplete first sync.
- User-facing errors name the problem and recovery. Rust, SQL, network payloads and IDs are optional Technical details, not the headline. Preserve advanced actions and diagnostics when reorganizing screens.

## Interaction and accessibility

- Authentication uses `AuthView.svelte` with explicit labels, correct autocomplete, local validation, Show/Hide password and duplicate-submit protection. Preserve `form.login`, Email/Password labels and the established integration selectors. Successful account creation awaiting email confirmation gets a success panel.
- Gate sign-in only on explicit service/configuration failures. Unknown probe state is pending, not a demonstrated failure. Preserve typed values on unsuccessful authentication; never log them.
- Use `Dialog.svelte` and native `<dialog>` for focus-protected flows. Label each dialog; trap focus, handle Escape/backdrop safely and restore focus to the trigger. Prevent dismissal and duplicate mutations while a protected operation is in flight.
- Every icon-only action needs an accessible name. Keep keyboard focus visible, input labels associated, navigation `aria-current`, the skip link and status/error announcements.
- Keep routine navigation immediate. Dialog/toast arrival can use the established 180ms opacity/6px motion; avoid decorative movement and repeated list animation. Respect reduced motion for animations, transitions and scrolling.
- Test narrow supported desktop windows. Collapse navigation structurally, wrap action groups and user content, and contain long technical strings. Do not assume a large window or require hover to discover the next action.

## Verification

Run checks appropriate to the change, and retain these release gates:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

From `apps/desktop`:

```sh
node --test tests/*.test.js
pnpm build
```

Add meaningful tests for changed profile creation/import, deletion orchestration, state priorities and recovery behavior. Preserve E2E action names/selectors or update the corresponding harnesses in the same change.

Use Playwright to exercise the real frontend, including auth, setup, manual creation/import, connection management, errors, dialogs, deletion, Settings and both themes. The explicit development preview adapter is in-memory and token-free; production must never silently enable it. Preview tests verify UI behavior, not live cloud sync or native browser writes.

Also test the rebuilt macOS application in WKWebView after substantial UI changes: clean boot, auth fields and validation, keyboard/focus behavior, native dialog dismissal/focus restoration, appearance and window resizing. Chromium screenshots alone cannot establish native behavior. Use dedicated browser profiles and explicitly authorized test accounts for native/cloud E2E runs, never personal browser data just to validate a redesign.

Update durable product/design rules when the implementation changes them. Report verification limits explicitly and keep the final working tree reviewable.
