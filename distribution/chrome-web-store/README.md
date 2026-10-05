# Synchrome Chrome Web Store handoff

Support: xavirn.me@gmail.com
Privacy URL reserved: https://www.garduc.com/synchrome/privacy/

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
2. Compare against local ID `gmpmcnknoaogpjdoonlddhpdpgkebbog`. The current development
   manifest key cannot guarantee the store's assigned signing identity.
3. If different, implement and test native allowlisting/pairing for the official identity,
   preserving existing local connections. Do not replace the constant blindly or expose
   pairing tokens. A separate app release is required before store users can connect.
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
