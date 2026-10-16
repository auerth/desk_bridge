import { motion, AnimatePresence } from 'motion/react';
import {
  Info,
  Keyboard,
  LayoutDashboard,
  Maximize2,
  Files,
  Monitor,
  Settings as SettingsIcon,
  Volume2,
  X,
  CheckCircle2,
  AlertCircle,
} from 'lucide-react';
import { useEffect, useState } from 'react';
import { getBackend } from '../api';
import appIcon from '../brand/app-icon/variant-a.svg';
import { PRODUCT_NAME } from '../brand';
import type { MessageKey } from '../i18n';
import { useT } from '../i18n/useT';
import { ROUTES, useStore, type Route } from '../store';
import { Banner, Button, Pill } from './ui';

// ---------------------------------------------------------------------------------------------
// Title bar (frameless window with Windows-style caption buttons)
// ---------------------------------------------------------------------------------------------

function CaptionButton({ label, onClick, children, danger }: { label: string; onClick: () => void; children: React.ReactNode; danger?: boolean }) {
  return (
    <button type="button" className={`caption-btn${danger ? ' caption-btn--close' : ''}`} onClick={onClick} aria-label={label} title={label}>
      {children}
    </button>
  );
}

export function Titlebar() {
  const t = useT();
  const backend = getBackend();
  const kind = useStore((s) => s.backendKind);
  const [maximized, setMaximized] = useState(false);

  useEffect(() => {
    let alive = true;
    void backend.window.isMaximized().then((m) => alive && setMaximized(m));
    const un = backend.window.onMaximizeChange((m) => alive && setMaximized(m));
    return () => {
      alive = false;
      un();
    };
  }, [backend]);

  return (
    <div className="titlebar" data-tauri-drag-region>
      <div className="titlebar__brand" data-tauri-drag-region>
        <img src={appIcon} alt="" width={18} height={18} className="titlebar__icon" data-tauri-drag-region />
        <span className="titlebar__title" data-tauri-drag-region>
          {PRODUCT_NAME}
        </span>
        {kind === 'mock' && <Pill tone="accent">{t('common.demo')}</Pill>}
      </div>
      <div className="titlebar__controls">
        <CaptionButton label={t('win.minimize')} onClick={() => backend.window.minimize()}>
          <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden>
            <path d="M0 5.5h10" stroke="currentColor" strokeWidth="1" />
          </svg>
        </CaptionButton>
        <CaptionButton label={maximized ? t('win.restore') : t('win.maximize')} onClick={() => backend.window.toggleMaximize()}>
          {maximized ? (
            <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden fill="none" stroke="currentColor" strokeWidth="1">
              <rect x="0.5" y="2.5" width="7" height="7" rx="1" />
              <path d="M2.5 2.5v-1a1 1 0 0 1 1-1h5a1 1 0 0 1 1 1v5a1 1 0 0 1-1 1h-1" />
            </svg>
          ) : (
            <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden fill="none" stroke="currentColor" strokeWidth="1">
              <rect x="0.5" y="0.5" width="9" height="9" rx="1.5" />
            </svg>
          )}
        </CaptionButton>
        <CaptionButton label={t('win.close')} onClick={() => backend.window.close()} danger>
          <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden>
            <path d="M0.5 0.5l9 9M9.5 0.5l-9 9" stroke="currentColor" strokeWidth="1" />
          </svg>
        </CaptionButton>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------------------------
// Navigation
// ---------------------------------------------------------------------------------------------

const NAV_ICONS: Record<Route, React.ReactNode> = {
  dashboard: <LayoutDashboard size={19} aria-hidden />,
  screens: <Monitor size={19} aria-hidden />,
  audio: <Volume2 size={19} aria-hidden />,
  files: <Files size={19} aria-hidden />,
  focus: <Maximize2 size={19} aria-hidden />,
  hotkeys: <Keyboard size={19} aria-hidden />,
  settings: <SettingsIcon size={19} aria-hidden />,
  about: <Info size={19} aria-hidden />,
};

export function connStateKey(state: string): MessageKey {
  const k = `conn.${state}` as MessageKey;
  return k;
}

export function Nav() {
  const t = useT();
  const route = useStore((s) => s.route);
  const navigate = useStore((s) => s.navigate);
  const state = useStore((s) => s.state);
  const connected = useStore((s) => s.conn.connected);
  const cs = connected ? (state?.connection.state ?? 'idle') : 'idle';
  const online = cs === 'connected';

  return (
    <nav className="nav" aria-label={t('shell.nav')}>
      <ul className="nav__list">
        {ROUTES.map((r) => (
          <li key={r}>
            <button
              type="button"
              className={`nav__item${route === r ? ' is-active' : ''}`}
              aria-current={route === r ? 'page' : undefined}
              onClick={() => navigate(r)}
            >
              {NAV_ICONS[r]}
              <span className="nav__label">{t(`nav.${r}` as MessageKey)}</span>
            </button>
          </li>
        ))}
      </ul>
      <div className="nav__foot">
        <span className={`dot dot--${online ? 'good' : cs === 'degraded' ? 'warn' : 'off'}`} aria-hidden />
        <div className="nav__foot-text">
          <span className="nav__foot-name">{state?.device.name ?? PRODUCT_NAME}</span>
          <span className="nav__foot-state">{connected ? t(connStateKey(cs)) : t('agent.offlineTitle')}</span>
        </div>
      </div>
    </nav>
  );
}

// ---------------------------------------------------------------------------------------------
// Warnings + toasts
// ---------------------------------------------------------------------------------------------

export function WarningBanners() {
  const t = useT();
  const warnings = useStore((s) => s.state?.warnings ?? []);
  const dismissed = useStore((s) => s.dismissedWarnings);
  const dismiss = useStore((s) => s.dismissWarning);
  const send = useStore((s) => s.send);
  const conn = useStore((s) => s.conn);
  const hasState = useStore((s) => s.state != null);
  const [busy, setBusy] = useState(false);

  const visible = warnings.filter((w) => !dismissed.includes(w.code));
  return (
    <div className="banners">
      {!conn.connected && hasState && <Banner tone="warn" title={t('agent.lost')} />}
      {visible.map((w) => {
        const titleKey = `warn.${w.code}` as MessageKey;
        const known = titleKey in knownWarn;
        const isPublic = w.code === 'network_public';
        return (
          <Banner
            key={w.code}
            tone="warn"
            title={known ? t(titleKey) : w.message}
            onDismiss={() => dismiss(w.code)}
            dismissLabel={t('warn.dismiss')}
            action={
              isPublic ? (
                <Button
                  variant="secondary"
                  size="sm"
                  loading={busy}
                  onClick={async () => {
                    setBusy(true);
                    await send({ type: 'make_network_private' });
                    setBusy(false);
                  }}
                >
                  {t('warn.network_public.action')}
                </Button>
              ) : undefined
            }
          >
            {isPublic ? t('warn.network_public.body') : known ? w.message : undefined}
          </Banner>
        );
      })}
    </div>
  );
}

const knownWarn: Record<string, true> = {
  'warn.network_public': true,
  'warn.no_encoder': true,
  'warn.no_virtual_display': true,
  'warn.no_virtual_audio': true,
  'warn.firewall': true,
  'warn.version_mismatch': true,
  'warn.elevation': true,
  'warn.headset_missing': true,
};

export function Toaster() {
  const toasts = useStore((s) => s.toasts);
  const dismiss = useStore((s) => s.dismissToast);
  const t = useT();
  return (
    <div className="toaster" aria-live="polite" aria-atomic="false">
      <AnimatePresence initial={false}>
        {toasts.map((x) => (
          <motion.div
            key={x.id}
            className={`toast toast--${x.level}`}
            layout
            initial={{ opacity: 0, y: 12, scale: 0.98 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, y: 8 }}
            transition={{ duration: 0.2, ease: 'easeOut' }}
            role={x.level === 'error' ? 'alert' : 'status'}
          >
            {x.level === 'error' ? <AlertCircle size={18} aria-hidden /> : <CheckCircle2 size={18} aria-hidden />}
            <span className="toast__text">{x.text}</span>
            {x.action && (
              <Button variant="ghost" size="sm" onClick={x.action}>
                {x.actionLabel}
              </Button>
            )}
            <button type="button" className="icon-btn icon-btn--sm" onClick={() => dismiss(x.id)} aria-label={t('common.close')}>
              <X size={14} aria-hidden />
            </button>
          </motion.div>
        ))}
      </AnimatePresence>
    </div>
  );
}
