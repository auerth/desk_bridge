import type { AppState, UiEvent, UiRequest } from './types';

export interface AppInfo {
  version: string;
  /** Mica backdrop active on the main window (translucent surfaces allowed). */
  mica: boolean;
  start_hidden: boolean;
}

export interface ConnectionInfo {
  /** Named pipe to the agent is up. */
  connected: boolean;
  /** "not_running" | "disconnected" | other io error text */
  reason?: string;
  /** The UI just launched the agent and waits for its pipe. */
  starting?: boolean;
  start_failed?: boolean;
}

export type Unlisten = () => void;

export type FileDropEvent =
  | { type: 'enter' | 'over' | 'leave' }
  | { type: 'drop'; paths: string[] };

export interface UpdateInfo {
  version: string;
  notes: string;
}

/** Everything the UI needs from the outside world; implemented by `tauri.ts` and `mock.ts`. */
export interface Backend {
  readonly kind: 'tauri' | 'mock';
  /** Sends a request to the agent (rejects with `agent_offline`, `timeout` or the agent's error text). */
  request<T = unknown>(req: UiRequest): Promise<T>;
  onState(cb: (s: AppState) => void): Unlisten;
  onEvent(cb: (e: UiEvent) => void): Unlisten;
  onConnection(cb: (c: ConnectionInfo) => void): Unlisten;
  appInfo(): Promise<AppInfo>;
  /** Tells the shell that the first frame is painted (shows the hidden main window). */
  frontendReady(): void;
  window: {
    minimize(): void;
    toggleMaximize(): void;
    close(): void;
    isMaximized(): Promise<boolean>;
    onMaximizeChange(cb: (maximized: boolean) => void): Unlisten;
  };
  pickFolder(): Promise<string | null>;
  pickFiles(): Promise<string[]>;
  /** Picks one installer (.exe) for the partner update. */
  pickInstaller(): Promise<string | null>;
  /** Starts a received installer after the user confirmed (never silently). */
  launchInstaller(path: string): Promise<void>;
  onFileDrop(cb: (e: FileDropEvent) => void): Unlisten;
  openUrl(url: string): Promise<void>;
  openLogsFolder(): Promise<void>;
  revealPath(path: string): Promise<void>;
  checkUpdate(): Promise<UpdateInfo | null>;
  installUpdate(): Promise<void>;
  setWindowTheme(theme: 'system' | 'light' | 'dark'): void;
  openMain(): void;
  hideFlyout(): void;
  /** OSD window: a pill should be shown now. */
  onOsd(cb: (p: { icon: string; text: string }) => void): Unlisten;
  /** Flyout window: it was just opened (refresh, focus first control). */
  onFlyoutOpened(cb: () => void): Unlisten;
}
