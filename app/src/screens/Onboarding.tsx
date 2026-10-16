import { AnimatePresence, motion } from 'motion/react';
import { ArrowLeft, Check, Laptop, Monitor } from 'lucide-react';
import { useEffect, useMemo, useState } from 'react';
import appIcon from '../brand/app-icon/variant-a.svg';
import logoNavy from '../brand/logos/softwelop-logo-navy.svg';
import logoWhite from '../brand/logos/softwelop-logo-weiss.svg';
import { PRODUCT_NAME } from '../brand';
import { PairedBadge, PairingPanel } from '../components/Pairing';
import { Button, Kbd, Meter, Select, TextInput } from '../components/ui';
import type { MessageKey } from '../i18n';
import { useLang, useT } from '../i18n/useT';
import { hotkeyParts } from '../lib/hotkey';
import { useStore } from '../store';
import { useEffectiveHotkeys } from './Hotkeys';
import { BoundLayoutEditor } from './Screens';

const STEPS = ['welcome', 'name', 'role', 'pair', 'layout', 'audio', 'finish'] as const;
type Step = (typeof STEPS)[number];

const variants = {
  enter: (dir: number) => ({ opacity: 0, x: dir * 36 }),
  center: { opacity: 1, x: 0 },
  exit: (dir: number) => ({ opacity: 0, x: dir * -36 }),
};

function WelcomeStep({ onNext }: { onNext: () => void }) {
  const t = useT();
  return (
    <div className="onb-welcome">
      <motion.img src={appIcon} alt="" width={112} height={112} className="onb-welcome__icon" initial={{ scale: 0.8, opacity: 0 }} animate={{ scale: 1, opacity: 1 }} transition={{ duration: 0.5 }} />
      <h1 className="t-display">{PRODUCT_NAME}</h1>
      <p className="onb-welcome__tagline">{t('onb.welcomeTagline')}</p>
      <p className="onb-welcome__body">{t('onb.welcomeBody')}</p>
      <Button variant="accent" size="lg" onClick={onNext} autoFocus>
        {t('onb.start')}
      </Button>
    </div>
  );
}

function NameStep({ name, setName }: { name: string; setName: (v: string) => void }) {
  const t = useT();
  return (
    <div className="onb-step">
      <h1 className="t-h1">{t('onb.nameTitle')}</h1>
      <p className="onb-sub">{t('onb.nameBody')}</p>
      <label className="onb-label" htmlFor="onb-name">
        {t('onb.nameLabel')}
      </label>
      <TextInput id="onb-name" value={name} onChange={setName} width="100%" autoFocus maxLength={40} />
    </div>
  );
}

function RoleStep({ isPc, setIsPc }: { isPc: boolean | null; setIsPc: (v: boolean) => void }) {
  const t = useT();
  const card = (pc: boolean) => (
    <button type="button" role="radio" aria-checked={isPc === pc} className={`role-card${isPc === pc ? ' is-selected' : ''}`} onClick={() => setIsPc(pc)}>
      <span className="role-card__icon">{pc ? <Monitor size={36} strokeWidth={1.5} aria-hidden /> : <Laptop size={36} strokeWidth={1.5} aria-hidden />}</span>
      <span className="role-card__title">{pc ? t('onb.rolePc') : t('onb.roleLaptop')}</span>
      <span className="role-card__body">{pc ? t('onb.rolePcBody') : t('onb.roleLaptopBody')}</span>
      {isPc === pc && (
        <span className="role-card__check" aria-hidden>
          <Check size={16} />
        </span>
      )}
    </button>
  );
  return (
    <div className="onb-step">
      <h1 className="t-h1">{t('onb.roleTitle')}</h1>
      <p className="onb-sub">{t('onb.roleBody')}</p>
      <div className="role-cards" role="radiogroup" aria-label={t('onb.roleTitle')}>
        {card(true)}
        {card(false)}
      </div>
    </div>
  );
}

function AudioStep() {
  const t = useT();
  const state = useStore((s) => s.state);
  const config = useStore((s) => s.config);
  const devices = useStore((s) => s.audioDevices);
  const update = useStore((s) => s.updateConfig);
  const send = useStore((s) => s.send);
  useEffect(() => {
    void send({ type: 'list_audio_devices' }, { silent: true });
  }, [send]);
  if (!state || !config) return null;
  const sink = state.device.headset_sink;
  const mic = state.features.mic;
  const opts = [{ value: '__default__', label: t('audio.systemDefault') }, ...(devices?.render ?? []).map((d) => ({ value: d.id, label: d.name }))];

  return (
    <div className="onb-step">
      <h1 className="t-h1">{t('onb.audioTitle')}</h1>
      <p className="onb-sub">{sink ? t('onb.audioBody') : t('onb.audioElsewhere')}</p>
      {sink && (
        <>
          <label className="onb-label" htmlFor="onb-headset">
            {t('onb.audioHeadset')}
          </label>
          <Select
            id="onb-headset"
            width="100%"
            value={config.audio.laptop_to_pc.output_device || '__default__'}
            options={opts}
            onChange={(v) => update((c) => void (c.audio.laptop_to_pc.output_device = v === '__default__' ? '' : v))}
          />
          <div className="onb-row">
            <Button onClick={() => void send({ type: 'play_test_tone' })}>{t('onb.audioTest')}</Button>
            <Meter level={state.features.laptop_audio.level} label={t('audio.level')} />
          </div>
          <p className="onb-label">{t('onb.audioMic')}</p>
          <Meter level={mic.level} label={t('onb.audioMic')} dim={mic.state === 'unavailable'} />
          {mic.state === 'unavailable' && <p className="onb-note">{t('onb.audioMicSoon')}</p>}
        </>
      )}
    </div>
  );
}

function FinishStep({ connected, peerName, onDone }: { connected: boolean; peerName: string | null; onDone: () => void }) {
  const t = useT();
  const lang = useLang();
  const hk = useEffectiveHotkeys();
  const items: [MessageKey, string][] = [
    ['onb.hotkeyFocus', hk.focus_cycle],
    ['onb.hotkeyJump', hk.jump_other],
    ['onb.hotkeyEmergency', hk.emergency],
  ];
  return (
    <div className="onb-finish">
      <motion.div className="onb-finish__check" initial={{ scale: 0.6, opacity: 0 }} animate={{ scale: 1, opacity: 1 }} transition={{ type: 'spring', stiffness: 260, damping: 18 }}>
        <svg viewBox="0 0 48 48" width="56" height="56" aria-hidden>
          <motion.path d="M12 25l8 8 16-18" fill="none" stroke="currentColor" strokeWidth="5" strokeLinecap="round" strokeLinejoin="round" initial={{ pathLength: 0 }} animate={{ pathLength: 1 }} transition={{ delay: 0.25, duration: 0.5 }} />
        </svg>
      </motion.div>
      <h1 className="t-display onb-finish__title">{connected ? t('onb.finishTitleConnected') : t('onb.finishTitleNotConnected')} {connected && '✓'}</h1>
      {connected && peerName && <p className="onb-finish__peer">{t('conn.connectedTo', { name: peerName })}</p>}
      <p className="onb-finish__body">{connected ? t('onb.finishBodyConnected') : t('onb.finishBodyNot')}</p>
      <ul className="cheats">
        {items.map(([label, keys]) => (
          <li key={label}>
            <span>{t(label)}</span>
            <Kbd parts={hotkeyParts(keys, lang)} />
          </li>
        ))}
      </ul>
      <Button variant="primary" size="lg" onClick={onDone} autoFocus>
        {t('onb.finishOpen')}
      </Button>
    </div>
  );
}

export function Onboarding() {
  const t = useT();
  const state = useStore((s) => s.state);
  const config = useStore((s) => s.config);
  const update = useStore((s) => s.updateConfig);
  const send = useStore((s) => s.send);
  const initialStep = useMemo(() => {
    const p = new URLSearchParams(location.search).get('step');
    const i = STEPS.indexOf(p as Step);
    return i >= 0 ? i : 0;
  }, []);
  const [idx, setIdx] = useState(initialStep);
  const [dir, setDir] = useState(1);
  const [name, setName] = useState(state?.device.name ?? '');
  const [isPc, setIsPc] = useState<boolean | null>(state ? state.device.input_source : null);
  const [pairedName, setPairedName] = useState<string | null | undefined>(undefined);

  const step = STEPS[idx] ?? 'welcome';
  const connected = state?.connection.state === 'connected' || state?.connection.state === 'degraded' || pairedName !== undefined;
  const peerName = state?.connection.peer?.name ?? pairedName ?? null;
  const total = STEPS.length - 2; // welcome and finish do not count

  const go = (d: number) => {
    // Apply name and role when leaving the role step.
    if (step === 'role' && d > 0 && isPc != null && config) {
      update((c) => {
        c.device.name = name.trim() || state?.device.name || c.device.name;
        c.device.input_source = isPc;
        c.device.headset_sink = isPc;
        c.device.stream_source = !isPc;
      });
    }
    setDir(d);
    setIdx((i) => Math.min(STEPS.length - 1, Math.max(0, i + d)));
  };

  const finish = () => {
    void send({ type: 'complete_onboarding', name: name.trim() || (state?.device.name ?? ''), is_pc: isPc ?? true });
  };

  const canNext = step === 'name' ? name.trim().length > 0 : step === 'role' ? isPc != null : true;
  const sunset = step === 'finish';

  return (
    <div className={`onb${sunset ? ' onb--sunset' : ''}`} role="dialog" aria-modal="true" aria-label={t('onb.welcomeTitle')}>
      <div className="onb__drag" data-tauri-drag-region />
      <div className="onb__top">
        {idx > 0 && step !== 'finish' ? (
          <button type="button" className="onb__back" onClick={() => go(-1)}>
            <ArrowLeft size={16} aria-hidden /> {t('common.back')}
          </button>
        ) : (
          <span />
        )}
        {step !== 'welcome' && step !== 'finish' && <span className="onb__stepno">{t('onb.step', { n: idx, total })}</span>}
      </div>

      <div className="onb__stage">
        <AnimatePresence mode="wait" custom={dir} initial={false}>
          <motion.div key={step} custom={dir} variants={variants} initial="enter" animate="center" exit="exit" transition={{ duration: 0.22, ease: 'easeOut' }} className={`onb__panel onb__panel--${step}`}>
            {step === 'welcome' && <WelcomeStep onNext={() => go(1)} />}
            {step === 'name' && <NameStep name={name} setName={setName} />}
            {step === 'role' && <RoleStep isPc={isPc} setIsPc={setIsPc} />}
            {step === 'pair' && (
              <div className="onb-step onb-step--wide">
                <h1 className="t-h1">{t('onb.pairTitle')}</h1>
                <p className="onb-sub">{t('onb.pairBody')}</p>
                {pairedName !== undefined || state?.connection.state === 'connected' ? (
                  <PairedBadge name={pairedName ?? peerName} />
                ) : (
                  <div className="onb-pair">
                    <PairingPanel suggestShow={isPc ?? true} onPaired={(n) => setPairedName(n)} />
                  </div>
                )}
              </div>
            )}
            {step === 'layout' && (
              <div className="onb-step onb-step--wide">
                <h1 className="t-h1">{t('onb.layoutTitle')}</h1>
                <p className="onb-sub">{state && state.screens.screens.length > 0 ? t('onb.layoutBody') : t('onb.layoutWaiting')}</p>
                {state && state.screens.screens.length > 0 && (
                  <div className="onb-layout">
                    <BoundLayoutEditor height={260} />
                  </div>
                )}
              </div>
            )}
            {step === 'audio' && <AudioStep />}
            {step === 'finish' && <FinishStep connected={connected} peerName={peerName} onDone={finish} />}
          </motion.div>
        </AnimatePresence>
      </div>

      <div className="onb__bottom">
        {step !== 'welcome' && step !== 'finish' ? (
          <div className="onb__nav">
            <ol className="dots" aria-label={t('onb.step', { n: idx, total })}>
              {STEPS.slice(1, -1).map((s, i) => (
                <li key={s} className={i + 1 === idx ? 'is-current' : i + 1 < idx ? 'is-done' : ''} />
              ))}
            </ol>
            <div className="btn-group">
              {step === 'pair' && !connected && (
                <Button variant="ghost" className="btn--on-dark" onClick={() => go(1)}>
                  {t('onb.skip')}
                </Button>
              )}
              <Button variant="accent" disabled={!canNext || (step === 'pair' && !connected)} onClick={() => go(1)}>
                {t('common.next')}
              </Button>
            </div>
          </div>
        ) : (
          <span />
        )}
        <div className="onb__by">
          <span>{t('onb.footer')}</span>
          <img src={sunset ? logoNavy : logoWhite} alt="softwelop" width={88} className="onb__logo" />
        </div>
      </div>
    </div>
  );
}
