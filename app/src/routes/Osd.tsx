import { AnimatePresence, motion } from 'motion/react';
import { AlertTriangle, Clipboard, File, Lock, LockOpen, Mic, MicOff, Volume2, VolumeX, Maximize2 } from 'lucide-react';
import { useEffect, useRef, useState, type ReactNode } from 'react';
import { getBackend } from '../api';
import { BrandIcon } from '../components/ui';

const ICONS: Record<string, ReactNode> = {
  laptop: <BrandIcon kind="laptop" size={30} />,
  monitor: <BrandIcon kind="monitor" size={30} />,
  pc: <BrandIcon kind="monitor" size={30} />,
  link: <BrandIcon kind="link" size={30} />,
  mic: <Mic size={20} aria-hidden />,
  'mic-off': <MicOff size={20} aria-hidden />,
  volume: <Volume2 size={20} aria-hidden />,
  'volume-off': <VolumeX size={20} aria-hidden />,
  focus: <Maximize2 size={20} aria-hidden />,
  file: <File size={20} aria-hidden />,
  files: <File size={20} aria-hidden />,
  clipboard: <Clipboard size={20} aria-hidden />,
  lock: <Lock size={20} aria-hidden />,
  unlock: <LockOpen size={20} aria-hidden />,
  warning: <AlertTriangle size={20} aria-hidden />,
};

/** The OSD window: a short pill ("→ Laptop") that fades out; the native window is click-through. */
export function Osd() {
  const [item, setItem] = useState<{ icon: string; text: string; id: number } | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    document.documentElement.dataset.surface = 'osd';
    const show = (p: { icon: string; text: string }) => {
      setItem({ ...p, id: Date.now() });
      if (timer.current) clearTimeout(timer.current);
      timer.current = setTimeout(() => setItem(null), 600);
    };
    const un = getBackend().onOsd(show);
    // Browser preview (`#/osd?demo`): cycle through a few pills.
    let demo: ReturnType<typeof setInterval> | undefined;
    if (new URLSearchParams(location.search).has('demo') || location.hash.includes('demo')) {
      const samples = [
        { icon: 'laptop', text: '→ Laptop' },
        { icon: 'mic-off', text: 'Mikro stumm' },
        { icon: 'focus', text: 'Fokusmodus an' },
      ];
      let i = 0;
      show(samples[0]!);
      demo = setInterval(() => show(samples[++i % samples.length]!), 1600);
    }
    return () => {
      un();
      if (demo) clearInterval(demo);
      if (timer.current) clearTimeout(timer.current);
    };
  }, []);

  return (
    <div className="osd-root">
      <AnimatePresence>
        {item && (
          <motion.div
            key={item.id}
            className="osd-pill"
            role="status"
            initial={{ opacity: 0, y: -10, scale: 0.96 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, y: -6, scale: 0.98 }}
            transition={{ duration: 0.16, ease: 'easeOut' }}
          >
            <span className="osd-pill__icon">{ICONS[item.icon] ?? <BrandIcon kind="link" size={30} />}</span>
            <span className="osd-pill__text">{item.text}</span>
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}
