# Synchrome identity

Synchrome uses two connected paths with opposing arrows. The mark expresses a shared profile receiving and sending changes across browsers and devices. Its open paths remain legible as a small toolbar symbol, an application icon and a monochrome mark.

The application icon places the mark in off-white (`#f4fffd`) on a teal (`#087f75`) rounded square, with clear space around the silhouette for macOS. The interface uses the same geometry in `apps/desktop/src/components/Icon.svelte` with `name="logo"`; its colour comes from the current theme’s accent token. The icon artwork is theme independent.

Sources are `apps/desktop/public/synchrome.svg` for the browser favicon and `apps/desktop/src-tauri/icons/icon.svg` for the macOS icon. Keep their geometry identical. `icon.png` is a 1024-pixel transparent rendering of the vector; `icon.icns` includes the standard macOS sizes. Regenerate these raster outputs when changing the vector. Do not approximate the mark with text symbols or unrelated library icons.

The SVG mark, interface icons and application icon were drawn originally for this repository. No third-party imagery, font or icon artwork is included in these assets; the repository’s licence governs their use.
