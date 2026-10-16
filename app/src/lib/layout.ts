// Screen layout maths for the editor (UI side only; the agent owns the authoritative layout maths in
// `crates/input/src/layout.rs`). Layout space = 96-dpi normalised units: size = pixels * 96 / dpi.

import type { PlacementDto, ScreenDto } from '../api/types';

export interface Tile {
  key: string;
  deviceId: string;
  screenId: number;
  name: string;
  primary: boolean;
  /** Layout position (top-left) and size in 96-dpi units. */
  x: number;
  y: number;
  w: number;
  h: number;
  /** Original size in physical pixels and the Windows scale in percent. */
  px: { w: number; h: number };
  scalePct: number;
}

export const tileKey = (deviceId: string, screenId: number) => `${deviceId}#${screenId}`;

export function unitSize(px: number, dpi: number): number {
  return Math.round((px * 96) / Math.max(dpi, 48));
}

export function buildTiles(screens: ScreenDto[], placements: PlacementDto[]): Tile[] {
  const tiles: Tile[] = [];
  let fallbackX = 0;
  for (const s of screens) {
    const p = placements.find((pl) => pl.device_id === s.device_id && pl.screen_id === s.id);
    const w = unitSize(s.w, s.dpi);
    const h = unitSize(s.h, s.dpi);
    const x = p?.x ?? fallbackX;
    const y = p?.y ?? 0;
    fallbackX = x + w;
    tiles.push({
      key: tileKey(s.device_id, s.id),
      deviceId: s.device_id,
      screenId: s.id,
      name: s.name,
      primary: s.primary,
      x,
      y,
      w,
      h,
      px: { w: s.w, h: s.h },
      scalePct: Math.round((s.dpi / 96) * 100),
    });
  }
  return tiles;
}

export function toPlacements(tiles: Tile[]): PlacementDto[] {
  return tiles.map((t) => ({ device_id: t.deviceId, screen_id: t.screenId, x: Math.round(t.x), y: Math.round(t.y) }));
}

interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

function overlaps(a: Rect, b: Rect): boolean {
  return a.x < b.x + b.w && a.x + a.w > b.x && a.y < b.y + b.h && a.y + a.h > b.y;
}

const clamp = (v: number, lo: number, hi: number) => Math.min(Math.max(v, lo), hi);

export interface SnapResult {
  x: number;
  y: number;
  /** Tile the dragged one is attached to (null = free, e.g. only one tile). */
  attachedTo: string | null;
  side: 'left' | 'right' | 'top' | 'bottom' | null;
  /** Edge alignment that was snapped to along the shared edge. */
  align: 'start' | 'center' | 'end' | null;
}

/**
 * Resolves a desired top-left position for `moving` into a legal one: the tile always touches another
 * tile edge to edge (no overlap, no gap) and may slide along that edge with an offset. Edges, centres
 * of the neighbour snap when closer than `threshold` units.
 */
export function snapTile(moving: Tile, desired: { x: number; y: number }, others: Tile[], threshold: number): SnapResult {
  if (others.length === 0) return { ...desired, attachedTo: null, side: null, align: null };

  let best: (SnapResult & { dist: number }) | null = null;
  for (const o of others) {
    const minOverlap = Math.max(40, Math.min(moving.w, moving.h, o.w, o.h) * 0.2);
    const candidates: { x: number; y: number; side: SnapResult['side']; axis: 'x' | 'y' }[] = [
      { x: o.x + o.w, y: clamp(desired.y, o.y - moving.h + minOverlap, o.y + o.h - minOverlap), side: 'right', axis: 'y' },
      { x: o.x - moving.w, y: clamp(desired.y, o.y - moving.h + minOverlap, o.y + o.h - minOverlap), side: 'left', axis: 'y' },
      { x: clamp(desired.x, o.x - moving.w + minOverlap, o.x + o.w - minOverlap), y: o.y + o.h, side: 'bottom', axis: 'x' },
      { x: clamp(desired.x, o.x - moving.w + minOverlap, o.x + o.w - minOverlap), y: o.y - moving.h, side: 'top', axis: 'x' },
    ];
    for (const c of candidates) {
      let { x, y } = c;
      let align: SnapResult['align'] = null;
      // Snap the offset along the shared edge to start / centre / end of the neighbour.
      const along = c.axis;
      const cur = along === 'x' ? x : y;
      const oStart = along === 'x' ? o.x : o.y;
      const oSize = along === 'x' ? o.w : o.h;
      const mSize = along === 'x' ? moving.w : moving.h;
      const snaps: [number, SnapResult['align']][] = [
        [oStart, 'start'],
        [oStart + (oSize - mSize) / 2, 'center'],
        [oStart + oSize - mSize, 'end'],
      ];
      let bestSnap: [number, SnapResult['align']] | null = null;
      for (const s of snaps) {
        if (Math.abs(s[0] - cur) <= threshold && (!bestSnap || Math.abs(s[0] - cur) < Math.abs(bestSnap[0] - cur))) bestSnap = s;
      }
      if (bestSnap) {
        if (along === 'x') x = bestSnap[0];
        else y = bestSnap[0];
        align = bestSnap[1];
      }
      const rect = { x, y, w: moving.w, h: moving.h };
      if (others.some((other) => overlaps(rect, other))) continue;
      const dist = Math.hypot(x - desired.x, y - desired.y);
      if (!best || dist < best.dist) best = { x, y, attachedTo: o.key, side: c.side, align, dist };
    }
  }
  if (!best) return { ...desired, attachedTo: null, side: null, align: null };
  return { x: best.x, y: best.y, attachedTo: best.attachedTo, side: best.side, align: best.align };
}

export type QuickSide = 'left' | 'right' | 'top' | 'bottom';

/** Moves all tiles of `deviceId` so that the group sits on `side` of the anchor tile (centred). */
export function arrangeGroup(tiles: Tile[], movingDevice: string, anchorKey: string, side: QuickSide): Tile[] {
  const anchor = tiles.find((t) => t.key === anchorKey);
  const group = tiles.filter((t) => t.deviceId === movingDevice);
  if (!anchor || group.length === 0) return tiles;
  const gx = Math.min(...group.map((t) => t.x));
  const gy = Math.min(...group.map((t) => t.y));
  const gw = Math.max(...group.map((t) => t.x + t.w)) - gx;
  const gh = Math.max(...group.map((t) => t.y + t.h)) - gy;
  let nx = anchor.x;
  let ny = anchor.y;
  switch (side) {
    case 'right':
      nx = anchor.x + anchor.w;
      ny = anchor.y + (anchor.h - gh) / 2;
      break;
    case 'left':
      nx = anchor.x - gw;
      ny = anchor.y + (anchor.h - gh) / 2;
      break;
    case 'bottom':
      nx = anchor.x + (anchor.w - gw) / 2;
      ny = anchor.y + anchor.h;
      break;
    case 'top':
      nx = anchor.x + (anchor.w - gw) / 2;
      ny = anchor.y - gh;
      break;
  }
  const dx = Math.round(nx - gx);
  const dy = Math.round(ny - gy);
  return tiles.map((t) => (t.deviceId === movingDevice ? { ...t, x: t.x + dx, y: t.y + dy } : t));
}

/** Which side of the anchor the group currently sits on (for highlighting the quick buttons). */
export function currentSide(tiles: Tile[], movingDevice: string, anchorKey: string): QuickSide | null {
  const anchor = tiles.find((t) => t.key === anchorKey);
  const group = tiles.filter((t) => t.deviceId === movingDevice);
  if (!anchor || group.length === 0) return null;
  const gx = Math.min(...group.map((t) => t.x));
  const gy = Math.min(...group.map((t) => t.y));
  const gr = Math.max(...group.map((t) => t.x + t.w));
  const gb = Math.max(...group.map((t) => t.y + t.h));
  if (Math.abs(gx - (anchor.x + anchor.w)) < 2) return 'right';
  if (Math.abs(gr - anchor.x) < 2) return 'left';
  if (Math.abs(gy - (anchor.y + anchor.h)) < 2) return 'bottom';
  if (Math.abs(gb - anchor.y) < 2) return 'top';
  return null;
}
