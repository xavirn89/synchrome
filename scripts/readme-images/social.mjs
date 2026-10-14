// GitHub social preview, 1280x640.
// usage: node social.mjs <out.png> <repo-root> <raw-dir>
import { chromium } from "playwright";
import fs from "node:fs";
const [OUT, REPO, RAW] = process.argv.slice(2);
const logo = `data:image/svg+xml;base64,${fs.readFileSync(`${REPO}/apps/desktop/public/synchrome.svg`).toString("base64")}`;
const shot = `data:image/png;base64,${fs.readFileSync(`${RAW}/overview-light.png`).toString("base64")}`;
const html = `<!doctype html><html><head><style>html,body{margin:0}
body{width:1280px;height:640px;overflow:hidden;background:radial-gradient(120% 120% at 10% 0%,#e6f1ed,#cfe4dc);font-family:-apple-system,BlinkMacSystemFont,sans-serif;color:#203a37;position:relative}
.t{position:absolute;left:80px;top:150px;width:470px}
.t img{width:84px;height:84px}
h1{font-size:64px;letter-spacing:-.02em;margin:22px 0 10px;font-weight:700}
p{font-size:27px;line-height:1.35;margin:0;color:#3f5752}
.w{position:absolute;left:600px;top:80px;width:880px;border-radius:12px;overflow:hidden;box-shadow:0 30px 70px rgba(23,50,40,.28),0 0 0 1px rgba(0,0,0,.1);background:#fff}
.bar{height:30px;background:#eef2f0;border-bottom:1px solid #d6dfdb;display:flex;gap:7px;align-items:center;padding-left:14px}.bar i{width:11px;height:11px;border-radius:50%}
.w>img{display:block;width:100%}
</style></head><body><div class="t"><img src="${logo}"><h1>Synchrome</h1><p>Sync your Chromium browser profiles across your Macs.</p></div>
<div class="w"><div class="bar"><i style="background:#ff5f57"></i><i style="background:#febc2e"></i><i style="background:#28c840"></i></div><img src="${shot}"></div></body></html>`;
const b = await chromium.launch({ channel: "chrome" });
const p = await (await b.newContext({ viewport: { width: 1280, height: 640 } })).newPage();
await p.setContent(html);
await p.evaluate(() => Promise.all([...document.images].map((i) => i.decode())));
await p.screenshot({ path: OUT });
await b.close();
