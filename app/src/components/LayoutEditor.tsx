import { Laptop, Monitor } from 'lucide-react';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { PlacementDto, ScreenDto } from '../api/types';
import { useT } from '../i18n/useT';
import { arrangeGroup, buildTiles, currentSide, snapTile, toPlacements, tileKey, type QuickSide, type SnapResult, type Tile } from '../lib/layout';
import { Button } from './ui';

interface DragState {
  key: string;
  x: number;
  y: number;
  snap: SnapResult;
}

/**
 * Drag-to-arrange editor. Tiles are scaled by layout units (pixels * 96 / dpi), so a 2560 px laptop
 * panel at 150 % scaling is drawn smaller than a 2560 px monitor at 100 %.
 */
export function LayoutEditor({
  screens,
  placements,
  thisDeviceId,
  deviceNames,
  laptopDeviceId,
  onCommit,
  showQuick = true,
  height = 340,
}: {
  screens: ScreenDto[];
  placements: PlacementDto[];
  thisDeviceId: string;
  deviceNames: Record<string, string>;
  /** Device whose group the quick buttons move (the laptop). */
  laptopDeviceId: string | null;
  onCommit: (p: PlacementDto[]) => void;
  showQuick?: boolean;
  height?: number;
}) {
  const t = useT();
  const wrapRef = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ w: 640, h: height });
  const [drag, setDrag] = useState<DragState | null>(null);
  const dragMeta = useRef<{ offX: number; offY: number } | null>(null);
  const dragRef = useRef<DragState | null>(null);
  const tiles = useMemo(() => buildTiles(screens, placements), [screens, placements]);

  useEffect(() => {
    const el = wrapRef.current;
    if (!el) return;
    const ro = new ResizeObserver(([e]) => {
      if (e) setSize({ w: Math.max(200, e.contentRect.width), h: Math.max(160, e.contentRect.height) });
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // View transform from the committed layout, so the scale does not jump while dragging.
  const view = useMemo(() => {
    if (tiles.length === 0) return { scale: 1, ox: 0, oy: 0 };
    const minX = Math.min(...tiles.map((t) => t.x));
    const minY = Math.min(...tiles.map((t) => t.y));
    const maxX = Math.max(...tiles.map((t) => t.x + t.w));
    const maxY = Math.max(...tiles.map((t) => t.y + t.h));
    const big = Math.max(...tiles.map((t) => Math.max(t.w, t.h)));
    const padX = big * 0.1;
    const padY = big * 0.22;
    const w = maxX - minX + padX * 2;
    const h = maxY - minY + padY * 2;
    const scale = Math.min(size.w / w, size.h / h);
    const ox = minX - padX - (size.w / scale - w) / 2;
    const oy = minY - padY - (size.h / scale - h) / 2;
    return { scale, ox, oy };
  }, [tiles, size]);

  const deviceOrder = useMemo(() => Array.from(new Set(tiles.map((t) => t.deviceId))), [tiles]);
  const threshold = 24 / view.scale / 4;

  const moveTo = useCallback(
    (tile: Tile, x: number, y: number): SnapResult => {
      const others = tiles.filter((o) => o.key !== tile.key);
      return snapTile(tile, { x, y }, others, Math.max(18, threshold));
    },
    [tiles, threshold],
  );

  const commitTile = (tile: Tile, x: number, y: number) => {
    const next = tiles.map((tl) => (tl.key === tile.key ? { ...tl, x, y } : tl));
    onCommit(toPlacements(next));
  };

  const onPointerDown = (e: React.PointerEvent, tile: Tile) => {
    if (e.button !== 0) return;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    const rect = wrapRef.current?.getBoundingClientRect();
    if (!rect) return;
    const px = (e.clientX - rect.left) / view.scale + view.ox;
    const py = (e.clientY - rect.top) / view.scale + view.oy;
    dragMeta.current = { offX: px - tile.x, offY: py - tile.y };
    const d0: DragState = { key: tile.key, x: tile.x, y: tile.y, snap: { x: tile.x, y: tile.y, attachedTo: null, side: null, align: null } };
    dragRef.current = d0;
    setDrag(d0);
  };

  const onPointerMove = (e: React.PointerEvent, tile: Tile) => {
    if (!drag || drag.key !== tile.key || !dragMeta.current) return;
    const rect = wrapRef.current?.getBoundingClientRect();
    if (!rect) return;
    const px = (e.clientX - rect.left) / view.scale + view.ox;
    const py = (e.clientY - rect.top) / view.scale + view.oy;
    const desired = { x: px - dragMeta.current.offX, y: py - dragMeta.current.offY };
    const snap = moveTo(tile, desired.x, desired.y);
    const d1: DragState = { key: tile.key, x: desired.x, y: desired.y, snap };
    dragRef.current = d1;
    setDrag(d1);
  };

  const onPointerUp = (_e: React.PointerEvent, tile: Tile) => {
    const d = dragRef.current;
    if (d && d.key === tile.key && (Math.round(d.snap.x) !== Math.round(tile.x) || Math.round(d.snap.y) !== Math.round(tile.y))) {
      commitTile(tile, Math.round(d.snap.x), Math.round(d.snap.y));
    }
    dragRef.current = null;
    dragMeta.current = null;
    setDrag(null);
  };

  const onKeyDown = (e: React.KeyboardEvent, tile: Tile) => {
    const step = e.shiftKey ? 100 : 20;
    const d: Record<string, [number, number]> = { ArrowLeft: [-step, 0], ArrowRight: [step, 0], ArrowUp: [0, -step], ArrowDown: [0, step] };
    const delta = d[e.key];
    if (!delta) return;
    e.preventDefault();
    const snap = moveTo(tile, tile.x + delta[0], tile.y + delta[1]);
    commitTile(tile, Math.round(snap.x), Math.round(snap.y));
  };

  // Quick buttons: place the laptop group next to the primary screen of the other device.
  const anchorTile = useMemo(() => {
    const others = tiles.filter((tl) => tl.deviceId !== laptopDeviceId);
    return others.find((tl) => tl.primary) ?? others[0] ?? null;
  }, [tiles, laptopDeviceId]);
  const side = laptopDeviceId && anchorTile ? currentSide(tiles, laptopDeviceId, anchorTile.key) : null;
  const quick = (s: QuickSide) => {
    if (!laptopDeviceId || !anchorTile) return;
    onCommit(toPlacements(arrangeGroup(tiles, laptopDeviceId, anchorTile.key, s)));
  };

  const live = (tile: Tile) => (drag && drag.key === tile.key ? { x: drag.snap.x, y: drag.snap.y } : { x: tile.x, y: tile.y });

  // Shared-edge guide while dragging.
  const guide = (() => {
    if (!drag || !drag.snap.attachedTo || !drag.snap.side) return null;
    const moving = tiles.find((tl) => tl.key === drag.key);
    const other = tiles.find((tl) => tl.key === drag.snap.attachedTo);
    if (!moving || !other) return null;
    const mx = drag.snap.x;
    const my = drag.snap.y;
    if (drag.snap.side === 'left' || drag.snap.side === 'right') {
      const top = Math.max(my, other.y);
      const bottom = Math.min(my + moving.h, other.y + other.h);
      const x = drag.snap.side === 'right' ? other.x + other.w : other.x;
      return { x, y: top, w: 0, h: bottom - top };
    }
    const left = Math.max(mx, other.x);
    const right = Math.min(mx + moving.w, other.x + other.w);
    const y = drag.snap.side === 'bottom' ? other.y + other.h : other.y;
    return { x: left, y, w: right - left, h: 0 };
  })();

  const laptopName = laptopDeviceId ? (deviceNames[laptopDeviceId] ?? t('common.laptop')) : t('common.laptop');

  return (
    <div className="layout-editor">
      <div
        ref={wrapRef}
        className="layout-editor__canvas"
        style={{ height }}
        role="group"
        aria-label={t('screens.canvas')}
      >
        {tiles.map((tile) => {
          const p = live(tile);
          const dragging = drag?.key === tile.key;
          const idx = deviceOrder.indexOf(tile.deviceId);
          const mine = tile.deviceId === thisDeviceId;
          const devName = deviceNames[tile.deviceId] ?? tile.deviceId;
          const isLaptop = tile.deviceId === laptopDeviceId;
          const wpx = tile.w * view.scale;
          const hpx = tile.h * view.scale;
          const small = wpx < 150 || hpx < 70;
          return (
            <div
              key={tile.key}
              role="button"
              tabIndex={0}
              aria-label={`${t('screens.tileOf', { screen: tile.name, device: devName })}, ${t('screens.tileInfo', { w: tile.px.w, h: tile.px.h, scale: tile.scalePct })}`}
              className={`tile tile--${idx % 2 === 0 ? 'a' : 'b'}${dragging ? ' is-dragging' : ''}${small ? ' tile--small' : ''}`}
              style={{
                width: wpx,
                height: hpx,
                transform: `translate(${(p.x - view.ox) * view.scale}px, ${(p.y - view.oy) * view.scale}px)`,
                transition: dragging ? 'none' : 'transform 160ms var(--ease)',
              }}
              onPointerDown={(e) => onPointerDown(e, tile)}
              onPointerMove={(e) => onPointerMove(e, tile)}
              onPointerUp={(e) => onPointerUp(e, tile)}
              onPointerCancel={() => {
                dragMeta.current = null;
                setDrag(null);
              }}
              onKeyDown={(e) => onKeyDown(e, tile)}
            >
              <span className="tile__icon">{isLaptop ? <Laptop size={small ? 16 : 22} aria-hidden /> : <Monitor size={small ? 16 : 22} aria-hidden />}</span>
              <span className="tile__name">{devName}</span>
              {!small && <span className="tile__screen">{tile.name}</span>}
              <span className="tile__res">
                {tile.px.w} × {tile.px.h}
                {!small && ` · ${tile.scalePct} %`}
              </span>
              {mine && !small && <span className="tile__me">{t('common.thisDevice')}</span>}
            </div>
          );
        })}
        {guide && (
          <div
            className="tile-guide"
            style={{
              transform: `translate(${(guide.x - view.ox) * view.scale}px, ${(guide.y - view.oy) * view.scale}px)`,
              width: Math.max(guide.w * view.scale, 3),
              height: Math.max(guide.h * view.scale, 3),
            }}
            aria-hidden
          />
        )}
      </div>
      {showQuick && laptopDeviceId && anchorTile && (
        <div className="layout-editor__quick" role="group" aria-label={`${t('screens.quick')}: ${laptopName}`}>
          <span className="layout-editor__quick-label">{t('screens.quick')}</span>
          {(['left', 'right', 'top', 'bottom'] as const).map((s) => (
            <Button
              key={s}
              size="sm"
              variant={side === s ? 'primary' : 'secondary'}
              aria-pressed={side === s}
              onClick={() => quick(s)}
              aria-label={`${laptopName}: ${t(`screens.side${s === 'left' ? 'Left' : s === 'right' ? 'Right' : s === 'top' ? 'Top' : 'Bottom'}` as const)}`}
            >
              {t(`screens.side${s === 'left' ? 'Left' : s === 'right' ? 'Right' : s === 'top' ? 'Top' : 'Bottom'}` as const)}
            </Button>
          ))}
        </div>
      )}
    </div>
  );
}

export { tileKey };
