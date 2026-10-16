import * as TooltipPrimitive from '@radix-ui/react-tooltip';
import { AnimatePresence, MotionConfig, motion } from 'motion/react';
import { Loader2, PlugZap } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import { getBackend } from './api';
import { Nav, PartnerUpdateDialogs, Titlebar, Toaster, WarningBanners } from './components/Shell';
import { useT } from './i18n/useT';
import { applyTheme } from './lib/theme';
import { Flyout } from './routes/Flyout';
import { Osd } from './routes/Osd';
import { About } from './screens/About';
import { Audio } from './screens/Audio';
import { Dashboard } from './screens/Dashboard';
import { Files } from './screens/Files';
import { Focus } from './screens/Focus';
import { Hotkeys } from './screens/Hotkeys';
import { Onboarding } from './screens/Onboarding';
import { Screens } from './screens/Screens';
import { Settings } from './screens/Settings';
import { ROUTES, useStore, type Route } from './store';

const SCREENS: Record<Route, () => React.ReactNode> = {
  dashboard: () => <Dashboard />,
  screens: () => <Screens />,
  audio: () => <Audio />,
  files: () => <Files />,
  focus: () => <Focus />,
  hotkeys: () => <Hotkeys />,
  settings: () => <Settings />,
  about: () => <About />,
};

function surface(): 'main' | 'osd' | 'flyout' {
  const h = location.hash;
  if (h.startsWith('#/osd')) return 'osd';
  if (h.startsWith('#/flyout')) return 'flyout';
  try {
    const label = (window as unknown as { __TAURI_INTERNALS__?: { metadata?: { currentWindow?: { label?: string } } } }).__TAURI_INTERNALS__?.metadata?.currentWindow?.label;
    if (label === 'osd' || label === 'flyout') return label;
  } catch {
    /* ignore */
  }
  return 'main';
}

function AgentOffline() {
  const t = useT();
  const conn = useStore((s) => s.conn);
  return (
    <div className="offline">
      <div className="offline__card">
        <span className="offline__icon">{conn.starting ? <Loader2 className="spin" size={30} aria-hidden /> : <PlugZap size={30} aria-hidden />}</span>
        <h1 className="t-h1">{t('agent.offlineTitle')}</h1>
        <p>{conn.starting ? t('agent.starting') : conn.start_failed ? t('agent.startFailed') : t('agent.offlineBody')}</p>
        {!conn.start_failed && <p className="muted">{t('agent.offlineBody')}</p>}
      </div>
    </div>
  );
}

/** Applies theme + language from the config to the document. */
function useDocumentSettings() {
  const theme = useStore((s) => s.config?.general.theme);
  const lang = useStore((s) => s.lang);
  useEffect(() => {
    document.documentElement.lang = lang;
  }, [lang]);
  useEffect(() => {
    if (!theme) return;
    applyTheme(theme);
    if (theme !== 'system') return;
    const mq = matchMedia('(prefers-color-scheme: dark)');
    const on = () => applyTheme('system', false);
    mq.addEventListener('change', on);
    return () => mq.removeEventListener('change', on);
  }, [theme]);
}

function MainShell() {
  const route = useStore((s) => s.route);
  const navigate = useStore((s) => s.navigate);
  const state = useStore((s) => s.state);
  const loaded = useStore((s) => s.loaded);
  const t = useT();
  const first = useRef(true);
  const readySent = useRef(false);
  const [, force] = useState(0);

  // Deep links for development: index.html#/audio
  useEffect(() => {
    const fromHash = () => {
      const r = location.hash.replace(/^#\/?/, '').split('?')[0] as Route;
      if (ROUTES.includes(r)) navigate(r);
    };
    fromHash();
    const on = () => {
      fromHash();
      force((n) => n + 1);
    };
    window.addEventListener('hashchange', on);
    return () => window.removeEventListener('hashchange', on);
  }, [navigate]);

  // Show the (initially hidden) native window only after the first paint: no white flash.
  useEffect(() => {
    if (!loaded || readySent.current) return;
    readySent.current = true;
    requestAnimationFrame(() => requestAnimationFrame(() => getBackend().frontendReady()));
  }, [loaded]);

  // Move focus to the page title on navigation (screen readers + keyboard users).
  useEffect(() => {
    if (first.current) {
      first.current = false;
      return;
    }
    document.getElementById('page-title')?.focus({ preventScroll: true });
  }, [route]);

  const onboarding = state != null && !state.device.onboarding_done;
  const Screen = SCREENS[route];

  return (
    <div className={`app${onboarding ? ' app--onb' : ''}`}>
      <Titlebar />
      {!loaded ? null : state == null ? (
        <AgentOffline />
      ) : onboarding ? (
        <Onboarding />
      ) : (
        <div className="app__body">
          <a className="skip-link" href="#main" onClick={(e) => { e.preventDefault(); document.getElementById('main')?.focus(); }}>
            {t('shell.skipToContent')}
          </a>
          <Nav />
          <main id="main" className="content" tabIndex={-1}>
            <div className="content__inner">
              <WarningBanners />
              <AnimatePresence mode="wait" initial={false}>
                <motion.div key={route} initial={{ opacity: 0, y: 8 }} animate={{ opacity: 1, y: 0 }} exit={{ opacity: 0, y: -4 }} transition={{ duration: 0.18, ease: 'easeOut' }}>
                  {Screen()}
                </motion.div>
              </AnimatePresence>
            </div>
          </main>
        </div>
      )}
      {state != null && !onboarding && <PartnerUpdateDialogs />}
      <Toaster />
    </div>
  );
}

export function App() {
  const init = useStore((s) => s.init);
  const kind = surface();
  useDocumentSettings();

  useEffect(() => {
    document.documentElement.dataset.surface = kind;
    if (kind === 'osd') return;
    return init();
  }, [init, kind]);

  return (
    <MotionConfig reducedMotion="user">
      <TooltipPrimitive.Provider delayDuration={350} skipDelayDuration={200}>
        {kind === 'osd' ? <Osd /> : kind === 'flyout' ? <Flyout /> : <MainShell />}
      </TooltipPrimitive.Provider>
    </MotionConfig>
  );
}
