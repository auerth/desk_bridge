// Hotkey strings: "Ctrl+Alt+Shift+F", "ScrollLock", "Ctrl+Alt+End" (same grammar as crates/platform/src/hotkey.rs).
// Canonical order: Ctrl, Alt, Shift, Win, main key.

import type { Lang } from '../i18n';

const CODE_NAMES: Record<string, string> = {
  Enter: 'Enter',
  Escape: 'Esc',
  Backspace: 'Backspace',
  Tab: 'Tab',
  Space: 'Space',
  CapsLock: 'CapsLock',
  PrintScreen: 'PrintScreen',
  ScrollLock: 'ScrollLock',
  Pause: 'Pause',
  Insert: 'Insert',
  Home: 'Home',
  PageUp: 'PageUp',
  Delete: 'Delete',
  End: 'End',
  PageDown: 'PageDown',
  ArrowRight: 'Right',
  ArrowLeft: 'Left',
  ArrowDown: 'Down',
  ArrowUp: 'Up',
  NumLock: 'NumLock',
};

/** Keys that are fine without a modifier (they do not interfere with typing). */
const STANDALONE = new Set(['ScrollLock', 'Pause', 'PrintScreen']);

export type CaptureResult =
  | { ok: true; value: string }
  | { ok: false; reason: 'modifier-only' | 'unsupported' | 'need-modifier' };

function mainKeyName(code: string): string | null {
  if (/^Key[A-Z]$/.test(code)) return code.slice(3);
  if (/^Digit[0-9]$/.test(code)) return code.slice(5);
  if (/^F([1-9]|1[0-2])$/.test(code)) return code;
  return CODE_NAMES[code] ?? null;
}

const MODIFIER_CODES = new Set([
  'ControlLeft',
  'ControlRight',
  'ShiftLeft',
  'ShiftRight',
  'AltLeft',
  'AltRight',
  'MetaLeft',
  'MetaRight',
]);

/** Converts a keydown event to a hotkey string (or says why it cannot be one). */
export function captureHotkey(e: KeyboardEvent): CaptureResult {
  if (MODIFIER_CODES.has(e.code)) return { ok: false, reason: 'modifier-only' };
  const key = mainKeyName(e.code);
  if (!key) return { ok: false, reason: 'unsupported' };
  const mods: string[] = [];
  if (e.ctrlKey) mods.push('Ctrl');
  if (e.altKey) mods.push('Alt');
  if (e.shiftKey) mods.push('Shift');
  if (e.metaKey) mods.push('Win');
  const hasStrong = e.ctrlKey || e.altKey || e.metaKey;
  if (!hasStrong && !STANDALONE.has(key)) return { ok: false, reason: 'need-modifier' };
  return { ok: true, value: [...mods, key].join('+') };
}

const MOD_ORDER = ['Ctrl', 'Alt', 'Shift', 'Win'];

const ALIASES: Record<string, string> = {
  rollen: 'scrolllock',
  ende: 'end',
  leertaste: 'space',
  rücktaste: 'backspace',
  entf: 'delete',
  einfg: 'insert',
  pos1: 'home',
  bildauf: 'pageup',
  bildab: 'pagedown',
  druck: 'printscreen',
  escape: 'esc',
  return: 'enter',
};

/** Canonical comparison form (case-insensitive, German aliases accepted). */
export function normalizeHotkey(spec: string): string {
  const mods = new Set<string>();
  let key = '';
  for (const raw of spec.split('+')) {
    const p = raw.trim().toLowerCase();
    if (['ctrl', 'strg', 'control'].includes(p)) mods.add('Ctrl');
    else if (p === 'alt') mods.add('Alt');
    else if (p === 'shift') mods.add('Shift');
    else if (['win', 'gui', 'meta', 'super'].includes(p)) mods.add('Win');
    else key = p;
  }
  const k = ALIASES[key] ?? key;
  return [...MOD_ORDER.filter((m) => mods.has(m)), k].join('+').toLowerCase();
}

const KEY_LABELS: Record<Lang, Record<string, string>> = {
  de: {
    ScrollLock: 'Rollen',
    End: 'Ende',
    Backspace: 'Rücktaste',
    Space: 'Leertaste',
    Delete: 'Entf',
    Insert: 'Einfg',
    PageUp: 'Bild ↑',
    PageDown: 'Bild ↓',
    Home: 'Pos1',
    Enter: 'Eingabe',
    PrintScreen: 'Druck',
    Left: '←',
    Right: '→',
    Up: '↑',
    Down: '↓',
    Ctrl: 'Strg',
  },
  en: {
    ScrollLock: 'Scroll Lock',
    PrintScreen: 'Print Screen',
    PageUp: 'Page Up',
    PageDown: 'Page Down',
    Left: '←',
    Right: '→',
    Up: '↑',
    Down: '↓',
    CapsLock: 'Caps Lock',
    NumLock: 'Num Lock',
  },
};

/** Parts to render as key caps, localised ("Strg", "Alt", "Shift", "F"). */
export function hotkeyParts(spec: string, lang: Lang): string[] {
  return spec
    .split('+')
    .map((p) => p.trim())
    .filter(Boolean)
    .map((p) => KEY_LABELS[lang][p] ?? p);
}

/** Plain text for aria labels. */
export function hotkeyText(spec: string, lang: Lang): string {
  return hotkeyParts(spec, lang).join('+');
}
