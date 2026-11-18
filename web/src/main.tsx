import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import './styles.css';
import './styles-light.css';
import './theme';
import { App } from './App';
import { installEscape } from './ui/escape';
import { installNumberWheel } from './ui/fields';
import { installTooltips } from './ui/tooltips';

installNumberWheel();
installTooltips();
installEscape();

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
