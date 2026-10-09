# Synchrome product contract

Synchrome is a macOS desktop application for keeping selected browser data consistent across Macs. Its primary entity is a **Synchrome profile**: a named shared workspace such as Personal or Work. Local Chrome stable and Helium profiles connect to this shared profile. The audience needs to understand the connection and its state without understanding cloud tables, native messaging, or journals.

## This product phase

Improve navigation, authentication, progressive setup, profile creation and import, connection management, safe deletion, clear status, accessibility, and the desktop visual system. Preserve the validated synchronization engine and existing cloud contracts. Do not add history, IndexedDB, local-storage synchronization, Windows, tracked preferences E2, or other datasets.

## Facts the interface must preserve

- A profile may span multiple Macs. The v1 local engine supports one local browser connection to a given Synchrome profile on each Mac. Prevent a second local connection; do not promise an unsupported topology.
- Import creates the shared profile and connection. Browser data becomes the initial source after the companion confirms the connection and the first successful synchronization. Creating a mapping alone is not a completed import.
- Bookmarks and extension inventory/state are supported. Extension settings and the allowlisted browser preferences require explicit opt-in and applicable closed-browser behavior.
- Never synchronize passwords, cookies, sessions, or arbitrary browser files. Authentication secrets remain in the native service and Keychain.
- A remote connection is a recorded relationship, not evidence that its browser is currently online or synchronized.
- Removing a connection does not delete its browser profile. Deleting a Synchrome profile disconnects its cloud connections and invokes the existing safe cloud deletion service. Browser profiles, local bookmarks, and local browser files are retained.
- Disconnecting all mappings and deleting are separate existing operations. Partial failures must refresh state and give truthful recovery guidance.
- Device retirement only removes a device from compaction membership. It does not disconnect or stop synchronization.
- The companion is published on the Chrome Web Store (item `ceflmoojogokmklgcmcihdgakbefgiee`, unlisted). Setup opens its store page in the connected browser profile; loading the bundled folder unpacked stays available as a manual fallback. The pre-store companion id keeps working for existing connections.

## Product direction

A calm desktop workspace organized around profiles. Use a structured hierarchy of profile → device → browser connection. Give problems and setup a clear next action. Keep advanced diagnostics accessible below the everyday view. Use one restrained teal accent, clear typography, and useful density; support light, dark, and system appearance. Choose interaction details autonomously within this contract.

## Navigation

Overview answers what needs attention. Profiles contains shared profile management and connection details. Devices contains discovered browsers and connected Macs. Activity contains conflicts and observed synchronization events. Settings contains appearance, account, and advanced service diagnostics.

## Verification boundary

The browser development preview runs the real frontend with an explicit, token-free in-memory adapter, never real credentials. It tests interface behavior, not native browser writes or live cloud synchronization. Rust tests validate native orchestration. Existing credential-dependent end-to-end harnesses remain available and must not be run against personal data merely to validate a redesign.
