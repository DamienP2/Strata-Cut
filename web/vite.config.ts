import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

const server = process.env.SC_SERVER ?? 'http://localhost:8080';
export default defineConfig({
  plugins: [react()],
  build: {
    assetsDir: 'static', // /assets is kept for files served by the server
    rolldownOptions: {
      output: {
        codeSplitting: {
          groups: [
            { name: 'react', test: /node_modules[\\/](react|react-dom|scheduler|zustand|use-sync-external-store)[\\/]/ },
            { name: 'icons', test: /node_modules[\\/]lucide-react[\\/]/ },
          ],
        },
      },
    },
  },
  server: { port: 5173, proxy: { '/api': server, '/ws': { target: server.replace(/^http/, 'ws'), ws: true } } },
});
