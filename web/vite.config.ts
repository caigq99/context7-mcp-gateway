import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

const backend = process.env.GATEWAY_DEV_PROXY_TARGET ?? 'http://127.0.0.1:8787';

export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      '/auth': backend,
      '/api': backend,
      '/mcp': backend,
      '/health': backend,
    },
  },
});
