import { useEffect, useMemo, useRef, useState } from 'react';
import { Copy, FolderOpen, RefreshCw } from 'lucide-react';
import { getBackend } from '../api';
import type { PeerLogContent } from '../api/types';
import { useT } from '../i18n/useT';
import { useStore } from '../store';
import { Button, Modal, Select, TextInput } from './ui';

/** Lines shown at most (the newest ones); the full text is in the saved file. */
const MAX_LINES = 5000;

function sizeLabel(n: number): string {
  if (n >= 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  if (n >= 1024) return `${Math.round(n / 1024)} KB`;
  return `${n} B`;
}

/** Fetches the partner's logs over the pairing and shows them (plan: remote diagnostics). */
export function PeerLogsDialog({ open, onOpenChange }: { open: boolean; onOpenChange: (o: boolean) => void }) {
  const t = useT();
  const send = useStore((s) => s.send);
  const logs = useStore((s) => s.peerLogs);
  const [loading, setLoading] = useState(false);
  const [selected, setSelected] = useState('');
  const [content, setContent] = useState<PeerLogContent | null>(null);
  const [query, setQuery] = useState('');
  const requestedAt = useRef(0);
  const preRef = useRef<HTMLPreElement>(null);

  const fetchLogs = async () => {
    setLoading(true);
    requestedAt.current = Date.now();
    const r = await send({ type: 'fetch_peer_logs' });
    if (r === undefined) setLoading(false);
  };

  // First open: fetch right away.
  useEffect(() => {
    if (open && !logs && !loading) void fetchLogs();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  // Answer arrived: stop the spinner and pick the newest agent log.
  useEffect(() => {
    if (!logs) return;
    setLoading(false);
    const names = logs.files.map((f) => f.name);
    if (!names.includes(selected)) {
      setSelected(names.find((n) => n.startsWith('agent.log')) ?? names[0] ?? '');
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [logs?.at]);

  useEffect(() => {
    if (!open || !selected) return;
    let alive = true;
    void send<PeerLogContent>({ type: 'get_peer_log', name: selected }).then((c) => {
      if (alive && c) setContent(c);
    });
    return () => {
      alive = false;
    };
  }, [open, selected, logs?.at, send]);

  const lines = useMemo(() => {
    const all = (content?.content ?? '').split(/\r?\n/);
    const q = query.trim().toLowerCase();
    const hit = q ? all.filter((l) => l.toLowerCase().includes(q)) : all;
    return hit.slice(-MAX_LINES);
  }, [content, query]);

  // Newest lines at the bottom: scroll there when the text changes.
  useEffect(() => {
    const el = preRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [lines]);

  const file = logs?.files.find((f) => f.name === selected);

  return (
    <Modal
      open={open}
      onOpenChange={onOpenChange}
      wide
      title={logs?.peer ? t('peerLogs.titleFrom', { peer: logs.peer }) : t('peerLogs.title')}
      description={t('peerLogs.description')}
      footer={
        <>
          <Button
            size="sm"
            icon={<Copy size={15} aria-hidden />}
            disabled={!content}
            onClick={() => void navigator.clipboard.writeText(lines.join('\n'))}
          >
            {t('peerLogs.copy')}
          </Button>
          <Button
            size="sm"
            icon={<FolderOpen size={15} aria-hidden />}
            disabled={!logs?.folder}
            onClick={() => logs?.folder && void getBackend().revealPath(logs.folder)}
          >
            {t('peerLogs.openFolder')}
          </Button>
          <Button size="sm" variant="primary" icon={<RefreshCw size={15} aria-hidden />} loading={loading} onClick={() => void fetchLogs()}>
            {t('peerLogs.refresh')}
          </Button>
        </>
      }
    >
      {logs?.error ? (
        <p className="peer-logs__error">{logs.error}</p>
      ) : !logs || logs.files.length === 0 ? (
        <p className="peer-logs__empty">{loading ? t('peerLogs.loading') : t('peerLogs.none')}</p>
      ) : (
        <div className="peer-logs">
          <div className="peer-logs__bar">
            <Select
              ariaLabel={t('peerLogs.file')}
              width={280}
              value={selected}
              options={logs.files.map((f) => ({ value: f.name, label: `${f.name} (${sizeLabel(f.size)})` }))}
              onChange={(v) => setSelected(v)}
            />
            <TextInput value={query} onChange={setQuery} placeholder={t('peerLogs.filter')} ariaLabel={t('peerLogs.filter')} width={220} />
            <span className="peer-logs__meta">
              {t('peerLogs.lines', { n: lines.length })}
              {file?.truncated ? ` · ${t('peerLogs.truncated')}` : ''}
            </span>
          </div>
          <pre ref={preRef} className="peer-logs__text" tabIndex={0}>
            {lines.map((l, i) => (
              <span key={i} className={/\bERROR\b/.test(l) ? 'peer-logs__err' : /\bWARN\b/.test(l) ? 'peer-logs__warn' : undefined}>
                {l}
                {'\n'}
              </span>
            ))}
          </pre>
        </div>
      )}
    </Modal>
  );
}
