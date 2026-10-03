## verdict

- **Attention guidance — resolved.** Current native narrow and wide Overview captures show “Review conflicts” with copy that explicitly acknowledges other connections needing setup or recovery. The action and target are derived together; the native correction-batch checks confirm the matching route.
- **Cloud truth — resolved.** Current native Overview and expanded Settings captures show “Checking cloud…” in blue and “Cloud partially available” in amber. Both locations agree; partial diagnostics show Authentication 200, Database 503 and Storage 200. Confirmed availability remains positive in the light/dark Overview captures.
- **Narrow shell — resolved.** The current 720 × 528 interior capture shows the complete account avatar inside the window. The sibling selector now subtracts the preview banner height across the skip link; the native viewport-bound assertion passes.
- **Regressions from this correction batch:** none visible in the seven inspected native captures or the scoped code changes.

## remaining

Clear for the three original findings. Evidence inspected: `native-narrow.jpg`, `native-overview-light.jpg`, `native-overview-dark.jpg`, `native-cloud-pending.jpg`, `native-settings-pending.jpg`, `native-cloud-partial.jpg` and `native-settings-partial.jpg`; correction-batch native assertions were 63/63 at 720 × 528, with complementary Playwright checks 46/46.

The captures show the actual Tauri macOS WKWebView using explicitly labelled, token-free in-memory DEV fixtures. They do not establish live cloud synchronization or native browser writes. The later focus-restoration correction and its running wide harness are outside this scoped verdict. No additional detector was run.

This ship verdict covers the scored fixes, not the whole surface.

disposition: ship
