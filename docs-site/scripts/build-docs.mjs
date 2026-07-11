import { fileURLToPath } from 'node:url';
import path from 'node:path';
import fs from 'node:fs/promises';
import fg from 'fast-glob';
import { computeSlug } from './slug.mjs';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const DOCS_SITE_ROOT = path.resolve(__dirname, '..');
const CONTENT_DIR = path.join(DOCS_SITE_ROOT, 'src', 'content', 'docs');
const DIST_DIR = path.join(DOCS_SITE_ROOT, 'dist');
const DEFAULT_PUBLIC_BASE_URL = 'https://docs.aether-edge.workers.dev';

export function slugToOutputRelPath(slug) {
  return slug === '' ? 'index.md' : `${slug}.md`;
}

export function assertFilesFound(files) {
  if (files.length === 0) {
    throw new Error(
      'build-docs: no markdown files found under src/content/docs/ — did you run npm run sync?'
    );
  }
}

export function findOutputCollisions(pairs) {
  const sourcesByOutput = new Map();
  for (const [source, output] of pairs) {
    if (!sourcesByOutput.has(output)) sourcesByOutput.set(output, []);
    sourcesByOutput.get(output).push(source);
  }

  return [...sourcesByOutput.entries()]
    .filter(([, sources]) => sources.length > 1)
    .map(([outRelPath, sources]) => ({ outRelPath, sources }));
}

function parseFrontmatterScalar(value) {
  const trimmed = value.trim();
  if (trimmed.startsWith('"')) return JSON.parse(trimmed);
  if (trimmed.startsWith("'") && trimmed.endsWith("'")) return trimmed.slice(1, -1);
  return trimmed;
}

function firstParagraph(markdown) {
  const paragraphs = markdown.split(/\n\s*\n/);
  return (
    paragraphs.find((paragraph) => {
      const trimmed = paragraph.trim();
      return trimmed !== '' && !trimmed.startsWith('#') && !trimmed.startsWith('```');
    }) || ''
  )
    .replace(/\s+/g, ' ')
    .trim();
}

export function renderDocument(source) {
  const frontmatterMatch = source.match(/^---\n([\s\S]*?)\n---\n?([\s\S]*)$/);
  const metadata = frontmatterMatch?.[1] || '';
  const body = (frontmatterMatch?.[2] || source).trim();
  const titleMatch = metadata.match(/^title:\s*(.+)$/m);
  const bodyTitleMatch = body.match(/^#\s+(.+)$/m);
  const title = titleMatch
    ? parseFrontmatterScalar(titleMatch[1])
    : bodyTitleMatch?.[1]?.trim();

  if (!title) throw new Error('build-docs: every document must declare a title');

  const descriptionMatch = metadata.match(/^description:\s*(.+)$/m);
  const description = descriptionMatch
    ? parseFrontmatterScalar(descriptionMatch[1])
    : firstParagraph(body);
  const markdown = body.startsWith('# ')
    ? `${body}\n`
    : `# ${title}\n\n${body}${body ? '\n' : ''}`;

  return { title, description, markdown };
}

export function renderLlmsIndex(documents, publicBaseUrl) {
  const baseUrl = publicBaseUrl.replace(/\/$/, '');
  const entries = documents
    .map(({ slug, title, description }) => {
      const url = slug === '' ? `${baseUrl}/` : `${baseUrl}/${slug}`;
      return `- [${title}](${url})${description ? `: ${description}` : ''}`;
    })
    .join('\n');

  return [
    '# Aether Documentation',
    '',
    '> Pure Markdown documentation for the AI-native Aether IoT edge kernel and SDK.',
    '',
    entries,
    '',
  ].join('\n');
}

export function renderLlmsFull(documents) {
  return `${documents.map(({ markdown }) => markdown.trim()).join('\n\n---\n\n')}\n`;
}

/* v8 ignore start -- CLI filesystem orchestration is exercised by npm run build. */
async function main() {
  const files = (await fg('**/*.md', { cwd: CONTENT_DIR, onlyFiles: true })).sort();
  assertFilesFound(files);

  const pairs = files.map((relPath) => [
    relPath,
    slugToOutputRelPath(computeSlug(relPath)),
  ]);
  const collisions = findOutputCollisions(pairs);
  if (collisions.length > 0) {
    const details = collisions
      .map(({ outRelPath, sources }) => `  ${outRelPath} <- ${sources.join(', ')}`)
      .join('\n');
    throw new Error(`build-docs: output path collision(s) detected:\n${details}`);
  }

  const documents = await Promise.all(
    pairs.map(async ([relPath, outRelPath]) => {
      const source = await fs.readFile(path.join(CONTENT_DIR, relPath), 'utf8');
      const rendered = renderDocument(source);
      return {
        ...rendered,
        slug: computeSlug(relPath),
        outRelPath,
      };
    })
  );

  await fs.rm(DIST_DIR, { recursive: true, force: true });
  await Promise.all(
    documents.map(async ({ outRelPath, markdown }) => {
      const outputPath = path.join(DIST_DIR, outRelPath);
      await fs.mkdir(path.dirname(outputPath), { recursive: true });
      await fs.writeFile(outputPath, markdown, 'utf8');
    })
  );

  const publicBaseUrl = process.env.PUBLIC_BASE_URL || DEFAULT_PUBLIC_BASE_URL;
  await fs.writeFile(
    path.join(DIST_DIR, 'llms.txt'),
    renderLlmsIndex(documents, publicBaseUrl),
    'utf8'
  );
  await fs.writeFile(path.join(DIST_DIR, 'llms-full.txt'), renderLlmsFull(documents), 'utf8');

  console.log(`build-docs: wrote ${documents.length} Markdown documents and 2 text indexes`);
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main().catch((error) => {
    console.error(error);
    process.exitCode = 1;
  });
}
/* v8 ignore stop */
