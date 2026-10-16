import { useEffect, useState } from 'react';
import type { AppState, PlacementDto } from '../api/types';
import { LayoutEditor } from '../components/LayoutEditor';
import { Card, PageHeader, Segmented, Slider, SwitchRow, Row } from '../components/ui';
import { useT } from '../i18n/useT';
import { useStore } from '../store';

/** The laptop is the device that is shown in focus mode. */
export function laptopDeviceId(state: AppState): string | null {
  if (state.device.stream_source && !state.device.input_source) return state.device.id;
  const peer = state.connection.peer ?? state.peers[0];
  return peer?.id ?? null;
}

export function deviceNames(state: AppState): Record<string, string> {
  const names: Record<string, string> = { [state.device.id]: state.device.name };
  for (const p of state.peers) names[p.id] = p.name;
  if (state.connection.peer) names[state.connection.peer.id] = state.connection.peer.name;
  return names;
}

/** Layout editor bound to the agent (shared by the Screens page and the onboarding step). */
export function BoundLayoutEditor({ height }: { height?: number }) {
  const state = useStore((s) => s.state);
  const send = useStore((s) => s.send);
  const [local, setLocal] = useState<PlacementDto[] | null>(null);
  const revision = state?.screens.revision;
  useEffect(() => setLocal(null), [revision]);
  if (!state) return null;
  return (
    <LayoutEditor
      screens={state.screens.screens}
      placements={local ?? state.screens.placements}
      thisDeviceId={state.device.id}
      deviceNames={deviceNames(state)}
      laptopDeviceId={laptopDeviceId(state)}
      height={height}
      onCommit={(p) => {
        setLocal(p);
        void send({ type: 'set_layout', placements: p });
      }}
    />
  );
}

export function Screens() {
  const t = useT();
  const state = useStore((s) => s.state);
  const config = useStore((s) => s.config);
  const update = useStore((s) => s.updateConfig);
  if (!state || !config) return null;
  const inp = config.input;
  const hasScreens = state.screens.screens.length > 0;

  return (
    <>
      <PageHeader title={t('screens.title')} subtitle={t('screens.subtitle')} />
      <Card>
        {hasScreens ? (
          <>
            <BoundLayoutEditor />
            <p className="hint">{t('screens.hint')}</p>
          </>
        ) : (
          <p className="muted empty">{t('screens.empty')}</p>
        )}
      </Card>

      <Card title={t('screens.optionsTitle')}>
        <Row title={t('screens.delay')} description={t('screens.delayHint')}>
          <Slider
            value={inp.switch_delay_ms}
            min={0}
            max={500}
            step={10}
            ariaLabel={t('screens.delay')}
            format={(v) => t('common.ms', { n: v })}
            onChange={(v) => update((c) => void (c.input.switch_delay_ms = v))}
          />
        </Row>
        <Row title={t('screens.modifier')} description={t('screens.modifierHint')}>
          <Segmented
            ariaLabel={t('screens.modifier')}
            value={inp.require_modifier}
            onChange={(v) => update((c) => void (c.input.require_modifier = v))}
            options={[
              { value: 'none', label: t('screens.modNone') },
              { value: 'ctrl', label: t('screens.modCtrl') },
              { value: 'alt', label: t('screens.modAlt') },
              { value: 'shift', label: t('screens.modShift') },
            ]}
          />
        </Row>
        <Row title={t('screens.corner')} description={t('screens.cornerHint')}>
          <Slider
            value={inp.corner_margin_px}
            min={0}
            max={200}
            step={2}
            ariaLabel={t('screens.corner')}
            format={(v) => t('screens.cornerValue', { n: v })}
            onChange={(v) => update((c) => void (c.input.corner_margin_px = v))}
          />
        </Row>
        <SwitchRow
          title={t('screens.doubleBump')}
          description={t('screens.doubleBumpHint')}
          checked={inp.double_bump}
          onChange={(v) => update((c) => void (c.input.double_bump = v))}
        />
        <SwitchRow
          title={t('screens.oneToOne')}
          description={t('screens.oneToOneHint')}
          checked={inp.raw_one_to_one}
          onChange={(v) => update((c) => void (c.input.raw_one_to_one = v))}
        />
        <SwitchRow
          title={t('screens.hideCursor')}
          description={t('screens.hideCursorHint')}
          checked={inp.hide_cursor_on_remote}
          onChange={(v) => update((c) => void (c.input.hide_cursor_on_remote = v))}
        />
      </Card>
    </>
  );
}
