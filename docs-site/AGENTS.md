# Aether Plain-Text Documentation

This directory publishes Aether documentation for AI agents through a
Cloudflare Worker. It is deliberately not a website: production responses
must be Markdown or plain text, never HTML.

Production URL: `https://docs.aether-edge.workers.dev`.

## Content ownership

`content.manifest.txt` is the publication allowlist. `npm run sync` copies
matching repository Markdown into `src/content/docs/` and rewrites relative
links to published document routes or GitHub source URLs.

Everything under `src/content/docs/` is generated except:

- `index.md`
- `agent-quickstart.md`

Edit the original repository document for generated content. Do not edit a
generated mirror because the next sync deletes it.

## Build contract

`npm run build` performs two steps:

1. Synchronize allowlisted Markdown.
2. Delete `dist/`, strip build-only frontmatter, emit one `.md` file per
   document, then generate `llms.txt` and `llms-full.txt`.

The build must not emit `.html`, JavaScript, CSS, images, or framework assets.
Astro, Starlight, and browser-oriented documentation dependencies are not
allowed.

## Worker contract

`worker/entry.js` maps root, extensionless, trailing-slash, and direct `.md`
routes to Markdown assets. `.txt` indexes use `text/plain`. Missing documents,
unsupported methods, and infrastructure failures also return plain text.

No request header may cause an HTML response. `GET` and `HEAD` are the only
allowed methods.

## Verification

```bash
npm test
npm run test:worker
npm run build
find dist -type f ! -name '*.md' ! -name '*.txt'
```

The final `find` command must print nothing.
