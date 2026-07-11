// Default Vitest config for the plain-Node test suite (scripts/*.test.mjs).
// worker/**/*.test.js is excluded here because it runs under a separate pool
// (@cloudflare/vitest-pool-workers, see vitest.workers.config.js / `npm run test:worker`)
// that this default Node environment can't execute (it needs the `workerd` runtime).
import { configDefaults, defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    exclude: [...configDefaults.exclude, 'worker/**'],
  },
});
