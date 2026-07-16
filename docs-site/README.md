# Aether Documentation

The dual-mode unified documentation service for AetherIoT, covering
[AetherEdge](https://github.com/EvanL1/AetherEdge),
[AetherCloud](https://github.com/EvanL1/AetherCloud), and
[AetherContracts](https://github.com/EvanL1/AetherContracts). It is deployed at
[`docs.aetheriot.workers.dev`](https://docs.aetheriot.workers.dev).

- Browsers receive a searchable Astro + Starlight site.
- Agents can append `.md` or request `Accept: text/markdown`.
- Simplified Chinese is served from `/`; English is served from `/en/`.
- `/llms.txt` and `/llms-full.txt` provide the Chinese agent indexes.
- `/en/llms.txt` and `/en/llms-full.txt` provide the English agent indexes.

The English publication mirrors allowlisted product documentation from all
three repositories. The complete Chinese publication is maintained under
[`locales/zh-CN`](./locales/zh-CN) with an exact manifest so a new English page
cannot silently appear untranslated. Internal plans, ADRs, and competitive
analysis remain excluded. Mirrored English AetherCloud and AetherContracts
pages carry a direct link to their authoritative repository source. Chinese
contract specifications are reading aids; the tagged English release remains
normative.

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
