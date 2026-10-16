import { Clipboard, Maximize2, Mic, MousePointer2, Volume2 } from 'lucide-react';
import type { ReactNode } from 'react';
import type { AppState, Feature, Live } from '../api/types';
import type { MessageKey } from '../i18n';
import { useT } from '../i18n/useT';
import { useStore } from '../store';
import { Tip } from './ui';

export interface ChipModel {
  feature: Extract<Feature, 'input' | 'laptop_audio' | 'mic' | 'clipboard' | 'focus'>;
  label: MessageKey;
  tip: MessageKey;
  icon: ReactNode;
  enabled: boolean;
  state: Live;
  /** Extra line, e.g. "Genutzt von Teams" or the reason it is unavailable. */
  detail: string | null;
}

const ICON = { size: 18, 'aria-hidden': true } as const;

/** The five dashboard chips, derived from the current state. */
export function chipModels(state: AppState, t: ReturnType<typeof useT>): ChipModel[] {
  const f = state.features;
  const micDetail =
    f.mic.state === 'unavailable'
      ? t('chip.mic.unavailable')
      : f.mic.state === 'live' && f.mic.detail
        ? f.mic.detail
        : null;
  return [
    { feature: 'input', label: 'chip.input', tip: 'chip.input.tip', icon: <MousePointer2 {...ICON} />, enabled: f.input.enabled, state: f.input.state, detail: f.input.detail },
    { feature: 'laptop_audio', label: 'chip.laptopAudio', tip: 'chip.laptopAudio.tip', icon: <Volume2 {...ICON} />, enabled: f.laptop_audio.enabled, state: f.laptop_audio.state, detail: f.laptop_audio.device_missing ? t('warn.headset_missing') : f.laptop_audio.detail },
    { feature: 'mic', label: 'chip.mic', tip: 'chip.mic.tip', icon: <Mic {...ICON} />, enabled: f.mic.enabled, state: f.mic.state, detail: micDetail },
    { feature: 'clipboard', label: 'chip.clipboard', tip: 'chip.clipboard.tip', icon: <Clipboard {...ICON} />, enabled: f.clipboard.enabled, state: f.clipboard.state, detail: f.clipboard.detail },
    { feature: 'focus', label: 'chip.focus', tip: 'chip.focus.tip', icon: <Maximize2 {...ICON} />, enabled: f.focus.enabled, state: f.focus.state, detail: f.focus.state === 'unavailable' ? f.focus.unavailable_reason : f.focus.detail },
  ];
}

/** Toggleable status chip: shows off / ready / live / unavailable and flips the feature on click. */
export function FeatureChip({ model, compact }: { model: ChipModel; compact?: boolean }) {
  const t = useT();
  const send = useStore((s) => s.send);
  const unavailable = model.state === 'unavailable';
  const name = t(model.label);
  const stateText = t(`live.${model.state}` as MessageKey);
  const tip = (
    <span className="chip-tip">
      <strong>{name}</strong>
      <span>{t(model.tip)}</span>
      {model.detail && <em>{model.detail}</em>}
    </span>
  );

  return (
    <Tip content={tip} side="bottom">
      <button
        type="button"
        role="switch"
        aria-checked={model.enabled && !unavailable}
        aria-disabled={unavailable || undefined}
        aria-label={`${name}: ${stateText}${model.detail ? `, ${model.detail}` : ''}`}
        className={`fchip fchip--${model.state}${compact ? ' fchip--compact' : ''}`}
        onClick={() => {
          if (unavailable) return;
          void send({ type: 'set_feature', feature: model.feature, enabled: !model.enabled });
        }}
      >
        <span className="fchip__icon">{model.icon}</span>
        <span className="fchip__text">
          <span className="fchip__label">{name}</span>
          <span className="fchip__state">
            <span className="fchip__dot" aria-hidden />
            {stateText}
            {!compact && model.detail && model.state !== 'off' && <span className="fchip__detail"> · {model.detail}</span>}
          </span>
        </span>
        <span className="fchip__toggle" aria-hidden>
          <span />
        </span>
      </button>
    </Tip>
  );
}
