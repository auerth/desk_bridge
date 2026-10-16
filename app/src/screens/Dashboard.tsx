import { ExternalLink, Lock, LockOpen, Mic, MoveRight, Play, Square } from 'lucide-react';
import { useMemo } from 'react';
import type { AppState } from '../api/types';
import { chipModels, FeatureChip } from '../components/FeatureChip';
import { connStateKey } from '../components/Shell';
import { Banner, BrandIcon, Button, Card, PageHeader, Pill, Sparkline } from '../components/ui';
import { useLang, useT } from '../i18n/useT';
import { formatMs, formatSince, rttQuality, type Quality } from '../lib/format';
import { useStore } from '../store';
import { getBackend } from '../api';
import { RELEASES_URL } from '../brand';

const QUALITY_COLOR: Record<Quality | 'off', string> = {
  excellent: 'var(--c-primary)',
  good: 'var(--c-primary)',
  fair: 'var(--sw-soft-spark)',
  poor: 'var(--sw-powerline-red)',
  off: 'var(--c-border-strong)',
};

/** Two devices with a connection line between them; the line animates and is coloured by quality. */
const reduceMotion = typeof matchMedia !== 'undefined' && matchMedia('(prefers-reduced-motion: reduce)').matches;

function DevicePair({ state }: { state: AppState }) {
  const t = useT();
  const lang = useLang();
  const conn = state.connection;
  const online = conn.state === 'connected' || conn.state === 'degraded';
  const quality = online ? rttQuality(conn.rtt_us, conn.link) : 'poor';
  const color = online ? (conn.state === 'degraded' ? QUALITY_COLOR.fair : QUALITY_COLOR[quality]) : QUALITY_COLOR.off;
  const thisIsLaptop = state.device.stream_source && !state.device.input_source;
  const peer = conn.peer ?? state.peers[0] ?? null;
  const left = { icon: thisIsLaptop ? 'monitor' : 'monitor', name: thisIsLaptop ? (peer?.name ?? t('common.pc')) : state.device.name, role: t('common.pc') } as const;
  const right = { icon: 'laptop', name: thisIsLaptop ? state.device.name : (peer?.name ?? t('common.laptop')), role: t('common.laptop') } as const;
  const leftIsThis = !thisIsLaptop;
  const sub = (isThis: boolean, isPc: boolean) => (isThis ? t('common.thisDevice') : isPc ? t('dash.roleInput') : t('dash.roleStream'));

  return (
    <div className="pair" style={{ ['--link' as string]: color }}>
      <div className="pair__device">
        <div className="pair__glyph">
          <BrandIcon kind={left.icon} size={104} />
        </div>
        <p className="pair__name">{left.name}</p>
        <p className="pair__role">{sub(leftIsThis, true)}</p>
      </div>
      <div className="pair__link" aria-hidden>
        <svg viewBox="0 0 320 60" preserveAspectRatio="none" width="100%" height="60">
          <path d="M4 40 C 70 40, 90 14, 160 14 S 250 40, 316 40" className={`pair__line${online ? ' is-online' : ''}`} fill="none" strokeWidth="3" strokeLinecap="round" />
          {online && !reduceMotion && (
            <circle r="5" className="pair__spark">
              <animateMotion dur="2.2s" repeatCount="indefinite" path="M4 40 C 70 40, 90 14, 160 14 S 250 40, 316 40" />
            </circle>
          )}
          <circle cx="4" cy="40" r="5" className="pair__end" />
          <circle cx="316" cy="40" r="5" className="pair__end" />
        </svg>
        <div className="pair__badge">
          <span className={`dot dot--${online ? (conn.state === 'degraded' ? 'warn' : 'good') : 'off'}`} />
          <span>{t(connStateKey(conn.state))}</span>
          {online && conn.rtt_us != null && <span className="num">· {formatMs(conn.rtt_us, lang)} ms</span>}
        </div>
      </div>
      <div className="pair__device">
        <div className="pair__glyph">
          <BrandIcon kind={right.icon} size={104} />
        </div>
        <p className="pair__name">{right.name}</p>
        <p className="pair__role">{sub(!leftIsThis, false)}</p>
      </div>
    </div>
  );
}

function FocusButton({ state }: { state: AppState }) {
  const t = useT();
  const send = useStore((s) => s.send);
  const config = useStore((s) => s.config);
  const f = state.features.focus;
  const conn = state.connection.state === 'connected' || state.connection.state === 'degraded';
  const active = f.phase === 'streaming' || f.phase === 'preparing';
  const busy = f.phase === 'preparing' || f.phase === 'stopping';
  const mode = config?.focus.mode ?? 'full';

  let hint: string | null = null;
  if (f.state === 'unavailable') hint = f.unavailable_reason ?? t('common.unavailable');
  else if (!conn) hint = t('dash.focusNeedPeer');
  else if (!f.enabled) hint = t('dash.focusDisabled');
  const disabled = hint != null && !active;

  const modeLabel = t(mode === 'split' ? 'dash.focusModeSplit' : mode === 'window' ? 'dash.focusModeWindow' : 'dash.focusModeFull');
  const label = f.phase === 'preparing' ? t('dash.focusStarting') : f.phase === 'stopping' ? t('dash.focusStopping') : active ? t('dash.focusStop') : t('dash.focusStart');

  return (
    <div className="focus-cta">
      <Button
        variant={active ? 'secondary' : 'accent'}
        size="lg"
        className="focus-cta__btn"
        icon={active ? <Square size={18} aria-hidden /> : <Play size={18} aria-hidden />}
        disabled={disabled}
        loading={busy}
        aria-describedby={hint ? 'focus-hint' : undefined}
        onClick={() => void send({ type: 'set_focus', mode: active ? 'off' : mode })}
      >
        {label}
      </Button>
      <p className="focus-cta__meta" id={hint ? 'focus-hint' : undefined}>
        {hint ??
          (active && f.width
            ? `${modeLabel} · ${f.width} × ${f.height} @ ${f.fps} Hz · ${f.codec.toUpperCase()}`
            : modeLabel)}
      </p>
    </div>
  );
}

export function Dashboard() {
  const t = useT();
  const lang = useLang();
  const state = useStore((s) => s.state);
  const send = useStore((s) => s.send);
  const navigate = useStore((s) => s.navigate);
  const chips = useMemo(() => (state ? chipModels(state, t) : []), [state, t]);

  if (!state) return null;
  const conn = state.connection;
  const online = conn.state === 'connected' || conn.state === 'degraded';
  const hasPeer = state.peers.length > 0;
  const peerName = conn.peer?.name ?? state.peers[0]?.name ?? t('common.otherDevice');
  const remote = state.location === 'remote';
  const quality = rttQuality(conn.rtt_us, conn.link);
  const recent = state.transfers.slice(0, 3);
  const p95 = state.metrics.input_latency_p95_us;
  const p95Limit = conn.link === 'wireless' ? 15 : 3;
  const slowInput = online && p95 != null && p95 / 1000 > p95Limit;

  return (
    <>
      <PageHeader title={t('dash.title')} subtitle={t('dash.subtitle')} />

      {!hasPeer ? (
        <Card className="empty-hero">
          <div className="empty-hero__art">
            <BrandIcon kind="monitor" size={72} />
            <BrandIcon kind="link" size={40} />
            <BrandIcon kind="laptop" size={72} />
          </div>
          <h2 className="t-h2">{t('dash.noPeerTitle')}</h2>
          <p className="muted">{t('dash.noPeerBody')}</p>
          <Button variant="primary" onClick={() => navigate('settings')}>
            {t('dash.pair')}
          </Button>
        </Card>
      ) : (
        <Card className="hero">
          <DevicePair state={state} />
          {online && conn.since_unix_ms && <p className="hero__since">{t('dash.since', { time: formatSince(conn.since_unix_ms) })}</p>}
        </Card>
      )}

      {state.update.available && (
        <Banner
          tone="info"
          title={t('dash.updateAvailable', { version: state.update.available })}
          action={
            <Button size="sm" variant="primary" icon={<ExternalLink size={15} aria-hidden />} onClick={() => void getBackend().openUrl(state.update.url ?? RELEASES_URL)}>
              {t('settings.openRelease')}
            </Button>
          }
        >
          {t('dash.updateAvailableHint', { current: state.update.current })}
        </Banner>
      )}

      {slowInput && p95 != null && (
        <Banner tone="info" title={t('dash.inputLatencyHint', { n: formatMs(p95, lang), limit: p95Limit })} />
      )}

      <h2 className="t-h2 section-title" id="chips-title">
        {t('dash.chips')}
      </h2>
      <div className="chip-grid" role="group" aria-labelledby="chips-title">
        {chips.map((c) => (
          <FeatureChip key={c.feature} model={c} />
        ))}
      </div>

      {state.features.mic.state === 'live' && state.features.mic.detail && (
        <p className="mic-hint" role="status">
          <Mic size={15} aria-hidden /> {state.features.mic.detail}
        </p>
      )}

      <div className="dash-grid">
        <Card className="dash-cta">
          <FocusButton state={state} />
          <div className="dash-cta__row">
            <div>
              <p className="row__title">{t('dash.location')}</p>
              <p className="row__desc">{remote ? t('dash.locationThere', { name: peerName }) : t('dash.locationHere', { name: state.device.name })}</p>
            </div>
            <div className="btn-group">
              <Button
                size="sm"
                variant="secondary"
                disabled={!online}
                icon={<MoveRight size={16} aria-hidden />}
                onClick={() => void send({ type: 'jump_to_other_device' })}
              >
                {remote ? t('dash.jumpBack', { name: state.device.name }) : t('dash.jump', { name: peerName })}
              </Button>
              <Button
                size="sm"
                variant={state.cursor_locked ? 'primary' : 'secondary'}
                disabled={!online}
                aria-pressed={state.cursor_locked}
                icon={state.cursor_locked ? <Lock size={16} aria-hidden /> : <LockOpen size={16} aria-hidden />}
                onClick={() => void send({ type: 'set_cursor_lock', locked: !state.cursor_locked })}
              >
                {state.cursor_locked ? t('dash.unlock') : t('dash.lock')}
              </Button>
            </div>
          </div>
        </Card>

        <Card title={t('dash.quality')} className="dash-quality" actions={online ? <Pill tone={quality === 'poor' ? 'danger' : quality === 'fair' ? 'warn' : 'primary'}>{t(`quality.${quality}`)}</Pill> : undefined}>
          <div className="quality">
            <div className="quality__spark">
              <Sparkline values={state.metrics.rtt_history_us} width={260} height={56} label={t('dash.sparkline')} />
            </div>
            <dl className="stats">
              <div>
                <dt>{t('dash.rtt')}</dt>
                <dd className="num">{online ? `${formatMs(conn.rtt_us, lang)} ms` : '–'}</dd>
              </div>
              <div>
                <dt>{t('dash.link')}</dt>
                <dd>{online ? t(`link.${conn.link === 'wired' || conn.link === 'wireless' ? conn.link : 'unknown'}`) : '–'}</dd>
              </div>
              <div>
                <dt>{t('dash.audioLatency')}</dt>
                <dd className="num">{online && state.features.laptop_audio.latency_ms != null ? `${Math.round(state.features.laptop_audio.latency_ms)} ms` : '–'}</dd>
              </div>
              <div>
                <dt>{t('dash.peerVersion')}</dt>
                <dd className="num">{conn.peer_version ?? '–'}</dd>
              </div>
            </dl>
          </div>
        </Card>
      </div>

      {recent.length > 0 && (
        <Card title={t('dash.recent')} actions={<Button size="sm" variant="ghost" onClick={() => navigate('files')}>{t('nav.files')}</Button>}>
          <ul className="mini-list">
            {recent.map((tr) => (
              <li key={tr.id}>
                <span>{tr.name}</span>
                <span className="muted">{t(tr.direction === 'send' ? 'files.sent' : 'files.received')}</span>
              </li>
            ))}
          </ul>
        </Card>
      )}
    </>
  );
}
