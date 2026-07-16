# AetherIoT Documentation

This directory publishes unified English documentation for AetherEdge,
AetherCloud, and AetherContracts through a dual-mode Cloudflare Worker.

Production URL: `https://docs.aetheriot.workers.dev`.

## Representations

- Browser requests receive the Astro + Starlight HTML site.
- A `.md` suffix or `Accept: text/markdown` receives the matching Markdown.
- `llms.txt` is the curated agent index.
- `llms-full.txt` is the complete published Markdown corpus.

HTML and Markdown are built from the same source set and must never diverge in
content scope.

## Public content boundary

`content.sources.json` declares the AetherEdge, AetherCloud, and
AetherContracts source repositories. Each source manifest is a publication
allowlist. Only English product documentation belongs in those manifests.
Public compatibility and operator migration guides are product documentation.
Do not publish internal agent instructions, plans, ADRs, competitive analysis,
or historical working notes.

`npm run sync` copies allowlisted Markdown from all three repositories into
`src/content/docs/`, namespaces non-Edge routes by product, rewrites relative
links, and marks cross-repository mirrors with their authoritative source.
Everything in that directory is generated except:

- `index.md`
- `agent-quickstart.md`

Edit generated content at its repository source. The next sync deletes edits
made directly to generated mirrors.

Local development expects sibling `AetherCloud` and `AetherContracts`
checkouts unless `AETHER_CLOUD_DOCS_ROOT` and `AETHER_CONTRACTS_DOCS_ROOT`
provide explicit roots. CI checks out all sources before synchronization.

## Build pipeline

1. Synchronize allowlisted Markdown.
2. Reject CJK characters in the complete publication set.
3. Build the Starlight HTML site.
4. Add Markdown twins, `llms.txt`, and `llms-full.txt` to `dist/`.

The language check is intentional: public Aether documentation is English-only.

## Worker contract

`worker/entry.js` performs representation selection. Normal requests pass to
the HTML assets. Markdown requests are rewritten to the corresponding `.md`
asset. Both representations set `Vary: Accept`.

Only `GET` and `HEAD` are allowed. Markdown lookup failures return typed plain
text responses and must never fall back to HTML.

## Verification

```bash
npm run check
npm run test:coverage
npm run test:worker
npm run build
test -f dist/index.html
test -f dist/index.md
test -f dist/llms.txt
```
