# Synchrome Chrome Web Store handoff

Support: xavirn.me@gmail.com
Privacy URL reserved: https://www.garduc.com/synchrome/privacy/

Step-by-step publication guide (Spanish, current order of operations): `GUIA-PUBLICACION.md`.

Files:
- `listing.md`: store name, short and full descriptions.
- `privacy-disclosures.md`: actual permission/data explanations for the dashboard.
- `reviewer-instructions.md`: native-app installation and safe test requirements.
- `site/synchrome/privacy/index.html`: self-contained public privacy page to deploy on Garduc.
- `assets/`: listing images and provenance (generated separately).

Rebuild the ZIP with `python3 scripts/package-extension.py`. Outputs are under the ignored
`dist/chrome-web-store` directory. The ZIP is for uploading a draft, not claiming that the
store item is already compatible with the installed native app.

## Publication gates

1. Obtain the official CWS Item ID and Package → View public key after creating the draft.
2. DONE (8 Oct 2026): store item id `ceflmoojogokmklgcmcihdgakbefgiee`; its public key is now the
   development manifest `key` (same id unpacked and in the store). Pre-store id
   `gmpmcnknoaogpjdoonlddhpdpgkebbog` stays accepted for existing connections.
3. DONE: app 1.0.3 accepts both origins and writes the pairing token for the store id (and for
   the pre-store id only where its store already exists); `tests/experiments/e2e_store_id.py` 7/7.
4. Publish the privacy page and a public compatible macOS app download. Verify both anonymously.
5. Complete store graphics, listing, privacy disclosures and reviewer instructions.
6. Submit for review with publication deferred; publish only after compatibility is tested.
7. Replace “Load unpacked” setup guidance with the real store link after availability is verified.

The operator must confirm the final public listing/privacy statements before submission.
No real account or personal browser data is part of the upload archive or listing screenshots.

Official references:
- https://developer.chrome.com/docs/webstore/publish
- https://developer.chrome.com/docs/webstore/images
- https://developer.chrome.com/docs/webstore/cws-dashboard-privacy
- https://developer.chrome.com/docs/extensions/reference/manifest/key

## Preparation record — 5 October 2026

The verified publisher dashboard was accessible and showed no items. No package was uploaded,
no item was created and no review was submitted during this preparation. Browser actions
were interrupted by ongoing user window changes; continued dashboard interaction awaits
the user’s availability. ZIP allowlist, identity consistency, JS syntax, deterministic hash,
PNG dimensions and privacy-page layouts (1280/375px) passed. The existing frontend suite
passed all 37 tests. No app engine/cloud behavior changed; native/cloud E2E was not rerun
for this packaging-only change.

The privacy policy was published through Garduc `main` on 5 October 2026
(commits `2c437c6`, `68044fa`). Its canonical URL returns the standalone policy
with HTTP 200; the slashless URL redirects to it. The store draft and native
store-identity compatibility remain outstanding.
