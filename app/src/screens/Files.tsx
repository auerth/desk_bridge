import { ArrowDownToLine, ArrowUpFromLine, CheckCircle2, FolderOpen, UploadCloud, XCircle } from 'lucide-react';
import { motion } from 'motion/react';
import { useEffect, useState } from 'react';
import { getBackend } from '../api';
import type { TransferDto } from '../api/types';
import { Banner, Button, Card, PageHeader, Pill, Row, Segmented, Select, SwitchRow } from '../components/ui';
import { useLang, useT } from '../i18n/useT';
import { formatBytes, formatTimeAgo } from '../lib/format';
import { useStore } from '../store';

function DropZone({ peerName, enabled }: { peerName: string; enabled: boolean }) {
  const t = useT();
  const send = useStore((s) => s.send);
  const [hover, setHover] = useState(false);

  useEffect(() => {
    const un = getBackend().onFileDrop((e) => {
      if (e.type === 'enter' || e.type === 'over') setHover(true);
      else if (e.type === 'leave') setHover(false);
      else if (e.type === 'drop') {
        setHover(false);
        if (enabled && e.paths.length) void send({ type: 'send_files', paths: e.paths });
      }
    });
    return un;
  }, [enabled, send]);

  const pick = async () => {
    const paths = await getBackend().pickFiles();
    if (paths.length) await send({ type: 'send_files', paths });
  };

  return (
    <div className={`dropzone${hover ? ' is-hover' : ''}${enabled ? '' : ' is-disabled'}`}>
      <motion.div animate={{ y: hover ? -4 : 0, scale: hover ? 1.08 : 1 }} transition={{ duration: 0.2 }}>
        <UploadCloud size={40} strokeWidth={1.5} aria-hidden />
      </motion.div>
      <p className="dropzone__title">{hover ? t('files.dropActive', { name: peerName }) : t('files.drop')}</p>
      {enabled ? (
        <>
          <p className="dropzone__or">{t('files.dropOr')}</p>
          <Button variant="secondary" onClick={() => void pick()}>
            {t('files.pick')}
          </Button>
        </>
      ) : (
        <p className="dropzone__or">{t('files.needPeer')}</p>
      )}
    </div>
  );
}

function statusLabel(s: string): 'files.statusRunning' | 'files.statusDone' | 'files.statusFailed' | 'files.statusCancelled' {
  switch (s) {
    case 'running':
      return 'files.statusRunning';
    case 'done':
      return 'files.statusDone';
    case 'cancelled':
      return 'files.statusCancelled';
    default:
      return 'files.statusFailed';
  }
}

function TransferItem({ tr }: { tr: TransferDto }) {
  const t = useT();
  const lang = useLang();
  const send = useStore((s) => s.send);
  const pct = tr.total_bytes ? Math.round((tr.done_bytes / tr.total_bytes) * 100) : 0;
  return (
    <li className="transfer">
      <span className={`transfer__icon transfer__icon--${tr.status}`}>
        {tr.status === 'done' ? <CheckCircle2 size={18} aria-hidden /> : tr.status === 'running' ? (tr.direction === 'send' ? <ArrowUpFromLine size={18} aria-hidden /> : <ArrowDownToLine size={18} aria-hidden />) : <XCircle size={18} aria-hidden />}
      </span>
      <div className="transfer__body">
        <p className="transfer__name">{tr.name}</p>
        <p className="transfer__meta">
          {t(tr.direction === 'send' ? 'files.sent' : 'files.received')} · {tr.files > 1 ? `${t('files.fileCount', { n: tr.files })} · ` : ''}
          {tr.status === 'running' ? `${formatBytes(tr.done_bytes, lang)} / ${formatBytes(tr.total_bytes, lang)}` : formatBytes(tr.total_bytes, lang)} · {formatTimeAgo(tr.started_unix_ms, lang)}
        </p>
        {tr.status === 'running' && (
          <div className="progress" role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct} aria-label={tr.name}>
            <div className="progress__bar" style={{ width: `${pct}%` }} />
          </div>
        )}
      </div>
      <Pill tone={tr.status === 'done' ? 'primary' : tr.status === 'running' ? 'accent' : 'danger'}>{t(statusLabel(tr.status))}</Pill>
      {tr.status === 'done' && (
        <Button size="sm" variant="ghost" onClick={() => void send({ type: 'show_transfer_in_folder', transfer_id: tr.id })}>
          {t('files.showInFolder')}
        </Button>
      )}
    </li>
  );
}

export function Files() {
  const t = useT();
  const state = useStore((s) => s.state);
  const config = useStore((s) => s.config);
  const update = useStore((s) => s.updateConfig);
  const send = useStore((s) => s.send);
  if (!state || !config) return null;

  const online = state.connection.state === 'connected' || state.connection.state === 'degraded';
  const peerName = state.connection.peer?.name ?? state.peers[0]?.name ?? t('common.otherDevice');
  const clip = config.clipboard;
  const sizeOpts = [16, 64, 256, 512, 1024, 4096].map((v) => ({ value: String(v), label: v >= 1024 ? `${v / 1024} GB` : `${v} MB` }));
  const eagerOpts = [256, 1024, 4096].map((v) => ({ value: String(v), label: v >= 1024 ? `${v / 1024} MB` : `${v} KB` }));

  const chooseFolder = async () => {
    const dir = await getBackend().pickFolder();
    if (dir) update((c) => void (c.files.target_dir = dir));
  };

  return (
    <>
      <PageHeader title={t('files.title')} subtitle={t('files.subtitle')} />
      {!online && <Banner tone="info" title={t('files.needPeer')} />}
      <DropZone peerName={peerName} enabled={online} />

      <Card title={t('files.history')}>
        {state.transfers.length === 0 ? (
          <p className="muted empty">{t('files.historyEmpty')}</p>
        ) : (
          <ul className="transfers">
            {state.transfers.map((tr) => (
              <TransferItem key={tr.id} tr={tr} />
            ))}
          </ul>
        )}
      </Card>

      <div className="two-col">
        <Card title={t('files.targetTitle')}>
          <Row title={t('files.targetFolder')} description={config.files.target_dir || t('files.targetDefault')} stack>
            <div className="btn-group">
              <Button size="sm" onClick={() => void chooseFolder()}>
                {t('common.change')}
              </Button>
              <Button size="sm" variant="ghost" icon={<FolderOpen size={16} aria-hidden />} onClick={() => void send({ type: 'open_incoming_folder' })}>
                {t('files.openFolder')}
              </Button>
            </div>
          </Row>
          <Row title={t('files.parallel')} description={t('files.parallelHint')}>
            <Segmented
              ariaLabel={t('files.parallel')}
              size="sm"
              value={String(config.files.parallel_streams)}
              onChange={(v) => update((c) => void (c.files.parallel_streams = Number(v)))}
              options={['1', '2', '4', '8'].map((v) => ({ value: v, label: v }))}
            />
          </Row>
        </Card>

        <Card title={t('clip.title')}>
          <SwitchRow title={t('clip.send')} description={t('clip.sendHint')} checked={clip.send} onChange={(v) => update((c) => void (c.clipboard.send = v))} />
          <SwitchRow title={t('clip.receive')} description={t('clip.receiveHint')} checked={clip.receive} onChange={(v) => update((c) => void (c.clipboard.receive = v))} />
          <SwitchRow title={t('clip.images')} checked={clip.sync_images} onChange={(v) => update((c) => void (c.clipboard.sync_images = v))} />
          <SwitchRow title={t('clip.richText')} checked={clip.sync_rich_text} onChange={(v) => update((c) => void (c.clipboard.sync_rich_text = v))} />
          <SwitchRow title={t('clip.files')} checked={clip.sync_files} onChange={(v) => update((c) => void (c.clipboard.sync_files = v))} />
          <SwitchRow title={t('clip.sensitive')} description={t('clip.sensitiveHint')} checked={clip.respect_sensitive} onChange={(v) => update((c) => void (c.clipboard.respect_sensitive = v))} />
          <Row title={t('clip.maxSize')}>
            <Select ariaLabel={t('clip.maxSize')} width={140} value={String(clip.max_size_mb)} options={sizeOpts.some((o) => o.value === String(clip.max_size_mb)) ? sizeOpts : [...sizeOpts, { value: String(clip.max_size_mb), label: `${clip.max_size_mb} MB` }]} onChange={(v) => update((c) => void (c.clipboard.max_size_mb = Number(v)))} />
          </Row>
          <Row title={t('clip.eager')} description={t('clip.eagerHint')}>
            <Select ariaLabel={t('clip.eager')} width={140} value={String(clip.eager_limit_kb)} options={eagerOpts.some((o) => o.value === String(clip.eager_limit_kb)) ? eagerOpts : [...eagerOpts, { value: String(clip.eager_limit_kb), label: `${clip.eager_limit_kb} KB` }]} onChange={(v) => update((c) => void (c.clipboard.eager_limit_kb = Number(v)))} />
          </Row>
        </Card>
      </div>
    </>
  );
}
