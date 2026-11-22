import React from 'react';
import { createRoot } from 'react-dom/client';
import { LanguageProvider } from './i18n';
import { App } from './shell';
import './style.css';

const rootElement = document.getElementById('root');
if (!rootElement) {
  throw new Error('Missing application root element');
}

createRoot(rootElement).render(
  <React.StrictMode>
    <LanguageProvider><App /></LanguageProvider>
  </React.StrictMode>,
);
