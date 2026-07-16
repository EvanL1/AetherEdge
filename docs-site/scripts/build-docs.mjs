import { fileURLToPath } from 'node:url';
import path from 'node:path';
import fs from 'node:fs/promises';
import fg from 'fast-glob';
import { computeSlug } from './slug.mjs';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const DOCS_SITE_ROOT = path.resolve(__dirname, '..');
const CONTENT_DIR = path.join(DOCS_SITE_ROOT, 'src', 'content', 'docs');
const DIST_DIR = path.join(DOCS_SITE_ROOT, 'dist');
const DEFAULT_PUBLIC_BASE_URL = 'https://docs.aetheriot.workers.dev';

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

export function assertHtmlBuildPresent(found) {
  if (!found) {
    throw new Error('build-docs: HTML build is missing — run astro build before emitting agent docs');
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
  const sections = [
    ['Overview', ({ slug }) => slug.startsWith('overview/')],
    [
      'AetherEdge',
      ({ slug }) =>
        slug === 'aetheredge' ||
        slug.startsWith('aetheredge/') ||
        slug === 'agent-quickstart' ||
        slug.startsWith('concepts/') ||
        slug.startsWith('guides/') ||
        slug.startsWith('reference/') ||
        slug.startsWith('crates/') ||
        slug.startsWith('extensions/') ||
        slug.startsWith('security/'),
    ],
    ['AetherCloud', ({ slug }) => slug === 'aethercloud' || slug.startsWith('aethercloud/')],
    [
      'AetherContracts',
      ({ slug }) => slug === 'aethercontracts' || slug.startsWith('aethercontracts/'),
    ],
    ['Tutorials', ({ slug }) => slug.startsWith('tutorials/')],
    ['Compatibility', ({ slug }) => slug.startsWith('compatibility/')],
    ['Roadmap', ({ slug }) => slug.startsWith('roadmap/')],
    ['Migration', ({ slug }) => slug.startsWith('migration/')],
  ];
  const remaining = documents.filter(({ slug }) => slug !== '');
  const renderedSections = [];

  for (const [label, matches] of sections) {
    const matched = remaining.filter(matches);
    if (matched.length === 0) continue;
    renderedSections.push(`## ${label}`);
    renderedSections.push('');
    renderedSections.push(
      matched
        .map(({ slug, title, description }) => {
          const url = `${baseUrl}/${slug}`;
          return `- [${title}](${url})${description ? `: ${description}` : ''}`;
        })
        .join('\n')
    );
    renderedSections.push('');
    for (const document of matched) {
      const index = remaining.indexOf(document);
      if (index !== -1) remaining.splice(index, 1);
    }
  }

  if (remaining.length > 0) {
    renderedSections.push('## More');
    renderedSections.push('');
    renderedSections.push(
      remaining
        .map(({ slug, title, description }) =>
          `- [${title}](${baseUrl}/${slug})${description ? `: ${description}` : ''}`
        )
        .join('\n')
    );
    renderedSections.push('');
  }

  return [
    '# AetherIoT',
    '',
    '> Open-source edge, cloud, and interoperability building blocks for reliable IoT systems.',
    '',
    'Documentation pages are available as Markdown. Append `.md` to any document URL or send `Accept: text/markdown`.',
    '',
    ...renderedSections,
    '',
  ].join('\n');
}

export function renderLlmsFull(documents) {
  return `${documents.map(({ markdown }) => markdown.trim()).join('\n\n---\n\n')}\n`;
}

/* v8 ignore start -- CLI filesystem orchestration is exercised by npm run build. */
async function main() {
  let htmlBuildPresent = true;
  try {
    await fs.access(path.join(DIST_DIR, 'index.html'));
  } catch {
    htmlBuildPresent = false;
  }
  assertHtmlBuildPresent(htmlBuildPresent);

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

  console.log(`build-docs: added ${documents.length} Markdown twins and 2 text indexes`);
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main().catch((error) => {
    console.error(error);
    process.exitCode = 1;
  });
}
/* v8 ignore stop */
