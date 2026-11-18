import { defineConfig } from 'vite';
import { fileURLToPath } from 'node:url';

export default defineConfig({
  publicDir: false,
  resolve: {
    alias: {
      '@mintcat/sdk-ui': fileURLToPath(new URL('../../packages/sdk-ui/src/index.ts', import.meta.url)),
      '@mintcat/sdk-ts': fileURLToPath(new URL('../../packages/sdk-ts/src/index.ts', import.meta.url)),
    },
    dedupe: ['react', 'react-dom', 'antd', '@ant-design/icons'],
  },
  server: { host: '127.0.0.1', port: 1430, strictPort: true },
});
