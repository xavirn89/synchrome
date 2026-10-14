// "How it works" diagram for the README, light and dark.
// usage: node diagram.mjs <outdir> <repo>
import { chromium } from "playwright";
import fs from "node:fs";
import path from "node:path";

const [OUT, REPO] = process.argv.slice(2).map((p) => path.resolve(p));
const logo = `data:image/svg+xml;base64,${fs.readFileSync(`${REPO}/apps/desktop/public/synchrome.svg`).toString("base64")}`;
const T = {
  light: { bg: "#f5f8f7", card: "#ffffff", line: "#d8e3de", fg: "#203a37", muted: "#59716c", accent: "#176b5d", soft: "#e0f0e9", mac: "#eaf1ef", cloud: "#eaf2fa", info: "#2d6193" },
  dark: { bg: "#141f20", card: "#1b292b", line: "#354844", fg: "#e5eeeb", muted: "#a2b8b0", accent: "#8bdbc0", soft: "#243d35", mac: "#102321", cloud: "#26394b", info: "#a4cafa" },
};
const icon = {
  browser: '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 9h18"/><path d="M7 6.5h.01M10 6.5h.01"/>',
  puzzle: '<path d="M15.39 4.39a1 1 0 0 0 1.68-.474 2.5 2.5 0 1 1 3.014 3.015 1 1 0 0 0-.474 1.68l1.683 1.682a2.414 2.414 0 0 1 0 3.414L19.61 15.39a1 1 0 0 1-1.68-.474 2.5 2.5 0 1 0-3.014 3.015 1 1 0 0 1 .474 1.68l-1.683 1.682a2.414 2.414 0 0 1-3.414 0L8.61 19.61a1 1 0 0 0-1.68.474 2.5 2.5 0 1 1-3.014-3.015 1 1 0 0 0 .474-1.68l-1.683-1.682a2.414 2.414 0 0 1 0-3.414L4.39 8.61a1 1 0 0 1 1.68.474 2.5 2.5 0 1 0 3.014-3.015 1 1 0 0 1-.474-1.68l1.683-1.682a2.414 2.414 0 0 1 3.414 0z"/>',
  cloud: '<path d="M17.5 19H9a7 7 0 1 1 6.71-9h1.79a4.5 4.5 0 1 1 0 9Z"/>',
  lock: '<rect x="4" y="11" width="16" height="10" rx="2"/><path d="M8 11V7a4 4 0 0 1 8 0v4"/>',
  laptop: '<path d="M18 5a2 2 0 0 1 2 2v8.526a2 2 0 0 0 .212.897l1.068 2.127a1 1 0 0 1-.9 1.45H3.62a1 1 0 0 1-.9-1.45l1.068-2.127A2 2 0 0 0 4 15.526V7a2 2 0 0 1 2-2z"/><path d="M20.054 15.987H3.946"/>',
};
const svg = (n, c) => `<svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="${c}" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">${icon[n]}</svg>`;

function page(theme) {
  const t = T[theme];
  const box = (ic, title, sub, extra = "") => `<div class="box" ${extra}><div class="hd">${ic}<b>${title}</b></div><div class="sub">${sub}</div></div>`;
  const mac = (name) => `<div class="mac"><div class="macname">${svg("laptop", t.muted)}${name}</div>
    ${box(svg("browser", t.accent), "Browser profile", "Helium or Chrome")}
    <div class="arrow v"><span><b style="color:inherit;font-weight:600">companion extension</b><br>live bookmarks &amp; extensions</span></div>
    ${box(`<img src="${logo}" width="20" height="20">`, "Synchrome app", "connector · sync engine")}
    <div class="note">${svg("lock", t.muted)}extension settings &amp; preferences are written only while the browser is closed</div></div>`;
  return `<!doctype html><html><head><meta charset="utf-8"><style>
  html,body{margin:0}
  body{width:1200px;background:${t.bg};color:${t.fg};font:15px/1.45 -apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;padding:40px 48px;box-sizing:border-box}
  .row{display:grid;grid-template-columns:1fr 150px 250px 150px 1fr;align-items:center}
  .mac{background:${t.mac};border:1px solid ${t.line};border-radius:16px;padding:18px 18px 16px;display:grid;gap:0}
  .macname{display:flex;align-items:center;gap:8px;color:${t.muted};font-size:13px;font-weight:600;letter-spacing:.02em;margin-bottom:14px;text-transform:uppercase}
  .box{background:${t.card};border:1px solid ${t.line};border-radius:12px;padding:14px 16px}
  .hd{display:flex;align-items:center;gap:10px}.hd b{font-weight:650}
  .sub{color:${t.muted};font-size:13px;margin-top:2px;padding-left:30px}
  .arrow.v{height:64px;margin-left:34px;border-left:2px solid ${t.accent};position:relative;display:flex;align-items:center}
  .arrow.v::before,.arrow.v::after{content:"";position:absolute;left:-6px;border:5px solid transparent}
  .arrow.v::before{top:-2px;border-bottom:7px solid ${t.accent};border-top:0}
  .arrow.v::after{bottom:-2px;border-top:7px solid ${t.accent};border-bottom:0}
  .arrow.v span{padding-left:14px;color:${t.muted};font-size:12.5px}
  .note{display:flex;gap:8px;align-items:flex-start;color:${t.muted};font-size:12.5px;margin-top:12px}
  .note svg{width:15px;height:15px;flex:none;margin-top:2px}
  .h{position:relative;height:2px;background:${t.info};margin:0 6px}
  .h::before,.h::after{content:"";position:absolute;top:-5px;border:6px solid transparent}
  .h::before{left:-4px;border-right:8px solid ${t.info};border-left:0}
  .h::after{right:-4px;border-left:8px solid ${t.info};border-right:0}
  .hl{color:${t.muted};font-size:12.5px;text-align:center;margin-top:8px}
  .cloud{background:${t.cloud};border:1px solid ${t.line};border-radius:16px;padding:20px;text-align:center}
  .cloud b{display:block;font-size:16px;margin:6px 0 4px}
  .cloud .sub{padding:0}
  .never{margin-top:26px;text-align:center;color:${t.muted};font-size:13px}
  .never b{color:${t.fg}}
  </style></head><body>
  <div class="row">${mac("This Mac")}
    <div><div class="h"></div><div class="hl">HTTPS (TLS)<br>+ Realtime wake-up</div></div>
    <div class="cloud">${svg("cloud", t.info).replace('width="20" height="20"', 'width="30" height="30"')}<b>Synchrome Cloud</b>
      <div class="sub">your account<br>change journals · snapshots<br>per-account access rules</div></div>
    <div><div class="h"></div><div class="hl">HTTPS (TLS)<br>+ Realtime wake-up</div></div>
    ${mac("Another Mac")}</div>
  <div class="never"><b>Never synchronized:</b> passwords, cookies, sessions or arbitrary browser files.</div>
  </body></html>`;
}

const browser = await chromium.launch({ channel: "chrome" });
for (const theme of ["light", "dark"]) {
  const ctx = await browser.newContext({ viewport: { width: 1200, height: 900 }, deviceScaleFactor: 2 });
  const p = await ctx.newPage();
  await p.setContent(page(theme));
  await p.evaluate(() => Promise.all([...document.images].map((i) => i.decode())));
  await p.waitForTimeout(300);
  await p.locator("body").screenshot({ path: `${OUT}/how-it-works-${theme}.png` });
  await ctx.close();
  console.log("saved", theme);
}
await browser.close();
