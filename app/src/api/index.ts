import type { Backend } from './backend';
import { createMockBackend } from './mock';
import { createTauriBackend, insideTauri } from './tauri';

/** Mock mode: outside Tauri (plain browser / `npm run dev`) or with `?mock` in the URL. */
export function wantsMock(): boolean {
  if (typeof location === 'undefined') return false;
  return new URLSearchParams(location.search).has('mock') || !insideTauri();
}

let backend: Backend | null = null;

export function getBackend(): Backend {
  backend ??= wantsMock() ? createMockBackend() : createTauriBackend();
  return backend;
}

export type { Backend } from './backend';
