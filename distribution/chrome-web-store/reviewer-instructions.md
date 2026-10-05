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

Prerequisites currently outstanding:
- Public macOS app download URL, with the official store extension ID supported.
- Published privacy policy URL.
- Official CWS item ID and public key incorporated into a tested native compatibility update.
- Reviewer account if self-service registration is insufficient for review.

Do not submit a package claiming compatibility with the current local/unpacked ID without checking its actual CWS item ID. Never change/delete personal browser data for review testing.
