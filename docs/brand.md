# Synchrome identity

The mark retains two opposing paths: sending and receiving changes around the same shared
profile. The 1.0.1 refinement uses continuous semicircular turns, shorter arrowheads and more
space between the arrows. Its balanced silhouette remains readable at toolbar sizes and in
monochrome, without decorative motion.

## One geometry

`apps/desktop/src/lib/brand.js` owns the four original paths on a 24-unit grid and the 2.05-unit
stroke. `Icon.svelte` imports these paths for the shell, authentication, boot and onboarding.
Run `node scripts/generate-brand.mjs` on macOS to regenerate all exported artwork. It uses the
existing Tauri SVG renderer, `sips` and `iconutil`; no extra dependency is required. macOS
sandbox restrictions may require running the generator outside the sandbox.

The generator reads the light accent and foreground from the shared desktop CSS tokens:

- Application icon: teal `#176b5d`, white mark, restrained lower shadow and inner highlight;
  transparent 1024-unit canvas, 864-unit rounded-square tile and clear surrounding space.
- Interface: current theme accent and shared primary text tokens. Retain light/dark/system
  appearance; the logo never owns a separate interface palette.
- Favicon and companion toolbar icons: the same geometry on a compact teal tile, rendered
  separately for 16, 32, 48 and 128 pixels. Their compact tile omits the Dock shadow and gutter.

## Assets

| Surface | Source/output |
| --- | --- |
| Interface mark | `apps/desktop/src/lib/brand.js`, consumed by `Icon.svelte` |
| Browser favicon | `apps/desktop/public/synchrome.svg` |
| macOS vector | `apps/desktop/src-tauri/icons/icon.svg` |
| macOS raster and icon family | `apps/desktop/src-tauri/icons/icon.png`, `icon.icns` |
| Companion monochrome vector | `apps/extension/brand.svg` |
| Companion toolbar/list/popup | `apps/extension/icons/{16,32,48,128}.png`, declared in the existing manifest |

The companion's manifest key and permissions remain unchanged: adding artwork must not change
its fixed extension identity or connection contract. Its popup adds the decorative brand image
next to the existing text heading, preserving all status/action selectors. Generated
`brand-tokens.css` shares the desktop palette; the popup follows the system light/dark
appearance, with visible keyboard focus and semantic status colors.

The wordmark remains native system typography with the existing restrained weight and spacing.
Do not substitute text glyphs or library icons for the mark. The brand vectors are original work for this repository and governed by its licence.
Internal controls use Lucide (ISC, with MIT notices for Feather-derived icons); the two
optional state transitions use Morphicons (MIT). Their full notices ship in the embedded
frontend as `THIRD-PARTY-LICENSES.txt`. The brand mark remains separate from these libraries.
