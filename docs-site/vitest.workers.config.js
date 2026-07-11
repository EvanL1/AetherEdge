// Separate Vitest config for the Cloudflare Worker (docs-site/worker/entry.js).
// Kept apart from the default `npm test` config because @cloudflare/vitest-pool-workers
// runs tests inside the actual `workerd` runtime, which is incompatible with the plain
// Node environment used by scripts/*.test.mjs. Run via `npm run test:worker`.
import { cloudflareTest } from '@cloudflare/vitest-pool-workers';
import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    include: ['worker/**/*.test.js'],
  },
  plugins: [
    cloudflareTest({
      main: './worker/entry.js',
      miniflare: {
        compatibilityDate: '2026-07-11',
        // Small hand-authored fixture set standing in for the real `dist/` build
        // output, so these tests don't depend on (or break from changes to) the
        // actual site content.
        assets: { directory: './worker/test-fixtures', binding: 'ASSETS' },
      },
    }),
  ],
});
