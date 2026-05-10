import babel from '@rolldown/plugin-babel';
import tailwindcss from '@tailwindcss/vite';
import { tanstackRouter } from '@tanstack/router-plugin/vite';
import react, { reactCompilerPreset } from '@vitejs/plugin-react';
import fs from 'fs';
import path from 'path';
import { defineConfig } from 'vite';

const DEFAULT_API_PROXY_TARGET = 'http://127.0.0.1:3000';
const BACKEND_CONFIG_PATH = path.resolve(
  __dirname,
  '../fork-chat-backend/config.json',
);

function parseBackendPort(serverAddr: unknown): string | null {
  if (typeof serverAddr !== 'string') {
    return null;
  }

  const match = serverAddr.trim().match(/:(\d+)$/);
  return match?.[1] ?? null;
}

function readBackendPortFromConfig(): string | null {
  try {
    const config = JSON.parse(fs.readFileSync(BACKEND_CONFIG_PATH, 'utf8')) as {
      server_addr?: unknown;
    };
    return parseBackendPort(config.server_addr);
  } catch {
    return null;
  }
}

function resolveApiProxyTarget(): string {
  const configPort = readBackendPortFromConfig();
  if (configPort) {
    return `http://127.0.0.1:${configPort}`;
  }

  return DEFAULT_API_PROXY_TARGET;
}

// https://vite.dev/config/
export default defineConfig({
  plugins: [
    tanstackRouter({
      target: 'react',
      autoCodeSplitting: true,
    }),
    react(),
    babel({ presets: [reactCompilerPreset()] }),
    tailwindcss(),
  ],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
    },
  },
  server: {
    proxy: {
      // Route API + SSE traffic through the backend during local frontend
      // development so the browser still talks to a single origin.
      '/api': {
        target: resolveApiProxyTarget(),
        changeOrigin: true,
      },
    },
  },
});
