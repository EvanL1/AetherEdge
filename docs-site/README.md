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

Only English product documentation declared by
[`content.sources.json`](./content.sources.json) and its three source manifests
is published. Internal plans, ADRs, and competitive analysis are intentionally
excluded. Mirrored AetherCloud and AetherContracts pages carry a direct link to
their authoritative repository source.

Local development resolves AetherCloud and AetherContracts from sibling
checkouts by default. Set `AETHER_CLOUD_DOCS_ROOT` or
`AETHER_CONTRACTS_DOCS_ROOT` when the repositories live elsewhere. Deployment
checks out all three repositories and rebuilds daily so source documentation
changes do not leave the unified site permanently stale.

```bash
npm ci
npm run check
npm run test:coverage
npm run test:worker
npm run build
npm run preview
```
