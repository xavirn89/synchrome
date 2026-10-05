# Dashboard privacy disclosures (review before saving)

Single purpose: Connect a selected browser profile to the Synchrome macOS application so the user's bookmarks and installed extension list/state can synchronize across their Macs.

Permission justifications:
- bookmarks: Read bookmark titles, URLs, folders and changes, and apply synchronized bookmark operations supplied by the desktop app.
- management: Read installed extension metadata and enabled state; apply enable/disable operations and explicitly confirmed removals. Open official store pages only when requested. Synchrome cannot silently install other extensions.
- nativeMessaging: Communicate with the locally installed com.synchrome.host connector; the companion has no direct cloud client.
- storage: Retain the local pairing token, connection/profile status and session-only acknowledged snapshot hashes. No account passwords or cloud access tokens are stored by the companion.
- alarms: Wake the MV3 worker periodically to reconcile changes and retry synchronization.

Remote code: No. All JavaScript ships in the extension. Native messages carry data and enumerated operations, not executable code. No content scripts, host permissions, remote scripts, eval or runtime code downloads.

Data declarations must include the native-app transfer; “no data collected” is inaccurate:
- Web history: declare conservatively because bookmark URLs/titles describe saved web destinations, even though chrome.history and visited-page history are not accessed.
- Website content: declare conservatively for saved bookmark titles/URLs, not page text, images or DOM access.
- Authentication information: local pairing token authenticates this browser connection. No account password or cloud access token is read by the companion.
- Personally identifiable information: app-associated account, shared profile/device identifiers and user-selected profile names; disclose the whole Synchrome data path, including the app's cloud account.
- Other categories: no targeted collection of financial, health, communications, location or user-activity telemetry.

Limited use certifications: data is used for the disclosed synchronization functionality, not sold, used for advertising or creditworthiness. Review these statements against the final privacy page before certifying.

Privacy URL must be live and public before submission:
https://www.garduc.com/synchrome/privacy/
