import * as DialogPrimitive from '@radix-ui/react-dialog';
import * as SelectPrimitive from '@radix-ui/react-select';
import * as SliderPrimitive from '@radix-ui/react-slider';
import * as SwitchPrimitive from '@radix-ui/react-switch';
import * as ToggleGroup from '@radix-ui/react-toggle-group';
import * as TooltipPrimitive from '@radix-ui/react-tooltip';
import { AlertTriangle, Check, ChevronDown, Info, Loader2, X } from 'lucide-react';
import { useId, useState, type ReactNode } from 'react';
import { useT } from '../i18n/useT';
import laptopIcon from '../brand/icons/icon-01-laptop-code.svg';
import linkIcon from '../brand/icons/icon-22-link.svg';
import monitorIcon from '../brand/icons/icon-02-monitor-code.svg';

// ---------------------------------------------------------------------------------------------
// Buttons
// ---------------------------------------------------------------------------------------------

export interface ButtonProps extends Omit<React.ButtonHTMLAttributes<HTMLButtonElement>, 'className'> {
  variant?: 'primary' | 'secondary' | 'ghost' | 'accent' | 'danger';
  size?: 'sm' | 'md' | 'lg';
  icon?: ReactNode;
  loading?: boolean;
  className?: string;
}

export function Button({ variant = 'secondary', size = 'md', icon, loading, children, className, disabled, ...rest }: ButtonProps) {
  return (
    <button
      type="button"
      className={`btn btn--${variant} btn--${size}${className ? ` ${className}` : ''}`}
      disabled={disabled || loading}
      aria-busy={loading || undefined}
      {...rest}
    >
      {loading ? <Loader2 className="spin" size={16} aria-hidden /> : icon}
      {children != null && <span>{children}</span>}
    </button>
  );
}

export function IconButton({
  label,
  children,
  className,
  ...rest
}: Omit<React.ButtonHTMLAttributes<HTMLButtonElement>, 'aria-label'> & { label: string }) {
  return (
    <Tip content={label}>
      <button type="button" className={`icon-btn${className ? ` ${className}` : ''}`} aria-label={label} {...rest}>
        {children}
      </button>
    </Tip>
  );
}

// ---------------------------------------------------------------------------------------------
// Layout helpers
// ---------------------------------------------------------------------------------------------

export function PageHeader({ title, subtitle, actions }: { title: string; subtitle?: string; actions?: ReactNode }) {
  return (
    <header className="page-head">
      <div>
        <h1 className="t-h1" id="page-title" tabIndex={-1}>
          {title}
        </h1>
        {subtitle && <p className="page-head__sub">{subtitle}</p>}
      </div>
      {actions && <div className="page-head__actions">{actions}</div>}
    </header>
  );
}

export function Card({
  title,
  subtitle,
  icon,
  actions,
  children,
  className,
  tone,
}: {
  title?: ReactNode;
  subtitle?: ReactNode;
  icon?: ReactNode;
  actions?: ReactNode;
  children?: ReactNode;
  className?: string;
  tone?: 'plain' | 'muted';
}) {
  return (
    <section className={`card${tone === 'muted' ? ' card--muted' : ''}${className ? ` ${className}` : ''}`}>
      {(title || actions) && (
        <header className="card__head">
          {icon && <span className="card__icon">{icon}</span>}
          <div className="card__titles">
            {title && <h2 className="t-h2 card__title">{title}</h2>}
            {subtitle && <p className="card__sub">{subtitle}</p>}
          </div>
          {actions && <div className="card__actions">{actions}</div>}
        </header>
      )}
      {children}
    </section>
  );
}

/** One setting: text on the left, control on the right (stacks on narrow widths). */
export function Row({
  title,
  description,
  children,
  disabled,
  htmlFor,
  stack,
}: {
  title: ReactNode;
  description?: ReactNode;
  children?: ReactNode;
  disabled?: boolean;
  htmlFor?: string;
  stack?: boolean;
}) {
  return (
    <div className={`row${disabled ? ' row--disabled' : ''}${stack ? ' row--stack' : ''}`}>
      <div className="row__text">
        <label className="row__title" htmlFor={htmlFor}>
          {title}
        </label>
        {description && <p className="row__desc">{description}</p>}
      </div>
      {children != null && <div className="row__control">{children}</div>}
    </div>
  );
}

export function Divider() {
  return <hr className="divider" />;
}

// ---------------------------------------------------------------------------------------------
// Form controls
// ---------------------------------------------------------------------------------------------

export function Switch({
  checked,
  onChange,
  label,
  disabled,
  id,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label?: string;
  disabled?: boolean;
  id?: string;
}) {
  return (
    <SwitchPrimitive.Root id={id} className="switch" checked={checked} onCheckedChange={onChange} disabled={disabled} aria-label={label}>
      <SwitchPrimitive.Thumb className="switch__thumb" />
    </SwitchPrimitive.Root>
  );
}

/** A setting row with a switch; the whole text is the label. */
export function SwitchRow({
  title,
  description,
  checked,
  onChange,
  disabled,
}: {
  title: ReactNode;
  description?: ReactNode;
  checked: boolean;
  onChange: (v: boolean) => void;
  disabled?: boolean;
}) {
  const id = useId();
  return (
    <Row title={title} description={description} htmlFor={id} disabled={disabled}>
      <Switch id={id} checked={checked} onChange={onChange} disabled={disabled} />
    </Row>
  );
}

export interface Option<V extends string> {
  value: V;
  label: ReactNode;
  hint?: string;
  disabled?: boolean;
}

export function Select<V extends string>({
  value,
  onChange,
  options,
  ariaLabel,
  id,
  disabled,
  placeholder,
  width,
}: {
  value: V;
  onChange: (v: V) => void;
  options: Option<V>[];
  ariaLabel?: string;
  id?: string;
  disabled?: boolean;
  placeholder?: string;
  width?: number | string;
}) {
  return (
    <SelectPrimitive.Root value={value} onValueChange={(v) => onChange(v as V)} disabled={disabled}>
      <SelectPrimitive.Trigger id={id} className="select" aria-label={ariaLabel} style={width ? { width } : undefined}>
        <span className="select__value">
          <SelectPrimitive.Value placeholder={placeholder} />
        </span>
        <SelectPrimitive.Icon asChild>
          <ChevronDown size={16} aria-hidden />
        </SelectPrimitive.Icon>
      </SelectPrimitive.Trigger>
      <SelectPrimitive.Portal>
        <SelectPrimitive.Content className="select__content" position="popper" sideOffset={6} collisionPadding={12}>
          <SelectPrimitive.Viewport className="select__viewport">
            {options.map((o) => (
              <SelectPrimitive.Item key={o.value} value={o.value} disabled={o.disabled} className="select__item">
                <SelectPrimitive.ItemText>{o.label}</SelectPrimitive.ItemText>
                {o.hint && <span className="select__hint">{o.hint}</span>}
                <SelectPrimitive.ItemIndicator className="select__check">
                  <Check size={14} aria-hidden />
                </SelectPrimitive.ItemIndicator>
              </SelectPrimitive.Item>
            ))}
          </SelectPrimitive.Viewport>
        </SelectPrimitive.Content>
      </SelectPrimitive.Portal>
    </SelectPrimitive.Root>
  );
}

export function Slider({
  value,
  min,
  max,
  step = 1,
  onChange,
  ariaLabel,
  format,
  disabled,
  width = 220,
}: {
  value: number;
  min: number;
  max: number;
  step?: number;
  onChange: (v: number) => void;
  ariaLabel: string;
  format?: (v: number) => string;
  disabled?: boolean;
  width?: number | string;
}) {
  return (
    <div className="slider-wrap" style={{ width }}>
      <SliderPrimitive.Root
        className="slider"
        value={[value]}
        min={min}
        max={max}
        step={step}
        disabled={disabled}
        onValueChange={(v) => onChange(v[0] ?? value)}
      >
        <SliderPrimitive.Track className="slider__track">
          <SliderPrimitive.Range className="slider__range" />
        </SliderPrimitive.Track>
        <SliderPrimitive.Thumb className="slider__thumb" aria-label={ariaLabel} />
      </SliderPrimitive.Root>
      <output className="slider__value">{format ? format(value) : value}</output>
    </div>
  );
}

export function Segmented<V extends string>({
  value,
  onChange,
  options,
  ariaLabel,
  disabled,
  size = 'md',
}: {
  value: V;
  onChange: (v: V) => void;
  options: Option<V>[];
  ariaLabel: string;
  disabled?: boolean;
  size?: 'sm' | 'md';
}) {
  return (
    <ToggleGroup.Root
      type="single"
      className={`segmented segmented--${size}`}
      value={value}
      onValueChange={(v) => v && onChange(v as V)}
      aria-label={ariaLabel}
      disabled={disabled}
    >
      {options.map((o) => (
        <ToggleGroup.Item key={o.value} value={o.value} className="segmented__item" disabled={o.disabled}>
          {o.label}
        </ToggleGroup.Item>
      ))}
    </ToggleGroup.Root>
  );
}

export function TextInput({
  value,
  onChange,
  placeholder,
  ariaLabel,
  id,
  disabled,
  onEnter,
  onBlur,
  width,
  inputMode,
  maxLength,
  autoFocus,
  invalid,
}: {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  ariaLabel?: string;
  id?: string;
  disabled?: boolean;
  onEnter?: () => void;
  onBlur?: () => void;
  width?: number | string;
  inputMode?: 'text' | 'numeric';
  maxLength?: number;
  autoFocus?: boolean;
  invalid?: boolean;
}) {
  return (
    <input
      id={id}
      className={`input${invalid ? ' input--invalid' : ''}`}
      value={value}
      placeholder={placeholder}
      aria-label={ariaLabel}
      aria-invalid={invalid || undefined}
      disabled={disabled}
      inputMode={inputMode}
      maxLength={maxLength}
      autoFocus={autoFocus}
      spellCheck={false}
      onBlur={onBlur}
      style={width ? { width } : undefined}
      onChange={(e) => onChange(e.target.value)}
      onKeyDown={(e) => {
        if (e.key === 'Enter') onEnter?.();
      }}
    />
  );
}

// ---------------------------------------------------------------------------------------------
// Feedback
// ---------------------------------------------------------------------------------------------

export function Tip({ content, children, side = 'top' }: { content: ReactNode; children: ReactNode; side?: 'top' | 'bottom' | 'left' | 'right' }) {
  return (
    <TooltipPrimitive.Root delayDuration={350}>
      <TooltipPrimitive.Trigger asChild>{children}</TooltipPrimitive.Trigger>
      <TooltipPrimitive.Portal>
        <TooltipPrimitive.Content className="tooltip" side={side} sideOffset={8} collisionPadding={12}>
          {content}
          <TooltipPrimitive.Arrow className="tooltip__arrow" />
        </TooltipPrimitive.Content>
      </TooltipPrimitive.Portal>
    </TooltipPrimitive.Root>
  );
}

export function InfoTip({ text }: { text: string }) {
  return (
    <Tip content={text}>
      <button type="button" className="info-tip" aria-label={text}>
        <Info size={15} aria-hidden />
      </button>
    </Tip>
  );
}

export function Banner({
  tone = 'info',
  title,
  children,
  action,
  onDismiss,
  dismissLabel,
}: {
  tone?: 'info' | 'warn' | 'danger';
  title: ReactNode;
  children?: ReactNode;
  action?: ReactNode;
  onDismiss?: () => void;
  dismissLabel?: string;
}) {
  return (
    <div className={`banner banner--${tone}`} role={tone === 'danger' ? 'alert' : 'status'}>
      <span className="banner__icon" aria-hidden>
        {tone === 'info' ? <Info size={18} /> : <AlertTriangle size={18} />}
      </span>
      <div className="banner__body">
        <p className="banner__title">{title}</p>
        {children && <p className="banner__text">{children}</p>}
        {action && <div className="banner__action">{action}</div>}
      </div>
      {onDismiss && (
        <button type="button" className="icon-btn icon-btn--sm" onClick={onDismiss} aria-label={dismissLabel}>
          <X size={16} aria-hidden />
        </button>
      )}
    </div>
  );
}

export function Pill({ tone = 'neutral', children }: { tone?: 'neutral' | 'primary' | 'accent' | 'warn' | 'danger'; children: ReactNode }) {
  return <span className={`pill pill--${tone}`}>{children}</span>;
}

export function Kbd({ parts }: { parts: string[] }) {
  return (
    <span className="kbd-group">
      {parts.map((p, i) => (
        <kbd key={`${p}-${i}`} className="kbd">
          {p}
        </kbd>
      ))}
    </span>
  );
}

/** Horizontal level meter (0..1). */
export function Meter({ level, label, dim }: { level: number; label: string; dim?: boolean }) {
  const pct = Math.round(Math.min(1, Math.max(0, level)) * 100);
  return (
    <div className={`meter${dim ? ' meter--dim' : ''}${pct > 92 ? ' meter--hot' : ''}`} role="meter" aria-label={label} aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct}>
      <div className="meter__fill" style={{ transform: `scaleX(${pct / 100})` }} />
    </div>
  );
}

export function Sparkline({ values, width = 240, height = 48, label }: { values: number[]; width?: number; height?: number; label: string }) {
  if (values.length < 2) {
    return <svg className="spark" width={width} height={height} role="img" aria-label={label} />;
  }
  const min = Math.min(...values);
  const max = Math.max(...values);
  const span = Math.max(max - min, 200);
  const pad = 3;
  const pts = values.map((v, i) => {
    const x = (i / (values.length - 1)) * (width - pad * 2) + pad;
    const y = height - pad - ((v - min) / span) * (height - pad * 2);
    return [x, y] as const;
  });
  const line = pts.map(([x, y], i) => `${i === 0 ? 'M' : 'L'}${x.toFixed(1)} ${y.toFixed(1)}`).join(' ');
  const area = `${line} L${(width - pad).toFixed(1)} ${height} L${pad} ${height} Z`;
  const last = pts[pts.length - 1];
  return (
    <svg className="spark" width={width} height={height} viewBox={`0 0 ${width} ${height}`} role="img" aria-label={label}>
      <path d={area} className="spark__area" />
      <path d={line} className="spark__line" fill="none" strokeLinecap="round" strokeLinejoin="round" />
      {last && <circle cx={last[0]} cy={last[1]} r={3} className="spark__dot" />}
    </svg>
  );
}

// ---------------------------------------------------------------------------------------------
// Brand glyphs
// ---------------------------------------------------------------------------------------------

const GLYPHS = { laptop: laptopIcon, monitor: monitorIcon, link: linkIcon } as const;

/** softwelop line icon, recoloured via mask (inherits `color`). Thin strokes: use at 48 px or more. */
export function BrandIcon({ kind, size = 64 }: { kind: keyof typeof GLYPHS; size?: number }) {
  const url = `url("${GLYPHS[kind]}")`;
  return <span className="brand-icon" aria-hidden style={{ width: size, height: size, WebkitMaskImage: url, maskImage: url }} />;
}

// ---------------------------------------------------------------------------------------------
// Dialogs
// ---------------------------------------------------------------------------------------------

export function Modal({
  open,
  onOpenChange,
  title,
  description,
  children,
  footer,
}: {
  open: boolean;
  onOpenChange: (o: boolean) => void;
  title: string;
  description?: string;
  children?: ReactNode;
  footer?: ReactNode;
}) {
  const t = useT();
  return (
    <DialogPrimitive.Root open={open} onOpenChange={onOpenChange}>
      <DialogPrimitive.Portal>
        <DialogPrimitive.Overlay className="modal__overlay" />
        <DialogPrimitive.Content className="modal" aria-describedby={description ? undefined : undefined}>
          <div className="modal__head">
            <DialogPrimitive.Title className="t-h2">{title}</DialogPrimitive.Title>
            <DialogPrimitive.Close asChild>
              <button type="button" className="icon-btn icon-btn--sm" aria-label={t('common.close')}>
                <X size={16} aria-hidden />
              </button>
            </DialogPrimitive.Close>
          </div>
          {description ? (
            <DialogPrimitive.Description className="modal__desc">{description}</DialogPrimitive.Description>
          ) : (
            <DialogPrimitive.Description className="sr-only">{title}</DialogPrimitive.Description>
          )}
          {children}
          {footer && <div className="modal__foot">{footer}</div>}
        </DialogPrimitive.Content>
      </DialogPrimitive.Portal>
    </DialogPrimitive.Root>
  );
}

export function ConfirmDialog({
  open,
  onOpenChange,
  title,
  description,
  confirmLabel,
  danger,
  onConfirm,
}: {
  open: boolean;
  onOpenChange: (o: boolean) => void;
  title: string;
  description: string;
  confirmLabel: string;
  danger?: boolean;
  onConfirm: () => void;
}) {
  const t = useT();
  return (
    <Modal
      open={open}
      onOpenChange={onOpenChange}
      title={title}
      description={description}
      footer={
        <>
          <Button variant="ghost" onClick={() => onOpenChange(false)}>
            {t('common.cancel')}
          </Button>
          <Button
            variant={danger ? 'danger' : 'primary'}
            onClick={() => {
              onConfirm();
              onOpenChange(false);
            }}
          >
            {confirmLabel}
          </Button>
        </>
      }
    />
  );
}

export function PromptDialog({
  open,
  onOpenChange,
  title,
  initial,
  label,
  confirmLabel,
  onConfirm,
}: {
  open: boolean;
  onOpenChange: (o: boolean) => void;
  title: string;
  initial: string;
  label: string;
  confirmLabel: string;
  onConfirm: (v: string) => void;
}) {
  const t = useT();
  const [value, setValue] = useState(initial);
  const submit = () => {
    const v = value.trim();
    if (!v) return;
    onConfirm(v);
    onOpenChange(false);
  };
  return (
    <Modal
      open={open}
      onOpenChange={(o) => {
        if (o) setValue(initial);
        onOpenChange(o);
      }}
      title={title}
      footer={
        <>
          <Button variant="ghost" onClick={() => onOpenChange(false)}>
            {t('common.cancel')}
          </Button>
          <Button variant="primary" onClick={submit} disabled={!value.trim()}>
            {confirmLabel}
          </Button>
        </>
      }
    >
      <TextInput value={value} onChange={setValue} ariaLabel={label} onEnter={submit} autoFocus width="100%" />
    </Modal>
  );
}
