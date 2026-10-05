# Listing artwork

- `store-icon.png`: 128×128 RGBA; original mark in a 96×96 tile, with transparent 16px margins, used in the store ZIP only. Existing unpacked companion icons remain untouched.
- `promo-small.png`: 440×280 PNG; original linked-arrow mark and two abstract browser frames. No browser trademarks or unsupported features.
- `screenshot-workspace.png`: 1280×800 real Svelte frontend, explicit in-memory development fixture. Illustrative sample data only; this is not live cloud synchronization evidence.
- `screenshot-companion.png`: 1280×800 actual popup markup/CSS/renderer, centered at its original 280px width, with a mock Chrome API reporting illustrative connected status and sample counts. No extension installation or live native/cloud execution is claimed by this screenshot.

The SVG sources are generated from the canonical brand paths by `node scripts/generate-store-art.mjs`; PNGs were rasterized in Chromium at exact dimensions with no scaling or stock image generation. Screenshots have no personal browser/account content.
