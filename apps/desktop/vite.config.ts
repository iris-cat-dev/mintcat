import { defineConfig } from 'vite';

export default defineConfig({
  publicDir: false,
  server: { host: '127.0.0.1', port: 1430, strictPort: true },
});
