// Frames raw screenshots as macOS windows for the README and renders the companion popup.
// usage: node compose.mjs <rawdir> <outdir> <repo>
import { chromium } from "playwright";
import fs from "node:fs";
import path from "node:path";

const [RAW, OUT, REPO] = process.argv.slice(2).map((p) => path.resolve(p));
fs.mkdirSync(OUT, { recursive: true });
const browser = await chromium.launch({ channel: "chrome" });

const THEMES = {
  light: { bg1: "#e6f1ed", bg2: "#cfe4dc", bar: "#eef2f0", barLine: "#d6dfdb", title: "#4b5f5a", shadow: "rgba(23,50,40,.28)" },
  dark: { bg1: "#16312c", bg2: "#0c1b19", bar: "#1d2a2b", barLine: "#2c3b3a", title: "#9fb3ad", shadow: "rgba(0,0,0,.55)" },
};

function frame(src, theme, { width = 1440, pad = 72, title = "Synchrome" } = {}) {
  const t = THEMES[theme];
  return `<!doctype html><html><head><meta charset="utf-8"><style>
  html,body{margin:0}
  body{width:${width + pad * 2}px;padding:${pad}px;box-sizing:border-box;
    background:radial-gradient(120% 120% at 20% 0%, ${t.bg1}, ${t.bg2});font:13px -apple-system,BlinkMacSystemFont,sans-serif}
  .win{border-radius:12px;overflow:hidden;box-shadow:0 30px 70px ${t.shadow},0 0 0 1px rgba(0,0,0,.12)}
  .bar{height:36px;background:${t.bar};border-bottom:1px solid ${t.barLine};display:flex;align-items:center;position:relative}
  .dots{display:flex;gap:8px;padding-left:16px}
  .dots i{width:12px;height:12px;border-radius:50%;display:block}
  .title{position:absolute;left:0;right:0;text-align:center;color:${t.title};font-weight:600}
  img{display:block;width:100%}
  </style></head><body><div class="win"><div class="bar"><div class="dots"><i style="background:#ff5f57"></i><i style="background:#febc2e"></i><i style="background:#28c840"></i></div><div class="title">${title}</div></div>
  <img src="file://${src}"></div></body></html>`;
}

async function render(html, out, scale = 1) {
  const ctx = await browser.newContext({ viewport: { width: 800, height: 600 }, deviceScaleFactor: scale });
  const p = await ctx.newPage();
  const tmp = path.join(OUT, ".tmp.html");
  fs.writeFileSync(tmp, html);
  await p.goto(`file://${tmp}`);
  await p.waitForLoadState("networkidle");
  await p.evaluate(() => Promise.all([...document.images].map((i) => i.decode())));
  const size = await p.evaluate(() => ({ width: document.body.offsetWidth, height: document.body.offsetHeight }));
  await p.setViewportSize(size);
  await p.screenshot({ path: out });
  fs.rmSync(tmp);
  await ctx.close();
  console.log("saved", path.basename(out));
}

for (const theme of ["light", "dark"]) {
  for (const name of ["overview", "profile", "setup", "activity", "devices", "settings", "signin"]) {
    await render(frame(`${RAW}/${name}-${theme}.png`, theme), `${OUT}/${name}-${theme}.png`);
  }
}

// Companion popup: the real popup.html with a stubbed chrome API and sample status.
const status = {
  ui: "connected", status: "connected", profile_name: "Personal",
  bookmarks: { state: "up_to_date", bookmarks: 248, pending: 0, last_upload_at: Math.floor(Date.now() / 1000) - 240,
    download: { state: "up_to_date", remote_pending: 0, conflicts: 0, last_download_at: Math.floor(Date.now() / 1000) - 60 } },
  extensions: { synced: 3, missing: 0, differences: 0, conflicts: 0, user_action: 0 },
};
for (const theme of ["light", "dark"]) {
  const ctx = await browser.newContext({ viewport: { width: 280, height: 300 }, deviceScaleFactor: 2, colorScheme: theme, locale: "en-GB", timezoneId: "Europe/Madrid" });
  await ctx.addInitScript((s) => { window.chrome = { runtime: { sendMessage: (_m, cb) => setTimeout(() => cb(s), 0) }, management: { get: async () => ({}) } }; }, status);
  const p = await ctx.newPage();
  await p.goto(`file://${REPO}/apps/extension/popup.html`);
  await p.waitForTimeout(400);
  const h = await p.evaluate(() => document.querySelector("main").getBoundingClientRect().height);
  await p.setViewportSize({ width: 280, height: Math.ceil(h) });
  await p.screenshot({ path: `${OUT}/.popup-${theme}.png` });
  await ctx.close();
  const t = THEMES[theme];
  const html = `<!doctype html><html><head><meta charset="utf-8"><style>
  html,body{margin:0}
  body{width:760px;height:560px;padding:56px 56px 0;box-sizing:border-box;background:radial-gradient(120% 120% at 20% 0%, ${t.bg1}, ${t.bg2});
    font:13px -apple-system,BlinkMacSystemFont,sans-serif;overflow:hidden}
  .win{border-radius:12px 12px 0 0;overflow:hidden;box-shadow:0 30px 70px ${t.shadow},0 0 0 1px rgba(0,0,0,.12);height:100%;background:${theme === "dark" ? "#202124" : "#fff"};position:relative}
  .tabs{height:38px;background:${theme === "dark" ? "#1b1c1f" : "#dee1e6"};display:flex;align-items:flex-end;gap:8px;padding-left:16px}
  .dots{display:flex;gap:8px;align-self:center;margin-right:12px}.dots i{width:12px;height:12px;border-radius:50%;display:block}
  .tab{height:30px;width:200px;border-radius:8px 8px 0 0;background:${theme === "dark" ? "#35363a" : "#fff"};color:${theme === "dark" ? "#e8eaed" : "#3c4043"};display:flex;align-items:center;padding-left:12px;box-sizing:border-box}
  .bar{height:40px;background:${theme === "dark" ? "#35363a" : "#fff"};border-bottom:1px solid ${theme === "dark" ? "#4a4c50" : "#e3e5e8"};display:flex;align-items:center;gap:10px;padding:0 12px}
  .url{flex:1;height:28px;border-radius:14px;background:${theme === "dark" ? "#202124" : "#f1f3f4"}}
  .ext{width:26px;height:26px;border-radius:6px;display:grid;place-items:center;background:${theme === "dark" ? "#4a4c50" : "#e8f0ee"}}
  .ext img{width:18px;height:18px}
  .pop{position:absolute;right:14px;top:84px;width:280px;border-radius:10px;overflow:hidden;
    box-shadow:0 12px 32px rgba(0,0,0,${theme === "dark" ? ".6" : ".22"}),0 0 0 1px rgba(0,0,0,.14)}
  .pop img{display:block;width:280px}
  .lines{padding:28px 32px;display:grid;gap:14px}.lines i{display:block;height:12px;border-radius:6px;background:${theme === "dark" ? "#2d2e31" : "#eef0f2"}}
  </style></head><body><div class="win">
  <div class="tabs"><div class="dots"><i style="background:#ff5f57"></i><i style="background:#febc2e"></i><i style="background:#28c840"></i></div><div class="tab">New Tab</div></div>
  <div class="bar"><div class="url"></div><div class="ext"><img src="file://${REPO}/apps/extension/icons/32.png"></div></div>
  <div class="lines"><i style="width:46%"></i><i style="width:38%"></i><i style="width:42%"></i><i style="width:30%"></i></div>
  <div class="pop"><img src="file://${OUT}/.popup-${theme}.png"></div></div></body></html>`;
  await render(html, `${OUT}/companion-${theme}.png`, 2);
  fs.rmSync(`${OUT}/.popup-${theme}.png`);
}
await browser.close();
