import { ExternalLink, Play, Square } from 'lucide-react';
import { useEffect, useMemo } from 'react';
import { getBackend } from '../api';
import appIcon from '../brand/app-icon/variant-a.svg';
import { PRODUCT_NAME } from '../brand';
import { chipModels, FeatureChip } from '../components/FeatureChip';
import { connStateKey } from '../components/Shell';
import { Button } from '../components/ui';
import { useLang, useT } from '../i18n/useT';
import { formatMs } from '../lib/format';
import { useStore } from '../store';

/** Compact tray flyout: status, the five feature chips and the focus-mode button. */
export function Flyout() {
  const t = useT();
  const lang = useLang();
  const state = useStore((s) => s.state);
  const connected = useStore((s) => s.conn.connected);
  const config = useStore((s) => s.config);
  const send = useStore((s) => s.send);
  const backend = getBackend();
  const chips = useMemo(() => (state ? chipModels(state, t) : []), [state, t]);

  useEffect(() => {
    document.documentElement.dataset.surface = 'flyout';
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && backend.hideFlyout();
    window.addEventListener('keydown', onKey);
    const un = backend.onFlyoutOpened(() => document.getElementById('flyout-open')?.focus());
    return () => {
      window.removeEventListener('keydown', onKey);
      un();
    };
  }, [backend]);

  const conn = state?.connection;
  const online = connected && (conn?.state === 'connected' || conn?.state === 'degraded');
  const f = state?.features.focus;
  const active = f?.phase === 'streaming' || f?.phase === 'preparing';
  const peer = conn?.peer?.name;

  return (
    <div className="flyout-root">
      <div className="flyout">
        <header className="flyout__head">
          <img src={appIcon} alt="" width={28} height={28} />
          <div className="flyout__status">
            <strong>{PRODUCT_NAME}</strong>
            <span role="status">
              <span className={`dot dot--${online ? (conn?.state === 'degraded' ? 'warn' : 'good') : 'off'}`} aria-hidden />
              {!connected || !state
                ? t('flyout.noState')
                : online && peer
                  ? `${t('conn.connectedTo', { name: peer })}${conn?.rtt_us != null ? ` · ${formatMs(conn.rtt_us, lang)} ms` : ''}`
                  : t(connStateKey(conn?.state ?? 'idle'))}
            </span>
          </div>
        </header>
        <div className="flyout__chips" role="group" aria-label={t('dash.chips')}>
          {chips.map((c) => (
            <FeatureChip key={c.feature} model={c} compact />
          ))}
        </div>
        <div className="flyout__actions">
          <Button
            variant={active ? 'secondary' : 'accent'}
            icon={active ? <Square size={16} aria-hidden /> : <Play size={16} aria-hidden />}
            disabled={!online || !f?.enabled || f?.state === 'unavailable'}
            onClick={() => {
              void send({ type: 'set_focus', mode: active ? 'off' : (config?.focus.mode ?? 'full') });
              backend.hideFlyout();
            }}
          >
            {active ? t('dash.focusStop') : t('dash.focusStart')}
          </Button>
          <Button id="flyout-open" variant="secondary" icon={<ExternalLink size={16} aria-hidden />} onClick={() => backend.openMain()}>
            {t('flyout.open')}
          </Button>
        </div>
      </div>
    </div>
  );
}
