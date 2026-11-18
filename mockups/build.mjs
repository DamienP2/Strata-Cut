// Builds mockups/strata-cut.html from mockups/src/strata-cut.html: inlines the lucide icons it uses.
// Usage: node mockups/build.mjs <lucide-static/icons dir>
import fs from 'node:fs';
import path from 'node:path';
const here = path.dirname(new URL(import.meta.url).pathname);
const iconsDir = process.argv[2];
let html = fs.readFileSync(path.join(here, 'src', 'strata-cut.html'), 'utf8');
const names = new Set([...html.matchAll(/data-i="([a-z0-9-]+)"/g), ...html.matchAll(/ic\('([a-z0-9-]+)'/g), ...html.matchAll(/'(square|circle-dot)'/g)].map((m) => m[1]));
const map = {};
for (const n of names) {
  const src = fs.readFileSync(path.join(iconsDir, `${n}.svg`), 'utf8');
  map[n] = src.slice(src.indexOf('>', src.indexOf('<svg')) + 1, src.lastIndexOf('</svg>')).replace(/\s*\n\s*/g, '');
}
const F = 'M145.93,22.93v-10.33h-35.97c2.12,2.08,7.43,7.77,7.92,13.05-.05-.42-.28-1.57-1.25-3.27-.98-1.72-2.72-4-5.8-6.66-.03-.03-.06-.05-.09-.08-1.11-.96-2.39-1.98-3.87-3.03C98.07,5.95,95.14,2.38,94.38.01c0,0-2.69,9.66,4.5,17.36,0,0-1.74-.36-3.41-1.02,0,0,2.98,7.41,11.12,11.33,0,0-2.11.07-3.92-.14,0,0,1.93,2.92,7.66,5.88v8.46c0,.57-.46,1.04-1.04,1.04h-3.97v7.83h24.8v-7.85h-3.76c-.57,0-1.04-.46-1.04-1.04v-3.41c1.19.21,4.51,1.95,5.96,3.12,3.92,3.17,8.11,8.05,8.11,8.05l1.83,1.13s-5.07-6.05-8.33-9.3c-2.27-2.27-6.1-6.1-6.1-6.1l-.2-.22h9.57v-7.19h-10.82v-7.51l9.78-.04c.58,0,1.04.46,1.04,1.04v1.5h9.8-.03Z';
html = html.replace(/(<svg class="brand-f-lead"[^>]*><path fill="currentColor" d=")[^"]*"/, `$1${F}"`);
html = html.replace('/*ICONS*/{}', JSON.stringify(map));
fs.writeFileSync(path.join(here, 'strata-cut.html'), html);
console.log(`${names.size} icons`);
