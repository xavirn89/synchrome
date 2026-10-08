# Reviewer instructions — complete delivery prerequisites before submitting

Synchrome is a macOS companion extension. Chrome 116+ and a separate macOS Synchrome app are required. It is not functional as a standalone extension on Windows/Linux.

1. Download/install the compatible Synchrome macOS app from a public URL supplied in this review field. A local Desktop DMG is not accessible to Google reviewers.
2. Use a dedicated test Chrome profile containing only sample bookmarks and extensions.
3. Create a Synchrome account and complete email confirmation. If Google requests credentials, provide a dedicated authorized test account privately in the dashboard, never personal credentials or repository files.
4. In Synchrome, create a shared profile and connect the dedicated Chrome profile. Close Chrome when the app asks before writing the local pairing token.
5. Install the native browser connector through Synchrome's setup assistant, then restart Chrome.
6. Install this store extension using its assigned store identity, open its popup, and confirm the connected shared-profile name.
7. Create/edit a sample bookmark and observe sync state. A second Mac with another dedicated profile can verify cross-Mac convergence.
8. Synchrome displays conflicts in the desktop app, where the reviewer can choose a resolution.

Public privacy policy: https://www.garduc.com/synchrome/privacy/

Prerequisites currently outstanding:
- Public download URL of the Synchrome macOS app 1.0.3 or later (the first version that accepts
  the store item id `ceflmoojogokmklgcmcihdgakbefgiee`), preferably notarized.
- Reviewer account if self-service registration is insufficient for review.

Done: official CWS item id and public key incorporated (app 1.0.3; development manifest uses the
store key, so unpacked and store builds share the id; the pre-store id stays accepted).

Do not submit a package claiming compatibility with the current local/unpacked ID without checking its actual CWS item ID. Never change/delete personal browser data for review testing.
