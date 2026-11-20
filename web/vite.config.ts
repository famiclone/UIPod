import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vite';

const __dirname = dirname(fileURLToPath(import.meta.url));
const rawVersion =
  process.env.UIPOD_VERSION?.trim() ||
  readFileSync(resolve(__dirname, '../VERSION'), 'utf8').trim() ||
  'v0.5.0';
const appVersion =
  rawVersion.startsWith('v') || rawVersion.includes('-') ? rawVersion : `v${rawVersion}`;

export default defineConfig({
  base: './',
  define: {
    __UIPOD_VERSION__: JSON.stringify(appVersion),
  },
  plugins: [
    {
      name: 'uipod-html-version',
      transformIndexHtml(html) {
        return html.replaceAll('__UIPOD_VERSION__', appVersion);
      },
    },
  ],
  server: {
    port: 5173,
    fs: {
      allow: ['..'],
    },
  },
  build: {
    target: 'es2022',
    outDir: 'dist',
    assetsInlineLimit: 0,
  },
});
