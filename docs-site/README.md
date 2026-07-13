# Aether Documentation

The dual-mode documentation service for
[AetherIot](https://github.com/EvanL1/AetherIot), deployed at
[`docs.aetheriot.workers.dev`](https://docs.aetheriot.workers.dev).

- Browsers receive a searchable Astro + Starlight site.
- Agents can append `.md` or request `Accept: text/markdown`.
- `/llms.txt` provides the curated document index.
- `/llms-full.txt` provides the complete published corpus.

Only English product documentation listed in
[`content.manifest.txt`](./content.manifest.txt) is published. Internal plans,
ADRs, migration notes, and competitive analysis are intentionally excluded.

```bash
npm ci
npm run check
npm run test:coverage
npm run test:worker
npm run build
npm run preview
```
