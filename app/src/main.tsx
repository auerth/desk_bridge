import { createRoot } from 'react-dom/client';
import './brand/ui-tokens.css';
import './styles/app.css';
import { App } from './App';

// No <StrictMode>: effects start/cancel pairing on the agent and must not run twice in development.
createRoot(document.getElementById('root')!).render(<App />);
