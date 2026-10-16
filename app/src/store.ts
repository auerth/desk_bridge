import { create } from 'zustand';
import { getBackend } from './api';
import type { AppInfo, ConnectionInfo } from './api/backend';
import {
  defaultConfig,
  type AppState,
  type AudioDeviceDto,
  type Config,
  type UiEvent,
  type UiRequest,
} from './api/types';
import { resolveLang, translate, type Lang } from './i18n';

export type Route = 'dashboard' | 'screens' | 'audio' | 'files' | 'focus' | 'hotkeys' | 'settings' | 'about';
export const ROUTES: Route[] = ['dashboard', 'screens', 'audio', 'files', 'focus', 'hotkeys', 'settings', 'about'];

export interface Toast {
  id: number;
  level: 'info' | 'error' | 'success';
  text: string;
  /** Optional action (e.g. reveal a file). */
  actionLabel?: string;
  action?: () => void;
}

export interface PairingResult {
  ok: boolean;
  message: string;
  peerName: string | null;
  at: number;
}

interface UiStore {
  backendKind: 'tauri' | 'mock';
  appInfo: AppInfo | null;
  conn: ConnectionInfo;
  /** First get_state/get_config round trip finished (or failed). */
  loaded: boolean;
  state: AppState | null;
  config: Config | null;
  route: Route;
  lang: Lang;
  audioDevices: { render: AudioDeviceDto[]; capture: AudioDeviceDto[] } | null;
  pairingResult: PairingResult | null;
  updateOffer: { version: string; notes: string } | null;
  /** The partner offers an installer; waits for the user's answer. */
  partnerOffer: { version: string; size: number; from: string } | null;
  /** A verified partner installer was received and may be started. */
  partnerReady: { path: string; version: string } | null;
  toasts: Toast[];
  /** Warning codes the user dismissed in this session. */
  dismissedWarnings: string[];

  init(): () => void;
  navigate(route: Route): void;
  /** Sends a request; on failure shows an error toast and resolves to `undefined`. */
  send<T = unknown>(req: UiRequest, opts?: { silent?: boolean }): Promise<T | undefined>;
  /** Optimistic config edit, debounced write to the agent. */
  updateConfig(mutate: (c: Config) => void): void;
  pushToast(t: Omit<Toast, 'id'>): void;
  dismissToast(id: number): void;
  dismissWarning(code: string): void;
  clearPartnerOffer(): void;
  clearPartnerReady(): void;
}

let toastId = 1;
let flushTimer: ReturnType<typeof setTimeout> | null = null;
let configVersion = 0;

function errorText(lang: Lang, e: unknown): string {
  const raw = e instanceof Error ? e.message : String(e);
  if (raw === 'agent_offline') return translate(lang, 'agent.errorOffline');
  if (raw === 'timeout') return translate(lang, 'agent.errorTimeout');
  return translate(lang, 'agent.errorGeneric', { error: raw });
}

export const useStore = create<UiStore>((set, get) => ({
  backendKind: getBackend().kind,
  appInfo: null,
  conn: { connected: false },
  loaded: false,
  state: null,
  config: null,
  route: 'dashboard',
  lang: resolveLang('auto'),
  audioDevices: null,
  pairingResult: null,
  updateOffer: null,
  partnerOffer: null,
  partnerReady: null,
  toasts: [],
  dismissedWarnings: [],

  init() {
    const backend = getBackend();
    const unsubs: (() => void)[] = [];

    const loadAll = async () => {
      try {
        const [state, config] = await Promise.all([
          backend.request<AppState>({ type: 'get_state' }),
          backend.request<Config>({ type: 'get_config' }),
        ]);
        // A successful request proves the pipe is up, even if the one-shot `agent://connection`
        // event fired before this window had subscribed.
        set({ state, config, lang: resolveLang(config.general.language), loaded: true, conn: { connected: true } });
      } catch {
        set({ loaded: true });
      }
    };

    unsubs.push(
      backend.onState((state) => {
        // State pushes only exist while the pipe is connected: heal a missed connection event.
        const wasOffline = !get().conn.connected;
        set({ state, conn: { connected: true } });
        if (wasOffline && !get().config) void loadAll();
      }),
      backend.onEvent((e: UiEvent) => {
        const lang = get().lang;
        switch (e.kind) {
          case 'audio_devices':
            set({ audioDevices: { render: e.render, capture: e.capture } });
            break;
          case 'pairing_result':
            set({ pairingResult: { ok: e.ok, message: e.message, peerName: e.peer_name, at: Date.now() } });
            break;
          case 'notification':
            get().pushToast({ level: e.level === 'error' ? 'error' : 'info', text: e.text });
            break;
          case 'update_available':
            set({ updateOffer: { version: e.version, notes: e.notes } });
            break;
          case 'partner_update_offered':
            set({ partnerOffer: { version: e.version, size: e.size, from: e.from } });
            break;
          case 'partner_update_ready':
            set({ partnerReady: { path: e.path, version: e.version }, partnerOffer: null });
            break;
          case 'diagnostics_exported':
            get().pushToast({
              level: 'success',
              text: `${translate(lang, 'settings.exportDone')}: ${e.path}`,
              actionLabel: translate(lang, 'files.showInFolder'),
              action: () => void backend.revealPath(e.path),
            });
            break;
          case 'osd':
            break;
        }
      }),
      backend.onConnection((conn) => {
        const was = get().conn.connected;
        set({ conn });
        if (conn.connected && !was) void loadAll();
        if (!conn.connected) set({ loaded: true });
      }),
    );

    void backend.appInfo().then((appInfo) => {
      set({ appInfo });
      document.documentElement.dataset.mica = appInfo.mica ? '1' : '0';
    });
    void loadAll();
    return () => unsubs.forEach((u) => u());
  },

  navigate(route) {
    set({ route });
  },

  async send<T>(req: UiRequest, opts?: { silent?: boolean }) {
    try {
      return await getBackend().request<T>(req);
    } catch (e) {
      if (!opts?.silent) get().pushToast({ level: 'error', text: errorText(get().lang, e) });
      return undefined;
    }
  },

  updateConfig(mutate) {
    const current = get().config ?? defaultConfig();
    const next = structuredClone(current);
    mutate(next);
    configVersion += 1;
    const version = configVersion;
    set({ config: next, lang: resolveLang(next.general.language) });
    if (flushTimer) clearTimeout(flushTimer);
    flushTimer = setTimeout(() => {
      flushTimer = null;
      const toSend = get().config;
      if (!toSend) return;
      void get()
        .send<Config>({ type: 'set_config', config: toSend })
        .then((stored) => {
          // Only adopt the agent's (clamped) version if nothing newer was edited meanwhile.
          if (stored && version === configVersion) set({ config: stored });
        });
    }, 140);
  },

  pushToast(t) {
    const id = toastId++;
    set((s) => ({ toasts: [...s.toasts.slice(-3), { ...t, id }] }));
    setTimeout(() => get().dismissToast(id), t.action ? 9000 : t.level === 'error' ? 7000 : 4500);
  },

  dismissToast(id) {
    set((s) => ({ toasts: s.toasts.filter((x) => x.id !== id) }));
  },

  clearPartnerOffer() {
    set({ partnerOffer: null });
  },

  clearPartnerReady() {
    set({ partnerReady: null });
  },

  dismissWarning(code) {
    set((s) => ({ dismissedWarnings: [...s.dismissedWarnings, code] }));
  },
}));

/** Convenience selectors. */
export const useConfig = () => useStore((s) => s.config) ?? defaultConfig();
