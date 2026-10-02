# Synchrome test dummy extension

Minimal, inert MV3 extension for the extensions E2E (`tests/experiments/e2e_extensions.py`).
No permissions. The `key` is a public key generated for this fixture (the private key was
discarded), so the extension id is stable across disposable profiles: `dlijhhlcpppmppbekhaonjhlahfidkgf`.
Loaded only into disposable Helium profiles with `--load-extension`.

Slice 11: `storage` permission and an empty `page.html`, so the harness can read/write this
extension's own chrome.storage.local/.sync (known keys: localTheme, localCount, syncMode,
syncEnabled; and apiToken, a fake value that must be excluded as sensitive).
