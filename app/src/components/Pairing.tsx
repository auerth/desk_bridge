import { CheckCircle2, Laptop, Loader2, Monitor, RefreshCw, Wifi } from 'lucide-react';
import { useEffect, useMemo, useRef, useState } from 'react';
import type { DiscoveredDto } from '../api/types';
import { useT } from '../i18n/useT';
import { formatClock } from '../lib/format';
import { useStore } from '../store';
import { Button, Pill, TextInput } from './ui';

/** Six single-digit boxes with paste support. */
export function CodeInput({
  value,
  onChange,
  onComplete,
  disabled,
  label,
  invalid,
}: {
  value: string;
  onChange: (v: string) => void;
  onComplete?: (v: string) => void;
  disabled?: boolean;
  label: string;
  invalid?: boolean;
}) {
  const t = useT();
  const refs = useRef<(HTMLInputElement | null)[]>([]);
  const digits = value.padEnd(6, ' ').slice(0, 6).split('');

  const setAt = (i: number, ch: string) => {
    const arr = value.padEnd(6, ' ').split('');
    arr[i] = ch;
    const next = arr.join('').replace(/ +$/, '');
    onChange(next);
    if (next.length === 6 && !next.includes(' ')) onComplete?.(next);
  };

  return (
    <div className={`code-input${invalid ? ' code-input--invalid' : ''}`} role="group" aria-label={label}>
      {digits.map((d, i) => (
        <input
          key={i}
          ref={(el) => {
            refs.current[i] = el;
          }}
          className="code-input__box"
          inputMode="numeric"
          autoComplete="one-time-code"
          maxLength={1}
          value={d.trim()}
          disabled={disabled}
          aria-label={t('pair.codeDigit', { n: i + 1 })}
          onFocus={(e) => e.target.select()}
          onChange={(e) => {
            const ch = e.target.value.replace(/\D/g, '').slice(-1);
            if (!ch) return;
            setAt(i, ch);
            refs.current[Math.min(i + 1, 5)]?.focus();
          }}
          onKeyDown={(e) => {
            if (e.key === 'Backspace') {
              e.preventDefault();
              if (d.trim()) setAt(i, ' ');
              else if (i > 0) {
                setAt(i - 1, ' ');
                refs.current[i - 1]?.focus();
              }
            } else if (e.key === 'ArrowLeft') refs.current[Math.max(0, i - 1)]?.focus();
            else if (e.key === 'ArrowRight') refs.current[Math.min(5, i + 1)]?.focus();
          }}
          onPaste={(e) => {
            const text = e.clipboardData.getData('text').replace(/\D/g, '').slice(0, 6);
            if (!text) return;
            e.preventDefault();
            onChange(text);
            refs.current[Math.min(text.length, 5)]?.focus();
            if (text.length === 6) onComplete?.(text);
          }}
        />
      ))}
    </div>
  );
}

/** Big pairing code with a countdown ring. */
function CodeDisplay({ code, secondsLeft }: { code: string; secondsLeft: number }) {
  const t = useT();
  const pct = Math.max(0, Math.min(1, secondsLeft / 120));
  const r = 20;
  const c = 2 * Math.PI * r;
  const groups = [code.slice(0, 3), code.slice(3, 6)];
  return (
    <div className="code-display">
      <div className="code-display__digits" aria-label={code.split('').join(' ')} role="img">
        {groups.map((g, gi) => (
          <span key={gi} className="code-display__group" aria-hidden>
            {g.split('').map((d, i) => (
              <span key={i} className="code-display__digit">
                {d}
              </span>
            ))}
          </span>
        ))}
      </div>
      <div className="code-display__timer">
        <svg width="48" height="48" viewBox="0 0 48 48" aria-hidden>
          <circle cx="24" cy="24" r={r} className="ring-track" fill="none" strokeWidth="4" />
          <circle
            cx="24"
            cy="24"
            r={r}
            className="ring-fill"
            fill="none"
            strokeWidth="4"
            strokeLinecap="round"
            strokeDasharray={c}
            strokeDashoffset={c * (1 - pct)}
            transform="rotate(-90 24 24)"
          />
        </svg>
        <span>{t('pair.codeValid', { time: formatClock(secondsLeft) })}</span>
      </div>
    </div>
  );
}

export function PairingPanel({
  initialMode,
  suggestShow,
  onPaired,
}: {
  initialMode?: 'show' | 'enter';
  /** Which device should show the code by default (the PC). */
  suggestShow: boolean;
  onPaired: (peerName: string | null) => void;
}) {
  const t = useT();
  const [mode, setMode] = useState<'show' | 'enter'>(initialMode ?? (suggestShow ? 'show' : 'enter'));
  const startedAt = useRef(Date.now());
  const result = useStore((s) => s.pairingResult);
  const state = useStore((s) => s.state);
  const send = useStore((s) => s.send);
  const [code, setCode] = useState('');
  const [selected, setSelected] = useState<DiscoveredDto | null>(null);
  const [busy, setBusy] = useState(false);
  const [manual, setManual] = useState('');
  const [showManual, setShowManual] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const donePaired = useRef(false);

  const pairing = state?.pairing ?? null;
  const addresses = state?.device.addresses ?? [];
  const discovered = useMemo(() => state?.discovered ?? [], [state?.discovered]);

  // "Show code" side: ask the agent for a code while this panel is visible.
  useEffect(() => {
    if (mode !== 'show') return;
    void send({ type: 'start_pairing' });
    return () => {
      void send({ type: 'cancel_pairing' }, { silent: true });
    };
  }, [mode, send]);

  // Result events from the agent (both sides).
  useEffect(() => {
    if (!result || result.at < startedAt.current - 50) return;
    setBusy(false);
    if (result.ok) {
      if (!donePaired.current) {
        donePaired.current = true;
        onPaired(result.peerName);
      }
    } else {
      setError(result.message || t('pair.failed'));
      setCode('');
    }
  }, [result, onPaired, t]);

  const submit = async (c: string) => {
    if (!selected && !manual.trim()) return;
    setError(null);
    setBusy(true);
    await send({ type: 'pair_with', device_id: selected?.id ?? null, address: selected ? null : manual.trim(), code: c });
    // The outcome arrives as a pairing_result event; fall back after a while.
    setTimeout(() => setBusy(false), 8000);
  };

  return (
    <div className="pairing">
      <div className="pairing__tabs" role="tablist" aria-label={t('pair.title')}>
        <button type="button" role="tab" aria-selected={mode === 'show'} className={mode === 'show' ? 'is-active' : ''} onClick={() => setMode('show')}>
          {t('pair.showCode')}
        </button>
        <button type="button" role="tab" aria-selected={mode === 'enter'} className={mode === 'enter' ? 'is-active' : ''} onClick={() => setMode('enter')}>
          {t('pair.enterCode')}
        </button>
      </div>

      {mode === 'show' ? (
        <div className="pairing__show" role="tabpanel">
          <p className="pairing__intro">{t('pair.codeIntro')}</p>
          {pairing ? (
            <CodeDisplay code={pairing.code} secondsLeft={pairing.seconds_left} />
          ) : (
            <div className="code-display code-display--empty">
              <Loader2 className="spin" size={28} aria-hidden />
            </div>
          )}
          <p className="pairing__wait" role="status">
            <Loader2 className="spin" size={14} aria-hidden /> {t('pair.waiting')}
          </p>
          {addresses.length > 0 && (
            <p className="pairing__addresses">
              {t('pair.myAddress')}: <strong>{addresses.join('  ·  ')}</strong>
            </p>
          )}
          <Button
            variant="ghost"
            size="sm"
            icon={<RefreshCw size={14} aria-hidden />}
            onClick={async () => {
              await send({ type: 'cancel_pairing' }, { silent: true });
              await send({ type: 'start_pairing' });
            }}
          >
            {t('pair.newCode')}
          </Button>
        </div>
      ) : (
        <div className="pairing__enter" role="tabpanel">
          <p className="pairing__intro">{t('pair.otherSideHint')}</p>
          <h3 className="t-h3">{t('pair.found')}</h3>
          <ul className="device-list" aria-label={t('pair.found')}>
            {discovered.map((d) => (
              <li key={d.id}>
                <button
                  type="button"
                  className={`device-card${selected?.id === d.id ? ' is-selected' : ''}`}
                  aria-pressed={selected?.id === d.id}
                  onClick={() => {
                    setSelected(d);
                    setError(null);
                  }}
                >
                  <span className="device-card__icon">{/laptop/i.test(d.name) ? <Laptop size={20} aria-hidden /> : <Monitor size={20} aria-hidden />}</span>
                  <span className="device-card__text">
                    <span className="device-card__name">{d.name}</span>
                    <span className="device-card__sub">
                      {d.address} · v{d.version}
                    </span>
                  </span>
                  {d.paired && <Pill tone="primary">{t('pair.paired')}</Pill>}
                </button>
              </li>
            ))}
          </ul>
          {discovered.length === 0 && (
            <p className="pairing__none" role="status">
              <Wifi size={16} aria-hidden /> {t('pair.searching')} {t('pair.noneFound')}
            </p>
          )}
          <button type="button" className="link-btn" onClick={() => setShowManual((v) => !v)} aria-expanded={showManual}>
            {t('pair.manual')}
          </button>
          {showManual && (
            <div className="pairing__manual">
              <TextInput value={manual} onChange={(v) => { setManual(v); setSelected(null); }} placeholder={t('pair.manualPlaceholder')} ariaLabel={t('pair.manual')} width={260} />
            </div>
          )}
          {(selected || manual.trim()) && (
            <div className="pairing__code">
              <p className="row__title">{selected ? t('pair.codeLabelFor', { name: selected.name }) : t('pair.codeLabel')}</p>
              <CodeInput value={code} onChange={setCode} onComplete={(c) => void submit(c)} disabled={busy} label={t('pair.codeLabel')} invalid={!!error} />
              {error && (
                <p className="pairing__error" role="alert">
                  {error}
                </p>
              )}
              <Button variant="primary" loading={busy} disabled={code.length !== 6} onClick={() => void submit(code)}>
                {busy ? t('pair.pairing') : t('pair.connect')}
              </Button>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

/** Small "paired" confirmation used after success. */
export function PairedBadge({ name }: { name: string | null }) {
  const t = useT();
  return (
    <p className="paired-ok" role="status">
      <CheckCircle2 size={20} aria-hidden /> {t('pair.success', { name: name ?? '' })}
    </p>
  );
}
