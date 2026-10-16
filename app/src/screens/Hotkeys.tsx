import { useEffect, useMemo, useState } from 'react';
import { DEFAULT_HOTKEYS, HOTKEY_ACTIONS, type HotkeyAction } from '../api/types';
import { Banner, Button, Card, Kbd, PageHeader, Pill } from '../components/ui';
import type { MessageKey } from '../i18n';
import { useLang, useT } from '../i18n/useT';
import { captureHotkey, hotkeyParts, normalizeHotkey } from '../lib/hotkey';
import { useStore } from '../store';

export function useEffectiveHotkeys(): Record<HotkeyAction, string> {
  const hk = useStore((s) => s.config?.hotkeys);
  return useMemo(() => {
    const out = { ...DEFAULT_HOTKEYS };
    for (const a of HOTKEY_ACTIONS) {
      const v = hk?.[a];
      if (v) out[a] = v;
    }
    return out;
  }, [hk]);
}

export function Hotkeys() {
  const t = useT();
  const lang = useLang();
  const update = useStore((s) => s.updateConfig);
  const config = useStore((s) => s.config);
  const effective = useEffectiveHotkeys();
  const [recording, setRecording] = useState<HotkeyAction | null>(null);
  const [error, setError] = useState<{ action: HotkeyAction; text: string } | null>(null);

  // action -> other action with the same binding
  const conflicts = useMemo(() => {
    const seen = new Map<string, HotkeyAction>();
    const out = new Map<HotkeyAction, HotkeyAction>();
    for (const a of HOTKEY_ACTIONS) {
      const n = normalizeHotkey(effective[a]);
      const prev = seen.get(n);
      if (prev) {
        out.set(a, prev);
        out.set(prev, a);
      } else seen.set(n, a);
    }
    return out;
  }, [effective]);

  const setBinding = (a: HotkeyAction, value: string) => {
    update((c) => {
      if (normalizeHotkey(value) === normalizeHotkey(DEFAULT_HOTKEYS[a])) delete c.hotkeys[a];
      else c.hotkeys[a] = value;
    });
  };

  useEffect(() => {
    if (!recording) return;
    const onKey = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      if (e.key === 'Escape' && !e.ctrlKey && !e.altKey && !e.shiftKey && !e.metaKey) {
        setRecording(null);
        setError(null);
        return;
      }
      const r = captureHotkey(e);
      if (!r.ok) {
        if (r.reason === 'need-modifier') setError({ action: recording, text: t('hotkeys.needModifier') });
        else if (r.reason === 'unsupported') setError({ action: recording, text: t('hotkeys.unsupported') });
        return;
      }
      const n = normalizeHotkey(r.value);
      const clash = HOTKEY_ACTIONS.find((o) => o !== recording && normalizeHotkey(effective[o]) === n);
      if (clash) {
        setError({ action: recording, text: t('hotkeys.conflict', { action: t(`hotkey.${clash}` as MessageKey) }) });
        return;
      }
      setBinding(recording, r.value);
      setRecording(null);
      setError(null);
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [recording, effective, t]);

  if (!config) return null;
  const anyCustom = HOTKEY_ACTIONS.some((a) => config.hotkeys[a]);

  return (
    <>
      <PageHeader
        title={t('hotkeys.title')}
        subtitle={t('hotkeys.subtitle')}
        actions={
          anyCustom ? (
            <Button
              variant="ghost"
              onClick={() =>
                update((c) => {
                  for (const a of HOTKEY_ACTIONS) delete c.hotkeys[a];
                })
              }
            >
              {t('hotkeys.resetAll')}
            </Button>
          ) : undefined
        }
      />
      {conflicts.size > 0 && <Banner tone="warn" title={t('hotkeys.conflictBanner')} />}
      <Card>
        <ul className="hotkey-list">
          {HOTKEY_ACTIONS.map((a) => {
            const isRec = recording === a;
            const custom = !!config.hotkeys[a];
            const clash = conflicts.get(a);
            const err = error?.action === a ? error.text : null;
            return (
              <li key={a} className={`hotkey${isRec ? ' is-recording' : ''}`}>
                <div className="hotkey__text">
                  <p className="row__title">{t(`hotkey.${a}` as MessageKey)}</p>
                  <p className="row__desc">{t(`hotkey.${a}.desc` as MessageKey)}</p>
                  {(err || clash) && (
                    <p className="hotkey__error" role="alert">
                      {err ?? t('hotkeys.conflict', { action: t(`hotkey.${clash}` as MessageKey) })}
                    </p>
                  )}
                </div>
                <div className="hotkey__keys">
                  {isRec ? (
                    <span className="hotkey__rec" role="status" aria-live="polite">
                      <span className="rec-dot" aria-hidden />
                      {t('hotkeys.recording')} <small>{t('hotkeys.recordingHint')}</small>
                    </span>
                  ) : (
                    <>
                      <Kbd parts={hotkeyParts(effective[a], lang)} />
                      {custom && <Pill tone="primary">{t('hotkeys.custom')}</Pill>}
                    </>
                  )}
                </div>
                <div className="hotkey__actions">
                  <Button
                    size="sm"
                    variant={isRec ? 'primary' : 'secondary'}
                    aria-label={`${t('hotkeys.change')}: ${t(`hotkey.${a}` as MessageKey)}`}
                    onClick={() => {
                      setError(null);
                      setRecording(isRec ? null : a);
                    }}
                  >
                    {isRec ? t('common.cancel') : t('hotkeys.change')}
                  </Button>
                  {custom && !isRec && (
                    <Button size="sm" variant="ghost" onClick={() => update((c) => void delete c.hotkeys[a])}>
                      {t('hotkeys.reset')}
                    </Button>
                  )}
                </div>
              </li>
            );
          })}
        </ul>
      </Card>
      <p className="hint">{t('hotkeys.note')}</p>
    </>
  );
}
