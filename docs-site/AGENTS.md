# AetherIoT Documentation

This directory publishes unified Simplified Chinese and English documentation
for AetherEdge, AetherCloud, and AetherContracts through a dual-mode Cloudflare
Worker. Chinese is the root locale and English is served from `/en/`.

Production URL: `https://docs.aetheriot.workers.dev`.

## Representations

- Browser requests receive the Astro + Starlight HTML site.
- A `.md` suffix or `Accept: text/markdown` receives the matching Markdown.
- `llms.txt` is the Chinese agent index.
- `en/llms.txt` is the English agent index.
- Each index covers every published page exactly once under the agent task,
  deployment and operations, safety and governance, recovery, platform
  reference, compatibility and status, or optional section.
- Never generate or publish a concatenated full-corpus text file. Agents must
  discover pages through the compact index and fetch Markdown on demand.

HTML and Markdown are built from the same source set and must never diverge in
content scope.

## Public content boundary

`content.sources.json` declares the three product repositories plus site-owned
English and Chinese content. Every source manifest is a publication allowlist.
Public compatibility, operator migration, and fail-closed recovery runbooks
are product documentation.
Do not publish internal agent instructions, plans, ADRs, competitive analysis,
or historical working notes.

`npm run sync` copies allowlisted Markdown into `src/content/docs/`. English
product sources are written under `en/`; Chinese sources are written at the
root locale. The sync namespaces non-Edge routes by product, rewrites relative
links, and marks cross-repository English mirrors with their authoritative
source. Everything in `src/content/docs/` is generated. Edit English product
content in its authoritative repository, English site pages under
`locales/en/`, and Chinese pages under `locales/zh-CN/`.

Every published cross-document link must be a complete absolute URL. Public
pages use `https://docs.aetheriot.workers.dev`, with `/en/` for English and the
root path for Simplified Chinese. Excluded repository documents and source
artifacts use an absolute GitHub URL. Same-page fragment links may remain
relative.

Chinese pages must preserve product names, protocol identifiers, code, paths,
and command names. AetherContracts translations must state that the tagged
English specification, Schema, fixtures, and TCK remain normative.
For Chinese bold labels, keep punctuation outside the emphasis marker, for
example `**成功标准**：运行命令`; `**成功标准：**运行命令` is ambiguous
under CommonMark and renders as literal asterisks.

Local development expects sibling `AetherCloud` and `AetherContracts`
checkouts unless `AETHER_CLOUD_DOCS_ROOT` and `AETHER_CONTRACTS_DOCS_ROOT`
provide explicit roots. CI checks out all sources before synchronization.

## Build pipeline

1. Synchronize allowlisted Markdown.
2. Reject CJK characters in `/en/` and reject untranslated root-locale pages.
3. Build the Starlight HTML site.
4. Add Markdown twins and one compact agent index per locale to `dist/`.

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
test -f dist/en/index.html
test -f dist/en/llms.txt
```
