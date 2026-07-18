import { fileURLToPath } from 'node:url';
import path from 'node:path';
import fs from 'node:fs/promises';
import fg from 'fast-glob';
import { fromMarkdown } from 'mdast-util-from-markdown';
import { computeSlug } from './slug.mjs';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const CONTENT_DIR = path.resolve(__dirname, '..', 'src', 'content', 'docs');
const DOCUMENTATION_ORIGIN = 'https://docs.aetheriot.workers.dev';
const INTERNAL_REFERENCE_PATTERN = /\bADR-\d{4}\b|(?:^|[/])docs\/adr\//i;
const MARKDOWN_LINK_PATTERN = /!?\[([^\]]*)\]\(([^)\s]+)(?:\s+[^)]*)?\)/g;
const ABSOLUTE_LINK_PATTERN = /^(?:https?:\/\/|mailto:|tel:|#)/i;
const LANGUAGE_SWITCH_PATTERN = /\b(?:English|Chinese|Simplified Chinese)\b|英文|中文|简体中文/i;

export function findInternalArchitectureReferences(sourcePath, content) {
  return content
    .split('\n')
    .map((text, index) => ({ path: sourcePath, line: index + 1, text }))
    .filter(({ text }) => INTERNAL_REFERENCE_PATTERN.test(text));
}

export function findUnrenderedMarkdownEmphasis(sourcePath, content) {
  const findings = [];

  function visit(node) {
    if (node.type === 'text' && node.value.includes('**')) {
      const startLine = node.position?.start.line ?? 1;
      for (const [offset, text] of node.value.split('\n').entries()) {
        if (!text.includes('**')) continue;
        findings.push({
          path: sourcePath,
          line: startLine + offset,
          text: text.trim(),
        });
      }
    }
    if (!Array.isArray(node.children)) return;
    for (const child of node.children) visit(child);
  }

  visit(fromMarkdown(content));
  return findings;
}

export function findNonAbsoluteLinks(sourcePath, content) {
  return content
    .split('\n')
    .flatMap((text, index) =>
      [...text.matchAll(MARKDOWN_LINK_PATTERN)]
        .map((match) => match[2])
        .filter((target) => !ABSOLUTE_LINK_PATTERN.test(target))
        .map((target) => ({ path: sourcePath, line: index + 1, target }))
    );
}

function normalizePublishedRoute(pathname) {
  let route = decodeURIComponent(pathname).replace(/\/+$/, '');
  if (route.endsWith('/index.md')) route = route.slice(0, -'/index.md'.length);
  else if (route.endsWith('.md')) route = route.slice(0, -'.md'.length);
  return route || '/';
}

export function findPublishedLinkViolations(documents) {
  const publishedRoutes = new Set(
    documents.map(({ path: sourcePath }) => {
      const slug = computeSlug(sourcePath);
      return slug === '' ? '/' : `/${slug}`;
    })
  );

  return documents.flatMap(({ path: sourcePath, content }) => {
    const sourceIsEnglish = sourcePath === 'en/index.md' || sourcePath.startsWith('en/');
    return content.split('\n').flatMap((lineText, index) =>
      [...lineText.matchAll(MARKDOWN_LINK_PATTERN)].flatMap((match) => {
        const [, text, target] = match;
        let url;
        try {
          url = new URL(target);
        } catch {
          return [];
        }
        if (url.origin !== DOCUMENTATION_ORIGIN) return [];

        const route = normalizePublishedRoute(url.pathname);
        if (!publishedRoutes.has(route)) {
          return [
            {
              kind: 'missing-route',
              path: sourcePath,
              line: index + 1,
              text,
              target,
            },
          ];
        }

        const targetIsEnglish = route === '/en' || route.startsWith('/en/');
        if (
          sourceIsEnglish !== targetIsEnglish &&
          !LANGUAGE_SWITCH_PATTERN.test(text)
        ) {
          return [
            {
              kind: 'wrong-locale',
              path: sourcePath,
              line: index + 1,
              text,
              target,
            },
          ];
        }
        return [];
      })
    );
  });
}

export function assertUserFacingDocumentation(documents) {
  const references = documents.flatMap(({ path: sourcePath, content }) =>
    findInternalArchitectureReferences(sourcePath, content)
  );
  if (references.length > 0) {
    const details = references
      .map(({ path: sourcePath, line, text }) => `  ${sourcePath}:${line}: ${text.trim()}`)
      .join('\n');
    throw new Error(`Public documentation contains maintainer-only architecture references:\n${details}`);
  }

  const unrenderedEmphasis = documents.flatMap(
    ({ path: sourcePath, content }) =>
      findUnrenderedMarkdownEmphasis(sourcePath, content)
  );
  if (unrenderedEmphasis.length > 0) {
    const emphasisDetails = unrenderedEmphasis
      .map(
        ({ path: sourcePath, line, text }) =>
          `  ${sourcePath}:${line}: ${text}`
      )
      .join('\n');
    throw new Error(
      `Public documentation contains unrendered Markdown emphasis:\n${emphasisDetails}`
    );
  }

  const nonAbsoluteLinks = documents.flatMap(({ path: sourcePath, content }) =>
    findNonAbsoluteLinks(sourcePath, content)
  );
  if (nonAbsoluteLinks.length > 0) {
    const linkDetails = nonAbsoluteLinks
      .map(
        ({ path: sourcePath, line, target }) =>
          `  ${sourcePath}:${line}: ${target}`
      )
      .join('\n');
    throw new Error(
      `Public documentation links must use an absolute documentation URL or repository URL:\n${linkDetails}`
    );
  }

  const publishedLinkViolations = findPublishedLinkViolations(documents);
  if (publishedLinkViolations.length === 0) return;

  const missingRoutes = publishedLinkViolations.filter(
    ({ kind }) => kind === 'missing-route'
  );
  if (missingRoutes.length > 0) {
    const missingDetails = missingRoutes
      .map(
        ({ path: sourcePath, line, target }) =>
          `  ${sourcePath}:${line}: ${target}`
      )
      .join('\n');
    throw new Error(
      `Absolute documentation URL does not resolve to a published documentation route:\n${missingDetails}`
    );
  }

  const localeDetails = publishedLinkViolations
    .map(
      ({ path: sourcePath, line, target }) =>
        `  ${sourcePath}:${line}: ${target}`
    )
    .join('\n');
  throw new Error(
    `Absolute documentation URL points to the wrong documentation locale:\n${localeDetails}`
  );
}

/* v8 ignore start -- filesystem orchestration is exercised by npm run check. */
async function main() {
  const files = (await fg('**/*.md', { cwd: CONTENT_DIR, onlyFiles: true })).sort();
  const documents = await Promise.all(
    files.map(async (sourcePath) => ({
      path: sourcePath,
      content: await fs.readFile(path.join(CONTENT_DIR, sourcePath), 'utf8'),
    }))
  );
  assertUserFacingDocumentation(documents);
  console.log(`check-audience: verified ${documents.length} user-facing documents`);
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main().catch((error) => {
    console.error(error);
    process.exitCode = 1;
  });
}
/* v8 ignore stop */
