import { Play, Square } from 'lucide-react';
import { useMemo } from 'react';
import type { AppState, Config, FocusConfig } from '../api/types';
import { Banner, Button, Card, PageHeader, Pill, Row, Segmented, Select, Slider, SwitchRow } from '../components/ui';
import { useLang, useT } from '../i18n/useT';
import { formatNumber } from '../lib/format';
import { useStore } from '../store';
import { laptopDeviceId } from './Screens';

/** Schematic 32:9 (or whatever the monitor is) preview of how laptop and PC share the monitor. */
export function FocusPreview({ w, h, focus }: { w: number; h: number; focus: FocusConfig }) {
  const t = useT();
  const mode = focus.mode;
  const pct = mode === 'split' ? focus.split_percent : 100;
  const laptopW = Math.round((w * pct) / 100);
  const label = t(mode === 'full' ? 'focus.modeFull' : mode === 'split' ? 'focus.modeSplit' : 'focus.modeWindow');
  const laptopLeft = focus.split_side === 'left';
  return (
    <figure className="preview">
      <div className="preview__monitor" style={{ aspectRatio: `${w} / ${h}` }} role="img" aria-label={t('focus.previewAlt', { mode: label })}>
        <div className={`preview__screen preview__screen--${mode}${laptopLeft ? ' is-left' : ''}`}>
          {mode === 'full' && (
            <div className="preview__laptop" style={{ flex: 1 }}>
              <span>{t('focus.previewLaptop')}</span>
              <small>{t('focus.previewNative', { w, h })}</small>
            </div>
          )}
          {mode === 'split' && (
            <>
              <div className="preview__laptop" style={{ flex: pct, order: laptopLeft ? 0 : 1 }}>
                <span>{t('focus.previewLaptop')}</span>
                <small>{t('focus.previewNative', { w: laptopW, h })}</small>
              </div>
              <div className="preview__pc" style={{ flex: 100 - pct, order: laptopLeft ? 1 : 0 }}>
                <span>{t('focus.previewPc')}</span>
                <small>{t('focus.previewNative', { w: w - laptopW, h })}</small>
              </div>
            </>
          )}
          {mode === 'window' && (
            <>
              <div className="preview__pc" style={{ flex: 1 }}>
                <span>{t('focus.previewPc')}</span>
              </div>
              <div className="preview__window">
                <div className="preview__window-bar" />
                <div className="preview__laptop preview__laptop--window">
                  <span>{t('focus.previewLaptop')}</span>
                </div>
              </div>
            </>
          )}
        </div>
      </div>
      <div className="preview__stand" aria-hidden />
      <figcaption className="preview__caption num">
        {w} × {h} · {label}
      </figcaption>
    </figure>
  );
}

function StatsGrid({ state }: { state: AppState }) {
  const t = useT();
  const lang = useLang();
  const s = state.features.focus.stats;
  if (!s) return null;
  const items: [string, string][] = [
    [t('focus.stat.fps'), `${s.fps}`],
    [t('focus.stat.bitrate'), `${formatNumber(s.bitrate_kbps / 1000, lang, 0)} Mbit/s`],
    [t('focus.stat.loss'), `${formatNumber(s.loss_pct, lang, 1)} %`],
    [t('focus.stat.encode'), `${formatNumber(s.encode_ms, lang, 1)} ms`],
    [t('focus.stat.network'), `${formatNumber(s.network_ms, lang, 1)} ms`],
    [t('focus.stat.decode'), `${formatNumber(s.decode_ms, lang, 1)} ms`],
    [t('focus.stat.total'), `${formatNumber(s.total_ms, lang, 1)} ms`],
  ];
  return (
    <dl className="stats stats--grid" aria-label={t('focus.liveStats')}>
      {items.map(([k, v]) => (
        <div key={k}>
          <dt>{k}</dt>
          <dd className="num">{v}</dd>
        </div>
      ))}
    </dl>
  );
}

export function Focus() {
  const t = useT();
  const state = useStore((s) => s.state);
  const config = useStore((s) => s.config);
  const update = useStore((s) => s.updateConfig);
  const send = useStore((s) => s.send);

  const pcScreens = useMemo(() => {
    if (!state) return [];
    const laptop = laptopDeviceId(state);
    return state.screens.screens.filter((s) => s.device_id !== laptop);
  }, [state]);

  if (!state || !config) return null;
  const f = state.features.focus;
  const fc = config.focus;
  const unavailable = f.state === 'unavailable';
  const online = state.connection.state === 'connected' || state.connection.state === 'degraded';
  const active = f.phase === 'streaming' || f.phase === 'preparing';
  const busy = f.phase === 'preparing' || f.phase === 'stopping';

  const monitor = fc.target_monitor >= 0 ? pcScreens[fc.target_monitor] : (pcScreens.find((s) => s.primary) ?? pcScreens[0]);
  const mw = monitor?.w ?? 5120;
  const mh = monitor?.h ?? 1440;

  const set = (fn: (f: FocusConfig) => void) => update((c: Config) => fn(c.focus));
  const monitorOpts = [
    { value: '-1', label: t('focus.monitorCursor') },
    ...pcScreens.map((s, i) => ({ value: String(i), label: t('focus.monitorN', { n: i + 1, name: `${s.name} (${s.w} × ${s.h})` }) })),
  ];

  return (
    <>
      <PageHeader
        title={t('focus.title')}
        subtitle={t('focus.subtitle')}
        actions={
          <Button
            variant={active ? 'secondary' : 'accent'}
            icon={active ? <Square size={16} aria-hidden /> : <Play size={16} aria-hidden />}
            disabled={(unavailable || !online || !f.enabled) && !active}
            loading={busy}
            onClick={() => void send({ type: 'set_focus', mode: active ? 'off' : fc.mode })}
          >
            {active ? t('focus.stop') : t('focus.start')}
          </Button>
        }
      />
      {unavailable && (
        <Banner tone="warn" title={t('focus.unavailableTitle')}>
          {f.unavailable_reason}
        </Banner>
      )}

      <Card title={t('focus.preview')} actions={active ? <Pill tone="accent">{t('live.live')}</Pill> : undefined}>
        <FocusPreview w={mw} h={mh} focus={fc} />
        {active && <StatsGrid state={state} />}
      </Card>

      <Card title={t('focus.mode')}>
        <div className="mode-cards" role="radiogroup" aria-label={t('focus.mode')}>
          {(['full', 'split', 'window'] as const).map((m) => (
            <button
              key={m}
              type="button"
              role="radio"
              aria-checked={fc.mode === m}
              className={`mode-card${fc.mode === m ? ' is-selected' : ''}`}
              onClick={() => set((x) => void (x.mode = m))}
            >
              <span className={`mode-card__glyph mode-card__glyph--${m}`} aria-hidden>
                <i />
                <i />
              </span>
              <span className="mode-card__title">{t(m === 'full' ? 'focus.modeFull' : m === 'split' ? 'focus.modeSplit' : 'focus.modeWindow')}</span>
              <span className="mode-card__hint">{t(m === 'full' ? 'focus.modeFullHint' : m === 'split' ? 'focus.modeSplitHint' : 'focus.modeWindowHint')}</span>
            </button>
          ))}
        </div>
        {fc.mode === 'split' && (
          <>
            <Row title={t('focus.splitSide')}>
              <Segmented
                ariaLabel={t('focus.splitSide')}
                value={fc.split_side}
                onChange={(v) => set((x) => void (x.split_side = v))}
                options={[
                  { value: 'left', label: t('focus.left') },
                  { value: 'right', label: t('focus.right') },
                ]}
              />
            </Row>
            <Row title={t('focus.splitWidth')}>
              <Segmented
                ariaLabel={t('focus.splitWidth')}
                value={String(fc.split_percent)}
                onChange={(v) => set((x) => void (x.split_percent = Number(v)))}
                options={[
                  { value: '33', label: '33 %' },
                  { value: '50', label: '50 %' },
                  { value: '67', label: '67 %' },
                ]}
              />
            </Row>
          </>
        )}
      </Card>

      <Card title={t('focus.display')}>
        <Row title={t('focus.monitor')}>
          <Select ariaLabel={t('focus.monitor')} width={320} value={String(fc.target_monitor)} options={monitorOpts} onChange={(v) => set((x) => void (x.target_monitor = Number(v)))} />
        </Row>
        <Row title={t('focus.resolution')}>
          <span className="static-value">{t('focus.resolutionAuto')}</span>
        </Row>
        <Row title={t('focus.fps')}>
          <Select
            ariaLabel={t('focus.fps')}
            width={240}
            value={String(fc.fps)}
            options={[
              { value: '0', label: t('focus.fpsAuto') },
              { value: '60', label: t('focus.fpsValue', { n: 60 }) },
              { value: '120', label: t('focus.fpsValue', { n: 120 }) },
              { value: '144', label: t('focus.fpsValue', { n: 144 }) },
            ]}
            onChange={(v) => set((x) => void (x.fps = Number(v)))}
          />
        </Row>
        <SwitchRow title={t('focus.disableInternal')} description={t('focus.disableInternalHint')} checked={fc.disable_internal_display} onChange={(v) => set((x) => void (x.disable_internal_display = v))} />
      </Card>

      <Card title={t('focus.advanced')}>
        <Row title={t('focus.codec')}>
          <Select
            ariaLabel={t('focus.codec')}
            width={240}
            value={fc.codec}
            options={[
              { value: 'auto', label: t('common.auto') },
              { value: 'hevc', label: t('focus.codecHevc') },
              { value: 'av1', label: t('focus.codecAv1') },
              { value: 'h264', label: t('focus.codecH264') },
            ]}
            onChange={(v) => set((x) => void (x.codec = v))}
          />
        </Row>
        <Row title={t('focus.quality')}>
          <Segmented
            ariaLabel={t('focus.quality')}
            value={fc.bitrate_auto ? 'auto' : 'manual'}
            onChange={(v) => set((x) => void (x.bitrate_auto = v === 'auto'))}
            options={[
              { value: 'auto', label: t('focus.qualityAuto') },
              { value: 'manual', label: t('focus.qualityManual') },
            ]}
          />
        </Row>
        {!fc.bitrate_auto && (
          <Row title={t('focus.bitrate')}>
            <Slider
              ariaLabel={t('focus.bitrate')}
              min={5}
              max={400}
              step={5}
              value={Math.round(fc.bitrate_kbps / 1000)}
              format={(v) => t('common.mbit', { n: v })}
              onChange={(v) => set((x) => void (x.bitrate_kbps = v * 1000))}
            />
          </Row>
        )}
        <SwitchRow title={t('focus.yuv444')} description={t('focus.yuv444Hint')} checked={fc.yuv444} onChange={(v) => set((x) => void (x.yuv444 = v))} />
      </Card>

      <Card title={t('focus.laptopAutomation')}>
        <SwitchRow title={t('focus.lidAuto')} description={t('focus.lidAutoHint')} checked={fc.lid_auto} onChange={(v) => set((x) => void (x.lid_auto = v))} />
        <SwitchRow title={t('focus.lidNothing')} description={t('focus.lidNothingHint')} checked={fc.lid_do_nothing} onChange={(v) => set((x) => void (x.lid_do_nothing = v))} />
      </Card>
    </>
  );
}
