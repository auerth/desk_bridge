import { FileArchive, FolderOpen, Laptop, Monitor, Pause, Play, Plus, RotateCw, Trash2, Unlink } from 'lucide-react';
import { useState } from 'react';
import { getBackend } from '../api';
import { PairedBadge, PairingPanel } from '../components/Pairing';
import { Button, Card, ConfirmDialog, Modal, PageHeader, Pill, PromptDialog, Row, Segmented, Select, SwitchRow, TextInput } from '../components/ui';
import { useT } from '../i18n/useT';
import { useStore } from '../store';

type UpdateStatus =
  | { kind: 'idle' }
  | { kind: 'checking' }
  | { kind: 'uptodate' }
  | { kind: 'available'; version: string }
  | { kind: 'installing' }
  | { kind: 'error'; text: string };

function PortField() {
  const t = useT();
  const port = useStore((s) => s.config?.network.port ?? 47820);
  const update = useStore((s) => s.updateConfig);
  const [text, setText] = useState(String(port));
  const n = Number(text);
  const valid = Number.isInteger(n) && n >= 1024 && n <= 65535;
  const commit = () => {
    if (valid && n !== port) update((c) => void (c.network.port = n));
    else if (!valid) setText(String(port));
  };
  return (
    <input
      className={`input input--num${valid ? '' : ' input--invalid'}`}
      inputMode="numeric"
      aria-label={t('settings.port')}
      aria-invalid={!valid || undefined}
      value={text}
      onChange={(e) => setText(e.target.value.replace(/\D/g, '').slice(0, 5))}
      onBlur={commit}
      onKeyDown={(e) => e.key === 'Enter' && commit()}
    />
  );
}

export function Settings() {
  const t = useT();
  const state = useStore((s) => s.state);
  const config = useStore((s) => s.config);
  const update = useStore((s) => s.updateConfig);
  const send = useStore((s) => s.send);
  const updateOffer = useStore((s) => s.updateOffer);

  const [renamePeer, setRenamePeer] = useState<{ id: string; name: string } | null>(null);
  const [unpair, setUnpair] = useState<{ id: string; name: string } | null>(null);
  const [pairOpen, setPairOpen] = useState(false);
  const [paired, setPaired] = useState<string | null | undefined>(undefined);
  const [host, setHost] = useState('');
  const [upd, setUpd] = useState<UpdateStatus>({ kind: 'idle' });
  const [latency, setLatency] = useState<number | null>(null);
  const [exporting, setExporting] = useState(false);

  if (!state || !config) return null;
  const backend = getBackend();

  const checkUpdate = async () => {
    setUpd({ kind: 'checking' });
    void send({ type: 'check_for_update' }, { silent: true });
    try {
      const r = await backend.checkUpdate();
      setUpd(r ? { kind: 'available', version: r.version } : { kind: 'uptodate' });
    } catch (e) {
      const raw = String(e instanceof Error ? e.message : e);
      setUpd({ kind: 'error', text: raw.includes('updater_unavailable') ? t('settings.updateNotConfigured') : t('settings.updateFailed', { error: raw }) });
    }
  };

  const addHost = async () => {
    const h = host.trim();
    if (!h || config.network.manual_hosts.includes(h)) return;
    setHost('');
    await send({ type: 'add_manual_host', host: h });
    update((c) => {
      if (!c.network.manual_hosts.includes(h)) c.network.manual_hosts.push(h);
    });
  };

  const testLatency = async () => {
    const t0 = performance.now();
    const r = await send({ type: 'ping' });
    if (r !== undefined) setLatency(Math.round((performance.now() - t0) * 10) / 10);
  };

  const available = upd.kind === 'available' ? upd.version : (updateOffer?.version ?? state.update.available);

  return (
    <>
      <PageHeader title={t('settings.title')} subtitle={t('settings.subtitle')} />

      <Card title={t('settings.general')}>
        <SwitchRow title={t('settings.autostart')} description={t('settings.autostartHint')} checked={config.general.autostart} onChange={(v) => update((c) => void (c.general.autostart = v))} />
        <Row title={t('settings.language')}>
          <Select
            ariaLabel={t('settings.language')}
            width={240}
            value={config.general.language}
            options={[
              { value: 'auto', label: t('settings.langAuto') },
              { value: 'de', label: t('settings.langDe') },
              { value: 'en', label: t('settings.langEn') },
            ]}
            onChange={(v) => update((c) => void (c.general.language = v))}
          />
        </Row>
        <Row title={t('settings.theme')}>
          <Segmented
            ariaLabel={t('settings.theme')}
            value={config.general.theme}
            onChange={(v) => update((c) => void (c.general.theme = v))}
            options={[
              { value: 'system', label: t('settings.themeSystem') },
              { value: 'light', label: t('settings.themeLight') },
              { value: 'dark', label: t('settings.themeDark') },
            ]}
          />
        </Row>
      </Card>

      <Card title={t('settings.thisDevice')}>
        <Row title={t('settings.deviceName')} stack>
          <TextInput ariaLabel={t('settings.deviceName')} width={300} value={config.device.name || state.device.name} onChange={(v) => update((c) => void (c.device.name = v))} />
        </Row>
        <SwitchRow title={t('settings.roleInput')} checked={config.device.input_source} onChange={(v) => update((c) => void (c.device.input_source = v))} />
        <SwitchRow title={t('settings.roleHeadset')} checked={config.device.headset_sink} onChange={(v) => update((c) => void (c.device.headset_sink = v))} />
        <SwitchRow title={t('settings.roleStream')} checked={config.device.stream_source} onChange={(v) => update((c) => void (c.device.stream_source = v))} />
      </Card>

      <Card
        title={t('settings.devices')}
        actions={
          <Button
            variant="primary"
            size="sm"
            icon={<Plus size={16} aria-hidden />}
            onClick={() => {
              setPaired(undefined);
              setPairOpen(true);
            }}
          >
            {t('settings.pairNew')}
          </Button>
        }
      >
        {state.peers.length === 0 ? (
          <p className="muted empty">{t('settings.devicesEmpty')}</p>
        ) : (
          <ul className="peer-list">
            {state.peers.map((p) => (
              <li key={p.id} className="peer">
                <span className="peer__icon">{/laptop/i.test(p.name) ? <Laptop size={20} aria-hidden /> : <Monitor size={20} aria-hidden />}</span>
                <div className="peer__text">
                  <p className="peer__name">
                    {p.name} <Pill tone={p.online ? 'primary' : 'neutral'}>{p.online ? t('settings.online') : t('settings.offline')}</Pill>
                  </p>
                  <p className="peer__sub">
                    {t('settings.fingerprint', { fp: p.fingerprint_short })}
                    {p.last_ip ? ` · ${p.last_ip}` : ''}
                  </p>
                </div>
                <Button size="sm" variant="ghost" onClick={() => setRenamePeer({ id: p.id, name: p.name })}>
                  {t('common.rename')}
                </Button>
                <Button size="sm" variant="ghost" icon={<Unlink size={15} aria-hidden />} onClick={() => setUnpair({ id: p.id, name: p.name })}>
                  {t('settings.unpair')}
                </Button>
              </li>
            ))}
          </ul>
        )}
      </Card>

      <Card title={t('settings.network')}>
        <Row title={t('settings.port')} description={t('settings.portHint')}>
          <PortField />
        </Row>
        <Row title={t('settings.manualHosts')} description={t('settings.manualHostsHint')} stack>
          <div className="host-list">
            {config.network.manual_hosts.map((h) => (
              <span key={h} className="host-chip">
                {h}
                <button type="button" aria-label={`${t('common.remove')}: ${h}`} onClick={() => update((c) => void (c.network.manual_hosts = c.network.manual_hosts.filter((x) => x !== h)))}>
                  <Trash2 size={13} aria-hidden />
                </button>
              </span>
            ))}
          </div>
          <div className="inline">
            <TextInput value={host} onChange={setHost} onEnter={() => void addHost()} placeholder={t('settings.manualHostPlaceholder')} ariaLabel={t('settings.manualHosts')} width={260} />
            <Button size="sm" disabled={!host.trim()} onClick={() => void addHost()}>
              {t('common.add')}
            </Button>
          </div>
        </Row>
      </Card>

      <Card title={t('settings.updates')}>
        <Row
          title={t('settings.version', { version: state.update.current || state.device.version })}
          description={
            <span role="status">
              {upd.kind === 'checking' || state.update.checking
                ? t('settings.checking')
                : upd.kind === 'uptodate'
                  ? t('settings.upToDate')
                  : upd.kind === 'error'
                    ? upd.text
                    : upd.kind === 'installing'
                      ? t('settings.updateInstalling')
                      : available
                        ? t('settings.updateAvailable', { version: available })
                        : undefined}
            </span>
          }
        >
          <div className="btn-group">
            <Button size="sm" loading={upd.kind === 'checking'} onClick={() => void checkUpdate()}>
              {t('settings.checkUpdate')}
            </Button>
            {available && upd.kind !== 'installing' && (
              <Button
                size="sm"
                variant="primary"
                onClick={async () => {
                  setUpd({ kind: 'installing' });
                  try {
                    await backend.installUpdate();
                  } catch (e) {
                    setUpd({ kind: 'error', text: t('settings.updateFailed', { error: String(e instanceof Error ? e.message : e) }) });
                  }
                }}
              >
                {t('settings.updateInstall')}
              </Button>
            )}
          </div>
        </Row>
        <SwitchRow title={t('settings.updateAuto')} checked={config.update.check_automatically} onChange={(v) => update((c) => void (c.update.check_automatically = v))} />
        <SwitchRow title={t('settings.updatePartner')} checked={config.update.offer_partner_update} onChange={(v) => update((c) => void (c.update.offer_partner_update = v))} />
      </Card>

      <Card title={t('settings.diagnostics')}>
        <Row title={t('settings.openLogs')}>
          <Button size="sm" icon={<FolderOpen size={16} aria-hidden />} onClick={() => void backend.openLogsFolder()}>
            {t('settings.openLogs')}
          </Button>
        </Row>
        <Row title={t('settings.exportDiag')} description={t('settings.exportDiagHint')}>
          <Button
            size="sm"
            icon={<FileArchive size={16} aria-hidden />}
            loading={exporting}
            onClick={async () => {
              setExporting(true);
              await send({ type: 'export_diagnostics', target: null });
              setExporting(false);
            }}
          >
            {t('settings.exportDiag')}
          </Button>
        </Row>
        <Row title={t('settings.logLevel')}>
          <Select
            ariaLabel={t('settings.logLevel')}
            width={160}
            value={config.general.log_level}
            options={(['error', 'warn', 'info', 'debug', 'trace'] as const).map((v) => ({ value: v, label: v }))}
            onChange={(v) => update((c) => void (c.general.log_level = v))}
          />
        </Row>
        <Row title={t('settings.latencyTest')} description={latency != null ? t('settings.latencyResult', { n: latency }) : t('settings.latencyHint')}>
          <Button size="sm" onClick={() => void testLatency()}>
            {t('settings.latencyTest')}
          </Button>
        </Row>
      </Card>

      <Card title={t('settings.advanced')}>
        <Row title={state.paused ? t('settings.resume') : t('settings.pause')} description={t('settings.pauseHint')}>
          <Button size="sm" icon={state.paused ? <Play size={16} aria-hidden /> : <Pause size={16} aria-hidden />} onClick={() => void send({ type: 'set_paused', paused: !state.paused })}>
            {state.paused ? t('settings.resume') : t('settings.pause')}
          </Button>
        </Row>
        <Row title={t('settings.restartAgent')} description={t('settings.restartAgentHint')}>
          <Button size="sm" variant="secondary" icon={<RotateCw size={16} aria-hidden />} onClick={() => void send({ type: 'restart_agent' })}>
            {t('settings.restartAgent')}
          </Button>
        </Row>
      </Card>

      <PromptDialog
        open={renamePeer != null}
        onOpenChange={(o) => !o && setRenamePeer(null)}
        title={t('settings.renameTitle')}
        label={t('settings.renameTitle')}
        initial={renamePeer?.name ?? ''}
        confirmLabel={t('common.save')}
        onConfirm={(name) => renamePeer && void send({ type: 'rename_peer', device_id: renamePeer.id, name })}
      />
      <ConfirmDialog
        open={unpair != null}
        onOpenChange={(o) => !o && setUnpair(null)}
        title={t('settings.unpairTitle', { name: unpair?.name ?? '' })}
        description={t('settings.unpairBody')}
        confirmLabel={t('settings.unpair')}
        danger
        onConfirm={() => unpair && void send({ type: 'unpair', device_id: unpair.id })}
      />
      <Modal open={pairOpen} onOpenChange={setPairOpen} title={t('pair.title')}>
        {paired !== undefined ? (
          <PairedBadge name={paired} />
        ) : (
          <PairingPanel suggestShow={state.device.input_source} onPaired={(n) => { setPaired(n); setTimeout(() => setPairOpen(false), 1600); }} />
        )}
      </Modal>
    </>
  );
}
