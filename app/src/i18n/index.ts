import { de, type MessageKey } from './de';
import { en } from './en';

export type Lang = 'de' | 'en';
export type LangPref = 'auto' | Lang;
export type { MessageKey };

const tables: Record<Lang, Record<MessageKey, string>> = { de, en };

/** `auto` follows the browser/OS language; German is the product's home language. */
export function resolveLang(pref: LangPref): Lang {
  if (pref === 'de' || pref === 'en') return pref;
  const nav = typeof navigator === 'undefined' ? 'de' : navigator.language || 'de';
  return nav.toLowerCase().startsWith('de') ? 'de' : 'en';
}

export type Params = Record<string, string | number>;

export function translate(lang: Lang, key: MessageKey, params?: Params): string {
  const raw = tables[lang][key] ?? de[key] ?? key;
  if (!params) return raw;
  return raw.replace(/\{(\w+)\}/g, (_m, name: string) => String(params[name] ?? `{${name}}`));
}

/** True if a dynamic key exists (used for per-warning-code and per-hotkey strings). */
export function hasKey(key: string): key is MessageKey {
  return key in de;
}
