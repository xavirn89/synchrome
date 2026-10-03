# Synchrome UI and product overhaul

## Diagnosis and direction

The original interface put account state, cloud probes, profiles, browsers, conflict controls and compaction diagnostics into one long, dark-only column. Mapping counts were labelled as device counts, import did not distinguish a stored link from a completed first sync, and profile deletion required users to disconnect everything themselves. Normal use exposed paths, service names and low-level state before answering what was connected.

The implemented direction is a calm desktop workspace around **Synchrome profiles**, using familiar list/detail navigation. A profile is a shared named space. Its connections are grouped by Mac, then browser. The connected-arrow identity and teal palette distinguish Synchrome without adding ornamental graph controls.

## Navigation and profile model

| Area | Purpose |
| --- | --- |
| Overview | Shared profiles, connection summaries and the next action needing attention |
| Profiles | Create, import, rename and safely delete; inspect browser connections grouped by Mac |
| Devices | Macs, discovered browser profiles, access problems and connection actions |
| Activity | Conflict decisions, retained conflict history and last observed sync results |
| Settings | Appearance, account, version and advanced service diagnostics |

The local v1 engine accepts one browser connection per shared profile per Mac. The UI prevents unsupported second local sources and offers Change browser. A profile may still connect across many Macs. Remote relationships are displayed without asserting a live remote sync state. Supporting multiple local browser origins in one shared profile remains a separately scoped engine change.

## Authentication and setup

Authentication uses a dedicated view with sign-in/sign-up modes, associated labels, autocomplete, email and password validation, password visibility, loading protection, friendly errors and email-confirmation feedback. Sign out explains that it pauses this Mac and keeps browser data and shared profiles.

Setup explains the browser → Synchrome profile → other Macs relationship. It allows exploring the workspace and choosing a detected local profile. When signing in on a new Mac with existing shared profiles, it offers to connect one of them rather than creating a duplicate. Full Disk Access guidance is specific to the browser. Import creates and maps through the existing native command, then guides connector installation, loading the companion, confirmation and first sync. Completion requires a real uploaded state; a token or mapping alone never means that browser data was imported.

## Profile management and deletion

Manual creation makes an unconnected profile. Import starts from detected browser data. Existing profiles can receive a local connection, and a local browser can be changed or disconnected. Version, access, companion, offline and dataset state determine the next action.

Delete requires typing the profile name and separately explains what is removed from cloud and what stays locally. Associated connections are disconnected in sequence before invoking the existing safe cloud deletion command. The view refreshes after both success and partial failure. A native safety fix ensures a rejected cloud deletion keeps local queued sync data. Cleanup failures after a confirmed cloud-row deletion remain explicit rather than reporting complete success. No deletion operation removes local browser profiles, local bookmarks or browser files.

## Presentation, accessibility and motion

`DESIGN.md` documents exact tokens, light/dark contrast, type, spacing, reusable controls and state hierarchy. System appearance follows macOS changes; Light and Dark can be selected explicitly. The presentation preference alone is stored locally.

Dialogs use native `<dialog>` with labelled content, focus containment, Escape/backdrop handling and busy protection. Navigation has accessible names and current-page state. Forms use labels and validation announcements. Error/success feedback uses live regions. Color is paired with words and icons. Narrow desktop navigation becomes a labelled icon rail; groups and long names wrap.

Motion is limited to pending work and brief dialog/toast arrival; reduced motion disables animations, transitions and smooth scrolling. The original SVG brand mark, PNG, ICNS and favicon are documented in `docs/brand.md`; no third-party artwork is used.

## Bugs corrected

- Local queued state was forgotten before cloud profile deletion had succeeded.
- Cloud cleanup failures could be presented as completed deletion.
- Version mismatch was caught by a generic link-mismatch error before its own explanation.
- Profile device counts counted browser connections instead of distinct device IDs.
- Conflict headings labelled extension/settings/preference conflicts as bookmarks.
- Conflict totals could double-count dataset subsets already included in the native aggregate.
- Local picker keys omitted the user-data directory.
- A prior data refresh could overwrite a newer mutation or a signed-out account view.
- Retry clicks could be silently discarded while background refresh was running.
- Changing a browser could become unrecoverable after successful disconnect followed by failed connect.
- Bookmark errors could offer a token-write retry instead of retrying sync.
- Remote connection names and state were inferred from this Mac's browser discovery.
- The cloud connection indicator did not automatically recover from a stale offline probe.
- Attention copy could describe a conflict while its action opened a different connection; mixed setup states could be described as already syncing.
- Unknown or partial cloud probes could be labelled and styled as a fully connected service.
- The sample-data banner could push account access below the narrow development window; the shell now accounts for its height across the skip link.

## Verification and evidence

The native preview uses the real Svelte frontend in a **Tauri WKWebView window**, with an explicitly labelled in-memory development transport for product interactions. Native read-only version, account, cloud, isolated device and browser-discovery commands are also checked through the Rust bridge. Production excludes this preview transport and its fixtures.

The native deterministic harness is `tests/experiments/ui_redesign_native.py`. It requires an already running debug UI driver and refuses to run without the explicit preview, isolated native data and disabled cloud gates. It never installs over `/Applications`, authenticates to a real account or touches personal browser profiles.

Computer Use additionally checks actual macOS accessibility controls, keyboard dismissal, profile creation, navigation, themes and screenshots. Desktop acceptance takes place in Tauri following the user's clarification. Playwright MCP also exercises the actual frontend as a complementary check, including auth/signup, first entry, import, companion/first sync, connection replacement failure/retry, disconnect, creation, busy dialogs, deletion failures, conflicts, Settings, themes, narrow layout and reduced motion. Browser-only preview results are not claimed as native synchronization results.

`native-validation.json` records the executed native UI checks. Captures in this directory show the native window, including its title bar and development-data label. `../ui-before.png` records the original monocolumn interface.

Rust tests cover deletion orchestration with mocked REST/Storage and real in-memory SQLite. Frontend Node tests cover transport isolation, creation/import, first sync, safe deletion, partial failures, state priorities, conflict totals and device grouping. Existing integration harnesses were adapted to the new navigation, disclosure controls and logout confirmation. Their credential-dependent live-cloud scenarios are retained rather than rerun against personal data.

| Gate | Result |
| --- | --- |
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --workspace --all-targets -- -D warnings` | Passed |
| `cargo test --workspace` | Passed, including seven native deletion safety tests |
| `node --test tests/*.test.js` / `pnpm test` | 37 passed |
| `pnpm build` | Passed; no preview fixture chunk in production assets |
| `cargo build -p synchrome-desktop --features tauri/custom-protocol` | Passed with the production frontend embedded |
| `pnpm tauri build --debug --bundles app` | Passed; local macOS bundle at `target/debug/bundle/macos/Synchrome.app` |
| Native WKWebView UI harness at 720 px | 63 passed, zero failed; `native-validation-narrow.json` |
| Earlier wide native WKWebView run | 50 passed, zero failed; `native-validation.json` |
| Playwright MCP UI checks | 46 passed, zero failed; `playwright-validation.json` |
| Existing Python integration harness syntax | All 32 parse successfully |

The native harness is reproducible with `NATIVE-TESTING.md`. The Playwright MCP runner is `tests/experiments/ui_redesign_playwright.js` and returns its structured result; save that result as `playwright-validation.json`. The preview is explicitly labelled sample data. Live cloud authentication, real browser writes, permission grants, and cross-Mac convergence were not exercised in this UI acceptance run.

The Impeccable frontend detector ran once on the changed Svelte/CSS surfaces and returned no findings. A fresh delegated finish review identified the three attention/cloud/banner issues listed above; they were corrected together and verified with the added product and UI regressions.

The follow-up reviewer confirmed the three functional corrections from code and the executed native assertions. Its visual disposition remains **recapture**: the final attention/cloud/banner regions still need updated native images. Computer Use reported the Mac locked, and the user was asked to unlock it. Existing JPEG captures document the native implementation before that final correction batch; they must not be presented as post-fix visual approval. Native pointer/keyboard resize and fresh auth/Settings captures also remain pending. The completed native DOM runs and their scope are recorded above.

The local `.app` is a debug build with production frontend assets, suitable for local review. It does not replace `/Applications/Synchrome.app` or constitute a signed/notarized release.

## Durable documentation

`AGENTS.md` is the entry point for future agents. `PRODUCT.md` preserves product invariants and scope. `DESIGN.md` defines the shared presentation and interaction system. Changes should update those contracts when product behavior changes, rather than documenting transient implementation steps.
