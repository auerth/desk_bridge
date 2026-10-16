import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { open as openDialog } from '@tauri-apps/plugin-dialog';
import { openUrl } from '@tauri-apps/plugin-opener';
import type { AppInfo, Backend, ConnectionInfo, FileDropEvent, UpdateInfo, Unlisten } from './backend';
import type { AppState, UiEvent, UiRequest } from './types';

/** Subscribes to a Tauri event; returns a synchronous unlisten function. */
function subscribe<T>(name: string, cb: (payload: T) => void): Unlisten {
  let un: (() => void) | undefined;
  let cancelled = false;
  void listen<T>(name, (e) => cb(e.payload)).then((u) => {
    if (cancelled) u();
    else un = u;
  });
  return () => {
    cancelled = true;
    un?.();
  };
}

export function createTauriBackend(): Backend {
  const win = getCurrentWindow();
  return {
    kind: 'tauri',
    request: <T,>(req: UiRequest) => invoke<T>('agent_request', { req }),
    onState: (cb) => subscribe<AppState>('agent://state', cb),
    onEvent: (cb) => subscribe<UiEvent>('agent://event', cb),
    onConnection: (cb) => subscribe<ConnectionInfo>('agent://connection', cb),
    appInfo: () => invoke<AppInfo>('app_info'),
    frontendReady: () => void invoke('frontend_ready'),
    window: {
      minimize: () => void win.minimize(),
      toggleMaximize: () => void win.toggleMaximize(),
      close: () => void win.close(),
      isMaximized: () => win.isMaximized(),
      onMaximizeChange: (cb) => {
        let un: (() => void) | undefined;
        let cancelled = false;
        void win.onResized(() => void win.isMaximized().then(cb)).then((u) => {
          if (cancelled) u();
          else un = u;
        });
        return () => {
          cancelled = true;
          un?.();
        };
      },
    },
    pickFolder: async () => {
      const r = await openDialog({ directory: true, multiple: false });
      return typeof r === 'string' ? r : null;
    },
    pickFiles: async () => {
      const r = await openDialog({ directory: false, multiple: true });
      return Array.isArray(r) ? r : typeof r === 'string' ? [r] : [];
    },
    onFileDrop: (cb: (e: FileDropEvent) => void) => {
      let un: (() => void) | undefined;
      let cancelled = false;
      void getCurrentWebview()
        .onDragDropEvent((e) => {
          const p = e.payload;
          if (p.type === 'drop') cb({ type: 'drop', paths: p.paths });
          else cb({ type: p.type });
        })
        .then((u) => {
          if (cancelled) u();
          else un = u;
        });
      return () => {
        cancelled = true;
        un?.();
      };
    },
    openUrl: (url) => openUrl(url),
    openLogsFolder: () => invoke('open_logs_folder'),
    revealPath: (path) => invoke('reveal_path', { path }),
    checkUpdate: () => invoke<UpdateInfo | null>('updater_check'),
    installUpdate: async () => {
      await invoke('updater_install');
    },
    setWindowTheme: (theme) => void invoke('set_window_theme', { theme }),
    openMain: () => void invoke('open_main'),
    hideFlyout: () => void invoke('hide_flyout'),
    onOsd: (cb) => subscribe<{ icon: string; text: string }>('osd://show', cb),
    onFlyoutOpened: (cb) => subscribe<null>('flyout://opened', () => cb()),
  };
}

export function insideTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}
