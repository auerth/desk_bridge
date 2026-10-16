// Generates the application icon set and the tray icons from the SVG sources.
//
//   npm run icons            (variant A is the default)
//   npm run icons -- b       (pick another variant: a | b | c)
//
// Output (committed to the tree, `tauri build` only needs the files):
//   src-tauri/icons/{32x32.png,128x128.png,128x128@2x.png,icon.png,icon.ico}
//   src-tauri/icons/tray/{connected,disconnected,mic-live,focus}[-light|-dark][@2x].png
//
// Tray naming: the suffix names the *taskbar* theme the icon is made for.
//   (no suffix) and "-dark" = white glyph for a dark taskbar,   "-light" = navy glyph for a light taskbar.

import { Resvg } from '@resvg/resvg-js';
import pngToIco from 'png-to-ico';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, '..');
const variant = (process.argv[2] ?? 'a').toLowerCase();
const iconDir = join(root, 'src-tauri', 'icons');
const trayDir = join(iconDir, 'tray');
mkdirSync(trayDir, { recursive: true });

/** Renders an SVG string at an exact pixel width. */
function render(svg, width) {
  return new Resvg(svg, {
    fitTo: { mode: 'width', value: width },
    font: { loadSystemFonts: false },
  })
    .render()
    .asPng();
}

// ---------------------------------------------------------------------------------------------
// App icon
// ---------------------------------------------------------------------------------------------

const source = readFileSync(join(root, 'src', 'brand', 'app-icon', `variant-${variant}.svg`), 'utf8');

/** At small sizes the line art needs heavier strokes to stay legible. */
function appSvgFor(size) {
  if (size > 40) return source;
  const bump = size <= 24 ? 1.7 : 1.4;
  return source.replace(/stroke-width="(\d+)"/g, (_m, w) => `stroke-width="${Math.round(Number(w) * bump)}"`);
}

const appPng = (size) => render(appSvgFor(size), size);

writeFileSync(join(iconDir, '32x32.png'), appPng(32));
writeFileSync(join(iconDir, '128x128.png'), appPng(128));
writeFileSync(join(iconDir, '128x128@2x.png'), appPng(256));
writeFileSync(join(iconDir, 'icon.png'), appPng(512));
const icoSizes = [16, 20, 24, 32, 48, 64, 256];
writeFileSync(join(iconDir, 'icon.ico'), await pngToIco(icoSizes.map(appPng)));

// ---------------------------------------------------------------------------------------------
// Tray icons – monochrome motif on a 16 px grid, coloured status dots
// ---------------------------------------------------------------------------------------------

const ORANGE = '#F18557';
const THEMES = {
  dark: { glyph: '#FFFFFF', accent: '#A3C7EB' }, // dark taskbar
  light: { glyph: '#21264E', accent: '#194093' }, // light taskbar
};

function traySvg(kind, theme) {
  const { glyph, accent } = THEMES[theme];
  const off = kind === 'disconnected';
  const filled = kind === 'focus';
  const g = off ? `opacity="0.5"` : '';
  const sw = 1.4;
  // Monitor (left/top), laptop (right/bottom), bridge arc between them.
  const monitor = `<rect x="1.2" y="3.2" width="8.6" height="6" rx="1.2" ${filled ? `fill="${glyph}"` : 'fill="none"'} stroke="${glyph}" stroke-width="${sw}"/>
    <path d="M5.5 9.6v1.8M3.3 11.9h4.4" stroke="${glyph}" stroke-width="${sw}" stroke-linecap="round" fill="none"/>`;
  const laptop = `<rect x="10.2" y="7.4" width="4.6" height="3.3" rx="0.8" fill="none" stroke="${glyph}" stroke-width="${sw}"/>
    <path d="M9.4 12.4h6.4" stroke="${glyph}" stroke-width="${sw}" stroke-linecap="round" fill="none"/>`;
  const arc = '';
  let dot = '';
  if (kind === 'mic-live') dot = `<circle cx="13" cy="3.6" r="2.6" fill="${ORANGE}"/>`;
  if (kind === 'focus') dot = `<circle cx="13" cy="3.6" r="2.6" fill="${accent}"/>`;
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" width="16" height="16"><g ${g}>${monitor}${laptop}</g>${arc}${dot}</svg>`;
}

for (const kind of ['connected', 'disconnected', 'mic-live', 'focus']) {
  for (const theme of ['dark', 'light']) {
    const svg = traySvg(kind, theme);
    writeFileSync(join(trayDir, `${kind}-${theme}.png`), render(svg, 16));
    writeFileSync(join(trayDir, `${kind}-${theme}@2x.png`), render(svg, 32));
    if (theme === 'dark') {
      writeFileSync(join(trayDir, `${kind}.png`), render(svg, 16));
      writeFileSync(join(trayDir, `${kind}@2x.png`), render(svg, 32));
    }
  }
}

// Preview sheet (not part of the build) to inspect the 16 px rendering: 16x enlargement, nearest neighbour.
const sheetDir = process.env.ICON_SHEET_DIR;
if (sheetDir) {
  mkdirSync(sheetDir, { recursive: true });
  const kinds = ['connected', 'disconnected', 'mic-live', 'focus'];
  const cell = 16 * 12;
  const parts = [];
  kinds.forEach((kind, i) => {
    ['dark', 'light'].forEach((theme, j) => {
      const png = render(traySvg(kind, theme), 16).toString('base64');
      parts.push(
        `<rect x="${i * cell}" y="${j * cell}" width="${cell}" height="${cell}" fill="${theme === 'dark' ? '#202020' : '#F3F3F3'}"/>` +
          `<image x="${i * cell}" y="${j * cell}" width="${cell}" height="${cell}" image-rendering="pixelated" href="data:image/png;base64,${png}"/>`,
      );
    });
  });
  const sheet = `<svg xmlns="http://www.w3.org/2000/svg" width="${cell * 4}" height="${cell * 2 + 200}">${parts.join('')}</svg>`;
  writeFileSync(join(sheetDir, 'tray-sheet.png'), render(sheet, cell * 4));
  const appSheet = icoSizes.map((s, k) => {
    const b64 = appPng(s).toString('base64');
    return `<image x="${k * 140}" y="${cell * 2 + 20}" width="${s > 64 ? 128 : Math.max(s * 2, 32)}" height="${s > 64 ? 128 : Math.max(s * 2, 32)}" image-rendering="pixelated" href="data:image/png;base64,${b64}"/>`;
  });
  const sheet2 = `<svg xmlns="http://www.w3.org/2000/svg" width="${cell * 4}" height="${cell * 2 + 200}"><rect width="100%" height="100%" fill="#808080"/>${appSheet.join('')}</svg>`;
  writeFileSync(join(sheetDir, 'app-sheet.png'), render(sheet2, cell * 4));
}

console.log(`Icons generated (variant ${variant.toUpperCase()}) in ${iconDir}`);
