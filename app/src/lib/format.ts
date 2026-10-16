import type { Lang } from '../i18n';

export function formatBytes(bytes: number, lang: Lang = 'de'): string {
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let v = bytes;
  let i = 0;
  while (v >= 1000 && i < units.length - 1) {
    v /= 1000;
    i++;
  }
  const digits = i === 0 ? 0 : v < 10 ? 1 : 0;
  return `${v.toLocaleString(lang, { maximumFractionDigits: digits, minimumFractionDigits: digits })} ${units[i]}`;
}

/** 1100 µs -> "1,1" (one decimal below 10 ms, none above). */
export function formatMs(us: number | null | undefined, lang: Lang = 'de'): string {
  if (us == null) return '–';
  const ms = us / 1000;
  return ms.toLocaleString(lang, { maximumFractionDigits: ms < 10 ? 1 : 0, minimumFractionDigits: ms < 10 ? 1 : 0 });
}

export function formatNumber(n: number, lang: Lang = 'de', digits = 0): string {
  return n.toLocaleString(lang, { maximumFractionDigits: digits, minimumFractionDigits: digits });
}

/** "3 h 17 min" style duration since a unix-ms timestamp. */
export function formatSince(sinceMs: number | null | undefined, now = Date.now()): string {
  if (!sinceMs) return '–';
  const s = Math.max(0, Math.floor((now - sinceMs) / 1000));
  const d = Math.floor(s / 86400);
  const h = Math.floor((s % 86400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  if (d > 0) return `${d} d ${h} h`;
  if (h > 0) return `${h} h ${m} min`;
  if (m > 0) return `${m} min`;
  return `${s} s`;
}

export function formatClock(sec: number): string {
  const m = Math.floor(sec / 60);
  const s = sec % 60;
  return `${m}:${String(s).padStart(2, '0')}`;
}

export function formatTimeAgo(ms: number, lang: Lang = 'de', now = Date.now()): string {
  const rtf = new Intl.RelativeTimeFormat(lang, { numeric: 'auto' });
  const diff = (ms - now) / 1000;
  const abs = Math.abs(diff);
  if (abs < 60) return rtf.format(Math.round(diff), 'second');
  if (abs < 3600) return rtf.format(Math.round(diff / 60), 'minute');
  if (abs < 86400) return rtf.format(Math.round(diff / 3600), 'hour');
  return rtf.format(Math.round(diff / 86400), 'day');
}

/** Latency bucket for colouring: excellent < 2 ms, good < 8, fair < 25, else poor. */
export type Quality = 'excellent' | 'good' | 'fair' | 'poor';
export function rttQuality(us: number | null | undefined, link?: string): Quality {
  if (us == null) return 'poor';
  const ms = us / 1000;
  const scale = link === 'wireless' ? 3 : 1;
  if (ms < 2 * scale) return 'excellent';
  if (ms < 8 * scale) return 'good';
  if (ms < 25 * scale) return 'fair';
  return 'poor';
}

export function basename(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).pop() ?? path;
}
