#!/usr/bin/env node
import { readFileSync, writeFileSync, copyFileSync, mkdirSync, mkdtempSync, rmSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { brandPaths, brandStroke } from '../apps/desktop/src/lib/brand.js';

const root = resolve(fileURLToPath(new URL('..', import.meta.url)));
const desktop = join(root, 'apps/desktop');
const icons = join(desktop, 'src-tauri/icons');
const extension = join(root, 'apps/extension');
const css = readFileSync(join(desktop, 'src/styles.css'), 'utf8');
const token = name => {
  const match = css.match(new RegExp(`--${name}:\\s*(#[a-fA-F0-9]+)`));
  if (!match) throw new Error(`Missing brand color token: ${name}`);
  return match[1];
};
const declarations = block => block.split('\n').filter(line => line.trim().startsWith('--')).join('\n');
const lightTokens = declarations(css.match(/:root \{([\s\S]*?)\n\}/)[1]);
const darkTokens = declarations(css.match(/:root\[data-theme=dark\] \{([\s\S]*?)\n\}/)[1]);
writeFileSync(join(extension, 'brand-tokens.css'), `/* Generated from the desktop theme by scripts/generate-brand.mjs. */\n:root {\n  color-scheme: light dark;\n${lightTokens}\n}\n@media (prefers-color-scheme: dark) {\n  :root {\n${darkTokens}\n  }\n}\n`);
const teal = token('accent');
const ink = token('fg');
const paths = brandPaths.map(d => `<path d="${d}"/>`).join('');
const mark = (color = 'currentColor') => `<g fill="none" stroke="${color}" stroke-width="${brandStroke}" stroke-linecap="round" stroke-linejoin="round">${paths}</g>`;
const svg = (size, body) => `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${size} ${size}" width="${size}" height="${size}"><title>Synchrome</title>${body}</svg>\n`;
const favicon = svg(24, `<rect width="24" height="24" rx="6" fill="${teal}"/><g transform="translate(1.2 1.2) scale(.9)">${mark('#ffffff')}</g>`);
writeFileSync(join(desktop, 'public/synchrome.svg'), favicon);
writeFileSync(join(extension, 'brand.svg'), svg(24, mark()));
writeFileSync(join(icons, 'icon.svg'), svg(1024, `<defs><filter id="shadow" x="-15%" y="-15%" width="130%" height="140%"><feDropShadow dx="0" dy="12" stdDeviation="16" flood-color="${ink}" flood-opacity=".18"/></filter></defs><rect x="80" y="80" width="864" height="864" rx="200" fill="${teal}" filter="url(#shadow)"/><rect x="82" y="82" width="860" height="860" rx="198" fill="none" stroke="#ffffff" stroke-opacity=".12" stroke-width="4"/><g transform="translate(176 176) scale(28)">${mark('#ffffff')}</g>`));

const scratch = mkdtempSync(join(tmpdir(), 'synchrome-brand-'));
const tauriIcon = (input, output, sizes) => execFileSync('pnpm', ['tauri', 'icon', input, '--output', output, ...sizes.flatMap(size => ['--png', String(size)])], { cwd: desktop, stdio: 'pipe' });
try {
  const rendered = join(scratch, 'rendered');
  tauriIcon(join(icons, 'icon.svg'), rendered, [1024]);
  copyFileSync(join(rendered, '1024x1024.png'), join(icons, 'icon.png'));
  const iconset = join(scratch, 'Synchrome.iconset');
  mkdirSync(iconset);
  for (const size of [16, 32, 128, 256, 512]) {
    for (const scale of [1, 2]) {
      execFileSync('sips', ['-z', String(size * scale), String(size * scale), join(icons, 'icon.png'), '--out', join(iconset, `icon_${size}x${size}${scale === 2 ? '@2x' : ''}.png`)], { stdio: 'pipe' });
    }
  }
  execFileSync('iconutil', ['-c', 'icns', iconset, '-o', join(icons, 'icon.icns')]);
  tauriIcon(join(desktop, 'public/synchrome.svg'), join(scratch, 'companion'), [16, 32, 48, 128]);
  mkdirSync(join(extension, 'icons'), { recursive: true });
  for (const size of [16, 32, 48, 128]) copyFileSync(join(scratch, 'companion', `${size}x${size}.png`), join(extension, 'icons', `${size}.png`));
} finally {
  rmSync(scratch, { recursive: true, force: true });
}
console.log('Generated one brand geometry across SVG, macOS PNG/ICNS and companion icons.');
