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
import { hasKey, type MessageKey } from '../i18n';
import { useLang, useT } from '../i18n/useT';
import { ROUTES, useStore, type Route } from '../store';
import { compareVersions, formatBytes, installerVersion } from '../lib/format';
import { Banner, Button, Modal, Pill } from './ui';

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
  const pushToast = useStore((s) => s.pushToast);
  const navigate = useStore((s) => s.navigate);
  const conn = useStore((s) => s.conn);
  const state = useStore((s) => s.state);
  const [busy, setBusy] = useState(false);

  const own = state?.device.version ?? '';
  const peerVersion = state?.connection.peer_version ?? null;
  const mixed = !!state && (state.connection.version_mismatch || (peerVersion != null && own !== '' && peerVersion !== own));
  const visible = warnings.filter((w) => !dismissed.includes(w.code) && !(mixed && w.code === 'version_mismatch'));
  const pu = usePartnerUpdate();
  return (
    <div className="banners">
      {!conn.connected && state && <Banner tone="warn" title={t('agent.lost')} />}
      {state?.connection.peer_logon_screen && (
        <Banner tone="info" title={t('logon.title', { name: state.connection.peer?.name ?? '' })}>
          {t('logon.body')}
        </Banner>
      )}
      {mixed && !dismissed.includes('versions_mixed') && (
        <Banner
          tone="info"
          title={t('update.mismatchTitle')}
          onDismiss={() => dismiss('versions_mixed')}
          dismissLabel={t('warn.dismiss')}
          action={
            pu.direction ? (
              <Button variant="primary" size="sm" loading={pu.busy} onClick={() => void pu.run()}>
                {pu.direction === 'push' ? t('pu.pushButton', { name: pu.peerName }) : t('pu.pullButton', { version: pu.peer ?? '' })}
              </Button>
            ) : (
              <Button variant="secondary" size="sm" onClick={() => navigate('settings')}>
                {t('nav.settings')}
              </Button>
            )
          }
        >
          {t('update.mismatchBody', { own, peer: peerVersion ?? '?' })}
        </Banner>
      )}
      {visible.map((w) => {
        const titleKey = `warn.${w.code}`;
        const bodyKey = `warn.${w.code}.body`;
        const isPublic = w.code === 'network_public';
        const body = isPublic ? t('warn.network_public.body') : w.message || (hasKey(bodyKey) ? t(bodyKey) : undefined);
        return (
          <Banner
            key={w.code}
            tone="warn"
            title={hasKey(titleKey) ? t(titleKey) : w.message || w.code}
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
              ) : w.code === 'elevation' ? (
                <Button
                  variant="primary"
                  size="sm"
                  loading={busy}
                  onClick={async () => {
                    setBusy(true);
                    const r = await send({ type: 'restart_agent', elevated: true });
                    if (r !== undefined) pushToast({ level: 'info', text: t('warn.elevation.restarting') });
                    setBusy(false);
                  }}
                >
                  {t('warn.elevation.action')}
                </Button>
              ) : undefined
            }
          >
            {body}
          </Banner>
        );
      })}
    </div>
  );
}

/**
 * Different versions on the two devices: the newer one pushes its installer ("push", installed
 * there right away), the older one can fetch it ("pull", needs a partner that supports that).
 */
export function usePartnerUpdate() {
  const t = useT();
  const state = useStore((s) => s.state);
  const send = useStore((s) => s.send);
  const toast = useStore((s) => s.pushToast);
  const own = state?.device.version ?? '';
  const peer = state?.connection.peer_version ?? null;
  const peerName = state?.connection.peer?.name ?? '';
  const upd = state?.update;
  const connected = state?.connection.state === 'connected' || state?.connection.state === 'degraded';
  const cmp = connected && peer && own ? compareVersions(own, peer) : null;
  const direction: 'push' | 'pull' | null = cmp == null || cmp === 0 ? null : cmp > 0 ? 'push' : upd?.partner_remote ? 'pull' : null;
  const busy = direction === 'push' ? upd?.partner_updating != null : direction === 'pull' ? !!upd?.updating_from_partner : false;

  const run = async () => {
    if (direction === 'pull') {
      await send({ type: 'request_partner_update' });
      return;
    }
    if (direction !== 'push') return;
    let r: { confirm_on_partner?: boolean } | undefined;
    if (upd?.installer_available) {
      r = await send({ type: 'update_partner' });
    } else {
      // No installer kept on this device (dev build, installed before 0.1.3): pick it.
      const path = await getBackend().pickInstaller();
      if (!path) return;
      const version = installerVersion(path);
      if (!version) {
        toast({ level: 'error', text: t('settings.sendInstallerBadName') });
        return;
      }
      r = await send({ type: 'offer_partner_update', path, version, install: true });
    }
    if (r?.confirm_on_partner) toast({ level: 'info', text: t('pu.confirmOnPartner', { name: peerName }) });
  };

  return { direction, own, peer, peerName, busy, run, installerAvailable: !!upd?.installer_available, partnerRemote: !!upd?.partner_remote };
}

/** Partner update flow: offer (accept/decline) and "received, start now?" - never silent. */
export function PartnerUpdateDialogs() {
  const t = useT();
  const lang = useLang();
  const offer = useStore((s) => s.partnerOffer);
  const ready = useStore((s) => s.partnerReady);
  const clearOffer = useStore((s) => s.clearPartnerOffer);
  const clearReady = useStore((s) => s.clearPartnerReady);
  const send = useStore((s) => s.send);
  const toast = useStore((s) => s.pushToast);
  const [starting, setStarting] = useState(false);

  const answer = (accept: boolean) => {
    void send({ type: 'answer_partner_update', accept });
    clearOffer();
  };

  return (
    <>
      <Modal
        open={offer != null}
        onOpenChange={(o) => {
          if (!o && offer) answer(false);
        }}
        title={t('pu.offerTitle')}
        description={offer ? t('pu.offerBody', { from: offer.from, version: offer.version, size: formatBytes(offer.size, lang) }) : undefined}
        footer={
          <>
            <Button variant="ghost" onClick={() => answer(false)}>
              {t('pu.decline')}
            </Button>
            <Button variant="primary" onClick={() => answer(true)}>
              {t('pu.accept')}
            </Button>
          </>
        }
      />
      <Modal
        open={ready != null}
        onOpenChange={(o) => !o && clearReady()}
        title={t('pu.readyTitle')}
        description={ready ? t('pu.readyBody', { version: ready.version }) : undefined}
        footer={
          <>
            <Button variant="ghost" onClick={clearReady}>
              {t('pu.later')}
            </Button>
            <Button
              variant="accent"
              loading={starting}
              onClick={async () => {
                if (!ready) return;
                setStarting(true);
                try {
                  await getBackend().launchInstaller(ready.path);
                  clearReady();
                } catch (e) {
                  toast({ level: 'error', text: t('pu.launchFailed', { error: String(e instanceof Error ? e.message : e) }) });
                } finally {
                  setStarting(false);
                }
              }}
            >
              {t('pu.start')}
            </Button>
          </>
        }
      />
    </>
  );
}

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
