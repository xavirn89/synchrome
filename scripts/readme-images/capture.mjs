// Raw README screenshots from the real frontend in its token-free development preview.
// usage: node capture.mjs <outdir>
import { chromium } from "playwright";
import fs from "node:fs";

const BASE = "http://localhost:4317";
const OUT = process.argv[2];
fs.mkdirSync(OUT, { recursive: true });
const browser = await chromium.launch({ channel: "chrome" });

async function open(scenario, theme) {
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 800 }, deviceScaleFactor: 2, colorScheme: theme, reducedMotion: "reduce" });
  await ctx.addInitScript((t) => { try { localStorage.setItem("synchrome.appearance", t); } catch {} }, theme);
  const p = await ctx.newPage();
  await p.goto(`${BASE}/?preview=${scenario}`);
  await p.addStyleTag({ content: ".preview-banner{display:none!important}" });
  if (scenario !== "auth") await p.waitForSelector('[data-testid="nav-overview"]', { timeout: 20000 });
  await p.waitForTimeout(1200);
  return { ctx, p };
}

async function shot(p, name) {
  await p.waitForTimeout(500);
  await p.screenshot({ path: `${OUT}/${name}.png` });
  console.log("saved", name);
}

for (const theme of ["dark", "light"]) {
  const { ctx, p } = await open("showcase", theme);
  await shot(p, `overview-${theme}`);
  await p.getByText("Personal", { exact: true }).first().click();
  await p.waitForTimeout(800);
  await shot(p, `profile-${theme}`);
  await p.click('[data-testid="nav-devices"]');
  await shot(p, `devices-${theme}`);
  await p.click('[data-testid="nav-settings"]');
  await shot(p, `settings-${theme}`);
  await ctx.close();

  const w = await open("workspace", theme);
  await w.p.getByText("Research", { exact: true }).first().click();
  await w.p.waitForTimeout(800);
  await w.p.getByRole("button", { name: "Finish setup" }).first().click();
  await w.p.waitForTimeout(900);
  await shot(w.p, `setup-${theme}`);
  await w.p.keyboard.press("Escape");
  await w.p.waitForTimeout(500);
  await w.p.click('[data-testid="nav-activity"]');
  await w.p.waitForTimeout(900);
  await shot(w.p, `activity-${theme}`);
  await w.ctx.close();

  const a = await open("auth", theme);
  await a.p.waitForSelector("form.login", { timeout: 20000 });
  await a.p.waitForTimeout(600);
  await shot(a.p, `signin-${theme}`);
  await a.ctx.close();
}
await browser.close();
