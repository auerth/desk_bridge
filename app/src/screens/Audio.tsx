import { Mic, RefreshCw, Volume2 } from 'lucide-react';
import { useEffect, useMemo } from 'react';
import type { AudioDeviceDto } from '../api/types';
import { Banner, Button, Card, Kbd, Meter, PageHeader, Pill, Row, Segmented, Select, Slider, SwitchRow, type Option } from '../components/ui';
import { useT } from '../i18n/useT';
import { useLang } from '../i18n/useT';
import { DEFAULT_HOTKEYS } from '../api/types';
import { hotkeyParts } from '../lib/hotkey';
import { useStore } from '../store';

const DEFAULT_ID = '__default__';

function deviceOptions(list: AudioDeviceDto[], defaultLabel: string, virtualTag: string): Option<string>[] {
  return [
    { value: DEFAULT_ID, label: defaultLabel },
    ...list.map((d) => ({ value: d.id, label: d.is_virtual ? `${d.name} (${virtualTag})` : d.name })),
  ];
}

export function Audio() {
  const t = useT();
  const lang = useLang();
  const state = useStore((s) => s.state);
  const config = useStore((s) => s.config);
  const devices = useStore((s) => s.audioDevices);
  const update = useStore((s) => s.updateConfig);
  const send = useStore((s) => s.send);

  useEffect(() => {
    void send({ type: 'list_audio_devices' }, { silent: true });
  }, [send]);

  const renderOpts = useMemo(() => deviceOptions(devices?.render ?? [], t('audio.systemDefault'), t('audio.virtualTag')), [devices, t]);
  const captureOpts = useMemo(() => deviceOptions(devices?.capture ?? [], t('audio.defaultComm'), t('audio.virtualTag')), [devices, t]);

  if (!state || !config) return null;
  const lap = state.features.laptop_audio;
  const mic = state.features.mic;
  const cfg = config.audio;
  const online = state.connection.state === 'connected' || state.connection.state === 'degraded';
  const micUnavailable = mic.state === 'unavailable';
  const micKeys = hotkeyParts(config.hotkeys.mic_mute ?? DEFAULT_HOTKEYS.mic_mute, lang);

  return (
    <>
      <PageHeader title={t('audio.title')} subtitle={t('audio.subtitle')} />
      {!state.device.headset_sink && <Banner tone="info" title={t('audio.notHeadsetDevice')} />}

      <Card
        icon={<Volume2 size={20} aria-hidden />}
        title={t('audio.laptopToHeadset')}
        subtitle={t('audio.laptopToHeadsetSub')}
        actions={<Pill tone={lap.state === 'live' ? 'accent' : 'neutral'}>{t(`live.${lap.state}`)}</Pill>}
      >
        {lap.device_missing && (
          <Banner tone="warn" title={t('warn.headset_missing')}>
            {t('audio.deviceMissing')}
          </Banner>
        )}
        <Row title={t('audio.output')} stack>
          <div className="inline">
            <Select
              ariaLabel={t('audio.output')}
              width={340}
              value={cfg.laptop_to_pc.output_device || DEFAULT_ID}
              options={renderOpts}
              onChange={(v) => update((c) => void (c.audio.laptop_to_pc.output_device = v === DEFAULT_ID ? '' : v))}
            />
            <Button variant="ghost" size="sm" icon={<RefreshCw size={14} aria-hidden />} onClick={() => void send({ type: 'list_audio_devices' })}>
              {t('audio.refresh')}
            </Button>
          </div>
        </Row>
        <Row title={t('audio.volume')}>
          <Slider
            ariaLabel={t('audio.volume')}
            min={0}
            max={150}
            step={1}
            value={Math.round(cfg.laptop_to_pc.volume * 100)}
            format={(v) => `${v} %`}
            onChange={(v) => update((c) => void (c.audio.laptop_to_pc.volume = v / 100))}
          />
        </Row>
        <SwitchRow
          title={t('audio.muteLocal')}
          description={t('audio.muteLocalHint')}
          checked={cfg.laptop_to_pc.mute_local_output}
          onChange={(v) => update((c) => void (c.audio.laptop_to_pc.mute_local_output = v))}
        />
        <SwitchRow title={t('audio.mute')} checked={cfg.laptop_to_pc.mute} onChange={(v) => update((c) => void (c.audio.laptop_to_pc.mute = v))} />
        <Row title={t('audio.level')}>
          <div className="meter-row">
            <Meter level={lap.level} label={t('audio.level')} dim={cfg.laptop_to_pc.mute || !online} />
            {cfg.laptop_to_pc.mute && <Pill tone="warn">{t('audio.mutedNow')}</Pill>}
            <span className="num muted meter-row__val">{lap.latency_ms != null && online ? t('audio.latencyValue', { n: Math.round(lap.latency_ms) }) : '–'}</span>
          </div>
        </Row>
        <Row title={t('audio.codec')} description={t('audio.codecHint')} stack>
          <div className="inline">
            <Segmented
              ariaLabel={t('audio.codec')}
              value={cfg.laptop_to_pc.codec}
              onChange={(v) => update((c) => void (c.audio.laptop_to_pc.codec = v))}
              options={[
                { value: 'auto', label: t('audio.codecAuto') },
                { value: 'pcm', label: t('audio.codecPcm') },
                { value: 'opus', label: t('audio.codecOpus') },
              ]}
            />
            {lap.codec && online && <Pill tone="primary">{t('audio.codecNow', { codec: lap.codec.toUpperCase() })}</Pill>}
          </div>
        </Row>
        {cfg.laptop_to_pc.codec !== 'pcm' && (
          <Row title={t('audio.opusBitrate')}>
            <Slider
              ariaLabel={t('audio.opusBitrate')}
              min={32}
              max={320}
              step={8}
              value={cfg.laptop_to_pc.opus_bitrate_kbps}
              format={(v) => `${v} kbit/s`}
              onChange={(v) => update((c) => void (c.audio.laptop_to_pc.opus_bitrate_kbps = v))}
            />
          </Row>
        )}
        <Row title={t('audio.testTone')} description={t('audio.testToneHint')}>
          <Button disabled={!online} onClick={() => void send({ type: 'play_test_tone' })}>
            {t('audio.testTone')}
          </Button>
        </Row>
      </Card>

      <Card
        icon={<Mic size={20} aria-hidden />}
        title={t('audio.micTitle')}
        subtitle={t('audio.micSub')}
        className={micUnavailable ? 'card--soon' : undefined}
        actions={micUnavailable ? <Pill tone="warn">{t('common.comingSoon')}</Pill> : <Pill tone={mic.state === 'live' ? 'accent' : 'neutral'}>{t(`live.${mic.state}`)}</Pill>}
      >
        {micUnavailable && (
          <Banner tone="info" title={t('audio.micUnavailableTitle')}>
            {t('audio.micUnavailableBody')}
          </Banner>
        )}
        <div className={micUnavailable ? 'is-inert' : undefined} aria-disabled={micUnavailable || undefined}>
          <Row title={t('audio.input')} stack>
            <Select
              ariaLabel={t('audio.input')}
              width={340}
              disabled={micUnavailable}
              value={cfg.mic_to_laptop.input_device || DEFAULT_ID}
              options={captureOpts}
              onChange={(v) => update((c) => void (c.audio.mic_to_laptop.input_device = v === DEFAULT_ID ? '' : v))}
            />
          </Row>
          <Row title={t('audio.level')} description={mic.state === 'live' && mic.detail ? mic.detail : undefined}>
            <Meter level={mic.level} label={t('audio.level')} dim={micUnavailable || cfg.mic_to_laptop.mute} />
          </Row>
          <SwitchRow
            title={t('audio.mute')}
            description={
              <span className="inline">
                {t('audio.micHotkey', { keys: '' })}
                <Kbd parts={micKeys} />
              </span>
            }
            disabled={micUnavailable}
            checked={cfg.mic_to_laptop.mute}
            onChange={(v) => update((c) => void (c.audio.mic_to_laptop.mute = v))}
          />
          <SwitchRow
            title={t('audio.micOnlyWhenUsed')}
            description={t('audio.micOnlyWhenUsedHint')}
            disabled={micUnavailable}
            checked={cfg.mic_to_laptop.only_when_used}
            onChange={(v) => update((c) => void (c.audio.mic_to_laptop.only_when_used = v))}
          />
          <SwitchRow
            title={t('audio.micSetDefault')}
            description={t('audio.micSetDefaultHint')}
            disabled={micUnavailable}
            checked={cfg.mic_to_laptop.set_as_default}
            onChange={(v) => update((c) => void (c.audio.mic_to_laptop.set_as_default = v))}
          />
        </div>
      </Card>
    </>
  );
}
