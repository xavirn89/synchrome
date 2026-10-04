# Internal icon update — 1.0.2

Synchrome's internal controls now use the official Lucide geometry through the shared
`Icon.svelte` renderer. Existing semantic names, sizes, currentColor styling, accessible
button labels and focus behavior remain stable. The original Synchrome brand mark remains
separate; this update does not replace the identity refined in 1.0.1.

| Before | After |
| --- | --- |
| Individually drawn interface symbols | Named Lucide icons with consistent geometry |
| Immediate eye and theme icon replacement | Immediate state changes, with Morphicons interpolation for pointer activation |
| No dependency attribution for icons | Bundled public third-party license notices |

Morphicons is limited to the password visibility and quick appearance controls. Its
built-in `snappy` spring is used; keyboard activation and reduced-motion preference take
an immediate path. Changing appearance through Settings also takes the immediate path.
Navigation, lists and recurring refresh indicators retain their existing motion policy.
Only named Lucide icon data is imported. Neither library makes network requests.

Sources: [Lucide core documentation](https://lucide.dev/guide/lucide),
[Morphicons](https://www.morphicons.com/). Licenses are included in
`apps/desktop/public/THIRD-PARTY-LICENSES.txt` (Lucide ISC / inherited Feather MIT,
Morphicons MIT).

[Orbs](https://libraries.dev/orbs) was reviewed. Its web components target React and AI
thinking states. No such state exists in Synchrome; no React dependency or orb was added.

## Verification

- Rust formatting, workspace clippy with warnings denied and workspace tests passed.
- Frontend Node tests: 37 passed. Production frontend and signed arm64 release build passed.
- [Native WKWebView checks](icons-native-validation.json): 63 passed, 0 failed.
- [Playwright checks](icons-playwright-validation.json): 46 passed, 0 failed, including
  720 px layouts, dialogs, auth, setup, connection recovery and both themes.
- [Native motion probe](icons-motion-native-validation.json): keyboard activation produced
  one immediate path update; pointer activation produced 28 interpolated path updates.
  These are DOM mouse events in the actual WKWebView, not claims of physical mouse input.
- Chromium reduced-motion emulation: one immediate path update, versus 52 updates for
  normal pointer animation. Password input type and accessible button name changed correctly.
- Impeccable detector returned no findings for the changed UI files.

Native screenshots: [light workspace](icons-native-light.jpg),
[dark workspace](icons-native-dark.jpg) and [authentication](icons-native-auth.jpg). Native and Chromium tests used explicit in-memory
fixtures and an isolated local app, with no real authentication, personal browser writes
or cloud mutations. This UI verification does not establish cross-Mac synchronization.

## Delivery

`~/Desktop/Synchrome-1.0.2-macOS-AppleSilicon.dmg` is Developer ID signed with hardened
runtime and timestamp. App signature, all 12 production bundle audit checks and DMG
integrity verification passed. Packaging used `--local-only`: no upload or notarization.
Gatekeeper reports an unnotarized Developer ID signature.

The installed app and the user's live session/connections were preserved. Close Synchrome
before replacing its copy in Applications. This installer contains the same companion
identity and connection protocol; existing browser connections do not need to be recreated.
