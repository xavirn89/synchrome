import { invoke as tauriInvoke } from "@tauri-apps/api/core";

const scenarios = new Set(["workspace", "onboarding", "auth", "offline", "empty"]);

// An explicit URL opt-in is required. A release build always invokes Rust, even if
// someone adds ?preview=workspace to the window URL.
export function previewScenarioFor(dev, search = "") {
  if (!dev) return null;
  const scenario = new URLSearchParams(search).get("preview");
  return scenarios.has(scenario) ? scenario : null;
}

export const previewScenario = previewScenarioFor(
  import.meta.env?.DEV === true,
  typeof window === "undefined" ? "" : window.location.search,
);
export const previewEnabled = previewScenario !== null;

const preview = import.meta.env?.DEV === true && previewEnabled
  ? import("./preview.js").then(({ createPreviewBackend }) => {
      const backend = createPreviewBackend(previewScenario, { delayMs: 140 });
      // Development-only controls let browser tests simulate OS/companion events.
      // No command in preview mode ever falls through to Rust or to the network.
      window.__SYNCHROME_PREVIEW__ = backend;
      return backend;
    })
  : null;

export async function invoke(command, args = {}) {
  return preview ? (await preview).invoke(command, args) : tauriInvoke(command, args);
}
