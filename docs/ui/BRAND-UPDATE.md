# Synchrome 1.0.1 — identity refinement

The user chose to evolve the existing synchronization mark. Impeccable and Emil Design
Engineering guided a focused refinement of the existing desktop identity; navigation,
authentication and sync behavior retain the existing product contract.

| Before | After | Why |
| --- | --- | --- |
| Tight angular turns and long arrowheads | Continuous opposing arcs with separated short arrows | Clear sending/receiving silhouette at small sizes |
| Independently copied interface/icon paths | Canonical `src/lib/brand.js` plus `scripts/generate-brand.mjs` | Keep the shell, favicon, Dock and companion geometry aligned |
| Mac icon teal differed from the desktop accent | Generator reads the shared light accent and foreground | Consistent identity with restrained icon depth |
| Companion had no declared icons | Dedicated 16/32/48/128 px exports and branded popup header | Recognizable browser toolbar and extension-list identity |
| Companion popup used an independent blue-gray palette | Generated desktop color tokens, system appearance and keyboard focus | Coherent brand in light/dark with semantic status colors |

## Verification

- Read-only inspection of the user's actual native app: all five Helium connections report
  Up to date. Aggregate local metadata confirms five completed first uploads, zero unpublished
  operations, zero recorded profile errors and zero conflicts. No tokens, bookmark contents or
  URLs were extracted. Cross-Mac convergence was not tested.
- Node frontend tests: 37 passed. Production frontend build passed with no preview fixture chunk.
- Rust formatting, workspace clippy with warnings denied and workspace tests passed.
- All four companion PNG dimensions validated; its manifest key and permissions are unchanged.
- Isolated debug app with embedded production frontend rendered the new mark in actual WKWebView,
  signed out with cloud deliberately unconfigured. Light/dark native captures and the DOM geometry
  report are saved alongside this document. The configuration warning in these captures belongs
  to the isolated test copy, not the production installer.
- Impeccable's single detector pass found pre-existing palette/radius/type drift in the companion
  popup; the correction batch replaced those styles with shared tokens and documented sizes.
- Developer ID signature, production bundle audit and DMG integrity verification passed.
  Package remains unnotarized; no submission to Apple was attempted.
- Isolated preview closed and its temporary directory removed. Installed Synchrome, its account,
  live connection registry and personal browser data were retained.

Delivery: `~/Desktop/Synchrome-1.0.1-macOS-AppleSilicon.dmg`.
SHA-256: `95c68e9468b337feccf58b378ac5ac5d4a431f099f3ed2f47e0040472e8ede3c`.

Quit the installed app before replacing it in Applications. Relaunch it to keep using the same
session and connections. In each already-loaded unpacked companion, use Reload on the extension
card (or Update with developer mode enabled) to load the new artwork and popup from the same
resource directory. No new extension identity or permissions are introduced.

The Impeccable context loader also reports the older PRODUCT.md schema; its `init` command owns
that optional record migration. The product contract was retained without unrelated rewriting.
