# README images

Regenerates `docs/readme/*.png` from the real interface, using the token-free development
preview (`?preview=showcase` and its sample data; never a real account or browser profile).
Requires Node and [Playwright](https://playwright.dev) (`npm i playwright`), launched with the
locally installed Google Chrome.

1. Serve the interface on port 4317: `pnpm --dir apps/desktop exec vite --port 4317 --strictPort`.
2. `node capture.mjs <raw-dir>` — light and dark screenshots at 1280×800, 2×.
3. `node compose.mjs <raw-dir> docs/readme <repo-root>` — macOS window frames and the companion popup.
4. `node diagram.mjs docs/readme <repo-root>` — the "How it works" diagram.
