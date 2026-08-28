import { defineConfig } from 'vite';

// https://vitejs.dev/config
export default defineConfig({
  define: {
    'process.env.GITHUB_OWNER': JSON.stringify(process.env.GITHUB_OWNER || 'ersensari'),
    'process.env.GITHUB_REPO': JSON.stringify(process.env.GITHUB_REPO || 'esi-harness'),
    'process.env.GOOSE_BUNDLE_NAME': JSON.stringify(process.env.GOOSE_BUNDLE_NAME || 'ESI-Studio'),
  },
});
