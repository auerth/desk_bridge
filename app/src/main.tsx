import { createRoot } from 'react-dom/client';
import './brand/ui-tokens.css';
import './styles/app.css';
import { App } from './App';
import { useStore } from './store';

// No <StrictMode>: effects start/cancel pairing on the agent and must not run twice in development.
createRoot(document.getElementById('root')!).render(<App />);

// Debug handle for the browser console (dev server only).
if (import.meta.env.DEV) (window as unknown as { __store: typeof useStore }).__store = useStore;
if (import.meta.env.DEV) useStore.subscribe((s, p) => { if (s.partnerOffer !== p.partnerOffer) console.log('partnerOffer', s.partnerOffer ? 'set' : 'cleared'); });
if (import.meta.env.DEV) void import('./api').then((m) => ((window as unknown as { __backend: unknown }).__backend = m.getBackend()));
