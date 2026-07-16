# Aether Documentation

The dual-mode unified documentation service for AetherIoT, covering
[AetherEdge](https://github.com/EvanL1/AetherEdge),
[AetherCloud](https://github.com/EvanL1/AetherCloud), and
[AetherContracts](https://github.com/EvanL1/AetherContracts). It is deployed at
[`docs.aetheriot.workers.dev`](https://docs.aetheriot.workers.dev).

- Browsers receive a searchable Astro + Starlight site.
- Agents can append `.md` or request `Accept: text/markdown`.
- `/llms.txt` provides the curated document index.
- `/llms-full.txt` provides the complete published corpus.

Only English product documentation listed in
[`content.manifest.txt`](./content.manifest.txt) is published. Internal plans,
internal plans, ADRs, and competitive analysis are intentionally excluded.
Public migration guides listed in the manifest are part of the product docs.

```bash
npm ci
npm run check
npm run test:coverage
npm run test:worker
npm run build
npm run preview
```
