# Aether Plain-Text Documentation

A Cloudflare Worker that publishes Aether documentation as Markdown and plain
text. It has no HTML renderer, theme, browser UI, database, or search service.

Public endpoints:

- `/` — Markdown documentation entry point
- `/llms.txt` — compact document index with absolute links
- `/llms-full.txt` — all published documents in one text response
- `/<document>` and `/<document>.md` — the same Markdown document

Content is synchronized from the repository paths in
[`content.manifest.txt`](./content.manifest.txt). The only hand-authored entry
documents are `src/content/docs/index.md` and
`src/content/docs/agent-quickstart.md`.

```bash
npm ci
npm test
npm run test:worker
npm run build
npm run dev
```

`npm run build` deletes `dist/` before emitting Markdown, so stale HTML can
never survive a rebuild.
