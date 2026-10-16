import { useEffect, useState } from 'react';
import { getBackend } from '../api';

export type ThemePref = 'system' | 'light' | 'dark';
export type Resolved = 'light' | 'dark';

function systemDark(): boolean {
  return typeof matchMedia !== 'undefined' && matchMedia('(prefers-color-scheme: dark)').matches;
}

export function resolveTheme(pref: ThemePref): Resolved {
  return pref === 'dark' || (pref === 'system' && systemDark()) ? 'dark' : 'light';
}

/** Applies the theme to <html>, remembers it for the next start (no flash) and tells the native window. */
export function applyTheme(pref: ThemePref, notifyWindow = true) {
  const resolved = resolveTheme(pref);
  const root = document.documentElement;
  root.dataset.theme = resolved;
  root.style.colorScheme = resolved;
  try {
    localStorage.setItem('db.theme', pref);
  } catch {
    /* private mode */
  }
  if (notifyWindow) getBackend().setWindowTheme(pref);
}

/** Current resolved theme (follows the `data-theme` attribute). */
export function useResolvedTheme(): Resolved {
  const read = () => (document.documentElement.dataset.theme === 'dark' ? 'dark' : 'light');
  const [theme, setTheme] = useState<Resolved>(read);
  useEffect(() => {
    const mo = new MutationObserver(() => setTheme(read()));
    mo.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] });
    return () => mo.disconnect();
  }, []);
  return theme;
}
