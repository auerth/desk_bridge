import { Download, ExternalLink, FileArchive, FileText, FolderOpen, Send, Laptop, Monitor, Pause, Play, Plus, RefreshCw, RotateCw, Trash2, Unlink } from 'lucide-react';
import { useState } from 'react';
import { getBackend } from '../api';
import { PairedBadge, PairingPanel } from '../components/Pairing';
import { PeerLogsDialog } from '../components/PeerLogs';
import { usePartnerUpdate } from '../components/Shell';
import { Button, Card, ConfirmDialog, Modal, PageHeader, Pill, PromptDialog, Row, Segmented, Select, SwitchRow, TextInput } from '../components/ui';
import { useLang, useT } from '../i18n/useT';
import { formatMs, installerVersion } from '../lib/format';
import { useStore } from '../store';
import { RELEASES_URL } from '../brand';

/** Comma separated list of IP addresses stored as a string array / single string. */
function AddressListField({ label, value, placeholder, onCommit }: { label: string; value: string; placeholder: string; onCommit: (v: string) => void }) {
  const [text, setText] = useState(value);
  return (
    <TextInput
      value={text}
      onChange={setText}
      onBlur={() => text !== value && onCommit(text.trim())}
      onEnter={() => onCommit(text.trim())}
      placeholder={placeholder}
      ariaLabel={label}
      width={300}
    />
  );
}

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
  const pushToast = useStore((s) => s.pushToast);
  const lang = useLang();

  const [renamePeer, setRenamePeer] = useState<{ id: string; name: string } | null>(null);
  const [unpair, setUnpair] = useState<{ id: string; name: string } | null>(null);
  const [pairOpen, setPairOpen] = useState(false);
  const [paired, setPaired] = useState<string | null | undefined>(undefined);
  const [host, setHost] = useState('');
  const [latency, setLatency] = useState<number | null>(null);
  const [exporting, setExporting] = useState(false);
  const [peerLogsOpen, setPeerLogsOpen] = useState(false);
  const pu = usePartnerUpdate();

  if (!state || !config) return null;
  const backend = getBackend();

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

  const sendInstaller = async () => {
    const path = await backend.pickInstaller();
    if (!path) return;
    const version = installerVersion(path);
    if (!version) {
      pushToast({ level: 'error', text: t('settings.sendInstallerBadName') });
      return;
    }
    await send({ type: 'offer_partner_update', path, version });
  };

  const m = state.metrics;
  const ms = (us: number | null) => (us == null ? '–' : formatMs(us, lang));

  const upd = state.update;

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
        <Row title={t('settings.bindAddress')} description={t('settings.bindAddressHint')}>
          <AddressListField
            label={t('settings.bindAddress')}
            value={config.network.bind_address}
            placeholder={t('settings.automatic')}
            onCommit={(v) => update((c) => void (c.network.bind_address = v))}
          />
        </Row>
        <Row title={t('settings.advertise')} description={t('settings.advertiseHint')}>
          <AddressListField
            label={t('settings.advertise')}
            value={config.network.advertise_addresses.join(', ')}
            placeholder={t('settings.automatic')}
            onCommit={(v) =>
              update((c) => void (c.network.advertise_addresses = v.split(/[\s,;]+/).map((x) => x.trim()).filter(Boolean)))
            }
          />
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
              {upd.checking
                ? t('settings.checking')
                : upd.available
                  ? t('settings.updateAvailable', { version: upd.available })
                  : upd.error
                    ? t('settings.updateFailed', { error: upd.error })
                    : upd.last_checked_unix_ms
                      ? t('settings.upToDate')
                      : undefined}
            </span>
          }
        >
          <div className="btn-group">
            <Button size="sm" loading={upd.checking} onClick={() => void send({ type: 'check_for_update' })}>
              {t('settings.checkUpdate')}
            </Button>
            {upd.available && (
              <Button size="sm" variant="primary" icon={<ExternalLink size={15} aria-hidden />} onClick={() => void backend.openUrl(upd.url ?? RELEASES_URL)}>
                {t('settings.openRelease')}
              </Button>
            )}
          </div>
        </Row>
        {pu.direction === 'push' && (
          <Row
            title={t('pu.pushTitle', { name: pu.peerName })}
            description={
              <span role="status">
                {pu.busy
                  ? t('pu.pushBusy', { version: upd.partner_updating ?? pu.own })
                  : `${t(pu.partnerRemote ? 'pu.pushHint' : 'pu.pushHintConfirm', { peer: pu.peer ?? '', own: pu.own })}${pu.installerAvailable ? '' : ` ${t('pu.pickHint', { own: pu.own })}`}`}
              </span>
            }
          >
            <Button size="sm" variant="primary" icon={<RefreshCw size={15} aria-hidden />} loading={pu.busy} onClick={() => void pu.run()}>
              {t('pu.pushButton', { name: pu.peerName })}
            </Button>
          </Row>
        )}
        {pu.direction === 'pull' && (
          <Row
            title={t('pu.pullTitle')}
            description={<span role="status">{pu.busy ? t('pu.pullBusy', { version: pu.peer ?? '' }) : t('pu.pullHint', { name: pu.peerName, peer: pu.peer ?? '' })}</span>}
          >
            <Button size="sm" variant="primary" icon={<Download size={15} aria-hidden />} loading={pu.busy} onClick={() => void pu.run()}>
              {t('pu.pullButton', { version: pu.peer ?? '' })}
            </Button>
          </Row>
        )}
        <SwitchRow title={t('settings.updateAuto')} checked={config.update.check_automatically} onChange={(v) => update((c) => void (c.update.check_automatically = v))} />
        <Row title={t('settings.sendInstaller')} description={t('settings.sendInstallerHint')}>
          <Button size="sm" icon={<Send size={15} aria-hidden />} disabled={state.connection.state !== 'connected'} onClick={() => void sendInstaller()}>
            {t('settings.sendInstaller')}
          </Button>
        </Row>
        <SwitchRow title={t('settings.updatePartner')} checked={config.update.offer_partner_update} onChange={(v) => update((c) => void (c.update.offer_partner_update = v))} />
        <SwitchRow
          title={t('settings.allowPartnerInstall')}
          description={t('settings.allowPartnerInstallHint')}
          checked={config.update.allow_partner_install}
          onChange={(v) => update((c) => void (c.update.allow_partner_install = v))}
        />
      </Card>

      <Card title={t('settings.diagnostics')}>
        <Row title={t('settings.openLogs')}>
          <Button size="sm" icon={<FolderOpen size={16} aria-hidden />} onClick={() => void backend.openLogsFolder()}>
            {t('settings.openLogs')}
          </Button>
        </Row>
        <Row title={t('peerLogs.title')} description={t('peerLogs.hint')}>
          <Button size="sm" icon={<FileText size={16} aria-hidden />} disabled={state.connection.state !== 'connected'} onClick={() => setPeerLogsOpen(true)}>
            {t('peerLogs.show')}
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
        <Row title={t('settings.inputLatency')} description={t('settings.inputLatencyHint')}>
          <span className="num">{t('settings.inputLatencyValue', { p50: ms(m.input_latency_p50_us), p95: ms(m.input_latency_p95_us), max: ms(m.input_latency_max_us) })}</span>
        </Row>
        <Row title={t('settings.agentLoad')}>
          <span className="num">
            {t('settings.agentLoadValue', {
              cpu: m.agent_cpu_pct == null ? '–' : m.agent_cpu_pct.toLocaleString(lang, { maximumFractionDigits: 1 }),
              ram: m.agent_rss_mb == null ? '–' : Math.round(m.agent_rss_mb),
            })}
          </span>
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

      <PeerLogsDialog open={peerLogsOpen} onOpenChange={setPeerLogsOpen} />
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
