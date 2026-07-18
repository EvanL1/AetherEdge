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
const FORBIDDEN_CONCATENATED_CORPUS = ['llms', 'full.txt'].join('-');

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

export function findHtmlHeadingViolations(pages) {
  return pages.flatMap(({ path: pagePath, html }) => {
    const headingCount = (html.match(/<h1(?:\s|>)/gi) || []).length;
    return headingCount === 1 ? [] : [{ path: pagePath, headingCount }];
  });
}

export function findLocalizedUiViolations(pages) {
  const unexpectedByLocale = {
    'zh-CN': [
      'title="Copy to clipboard"',
      'data-copied="Copied!"',
      '>Terminal window<',
      '>Previous<',
      '>Next<',
      'Section titled',
    ],
    en: [
      'title="复制到剪贴板"',
      'data-copied="已复制！"',
      '>终端窗口<',
      '>上一页<',
      '>下一页<',
      '标题的链接',
    ],
  };

  return pages.flatMap(({ path: pagePath, html }) => {
    const locale = pagePath.startsWith('en/') ? 'en' : 'zh-CN';
    return unexpectedByLocale[locale]
      .filter((text) => html.includes(text))
      .map((text) => ({ path: pagePath, locale, text }));
  });
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

export function partitionDocumentsByLocale(documents) {
  const partitions = { 'zh-CN': [], en: [] };
  for (const document of documents) {
    if (document.slug === 'en' || document.slug.startsWith('en/')) {
      partitions.en.push({
        ...document,
        publicSlug: document.slug,
        slug: document.slug === 'en' ? '' : document.slug.slice('en/'.length),
      });
    } else {
      partitions['zh-CN'].push({ ...document, publicSlug: document.slug });
    }
  }
  return partitions;
}

const LLMS_SECTION_KEYS = [
  'agent-tasks',
  'operations',
  'safety',
  'recovery',
  'reference',
  'status',
  'optional',
];

export function inferLlmsSection({ slug }) {
  const normalized = `/${slug.toLowerCase()}/`;

  if (
    normalized.includes('/recovery/') ||
    /\/(recover|rollback|reconnect|restore|revocation)(?:[-/])/.test(normalized)
  ) {
    return 'recovery';
  }
  if (
    normalized.includes('/compatibility/') ||
    normalized.includes('/roadmap/') ||
    normalized.includes('/status/') ||
    normalized.includes('/current-state-audit/') ||
    normalized.includes('/capability-map/')
  ) {
    return 'status';
  }
  if (
    normalized.includes('/security/') ||
    normalized.includes('/safe-operations/') ||
    normalized.includes('/governance/') ||
    normalized.includes('/governed-control/') ||
    normalized.includes('/control-authority/')
  ) {
    return 'safety';
  }
  if (
    normalized.includes('/agent-quickstart/') ||
    normalized.includes('/deployment/') ||
    /\/deploy(?:ment)?(?:[-/])/.test(normalized) ||
    normalized.includes('/migration/') ||
    normalized.includes('/configuration/') ||
    normalized.includes('/operational-observability/') ||
    normalized.includes('/cloudlink-and-core-state-machines/')
  ) {
    return 'operations';
  }
  if (
    normalized.includes('/guides/') ||
    normalized.includes('/get-started/') ||
    normalized.includes('/getting-started/')
  ) {
    return 'agent-tasks';
  }
  if (normalized.includes('/crates/') || normalized.includes('/extensions/')) {
    return 'optional';
  }
  return 'reference';
}

function markdownDocumentUrl(baseUrl, publicSlug) {
  return `${baseUrl}/${publicSlug}.md`;
}

export function findLlmsCoverageViolations(documents, index) {
  const destinations = [...index.matchAll(/\]\(([^)]+)\)/g)].map((match) => match[1]);
  const destinationPaths = destinations.map((destination) => {
    try {
      return new URL(destination, 'https://agent-index.invalid').pathname;
    } catch {
      return destination;
    }
  });
  return documents
    .filter(({ slug }) => slug !== '')
    .flatMap(({ slug, publicSlug }) => {
      const expectedPath = `/${publicSlug ?? slug}.md`;
      const count = destinationPaths.filter((destination) => destination === expectedPath).length;
      if (count === 1) return [];
      return [{ slug, expectedPath, count }];
    });
}

export function findLlmsLinkViolations(index) {
  return [...index.matchAll(/\]\(([^)]+)\)/g)]
    .map((match) => match[1])
    .filter(
      (destination) =>
        !destination.endsWith('.md') ||
        destination.endsWith(`/${FORBIDDEN_CONCATENATED_CORPUS}`)
    );
}

export function renderLlmsIndex(documents, publicBaseUrl, language = 'en') {
  const baseUrl = publicBaseUrl.replace(/\/$/, '');
  const chinese = language === 'zh-CN';
  const labels = chinese
    ? {
        'agent-tasks': '智能体任务手册',
        operations: '部署与运维',
        safety: '安全与治理',
        recovery: '故障恢复',
        reference: '平台参考',
        status: '兼容性与状态',
        optional: '可选内容',
      }
    : {
        'agent-tasks': 'Agent Task Manual',
        operations: 'Deployment and Operations',
        safety: 'Safety and Governance',
        recovery: 'Recovery',
        reference: 'Platform Reference',
        status: 'Compatibility and Status',
        optional: 'Optional',
      };
  const indexedDocuments = documents.filter(({ slug }) => slug !== '');
  const renderedSections = [];

  for (const section of LLMS_SECTION_KEYS) {
    const matched = indexedDocuments.filter((document) => inferLlmsSection(document) === section);
    renderedSections.push(`## ${labels[section]}`);
    renderedSections.push('');
    if (matched.length > 0) {
      renderedSections.push(
        matched
          .map(({ slug, publicSlug, title, description }) => {
            const url = markdownDocumentUrl(baseUrl, publicSlug ?? slug);
            return `- [${title}](${url})${description ? `: ${description}` : ''}`;
          })
          .join('\n')
      );
    } else {
      renderedSections.push(chinese ? '- 当前没有已发布条目。' : '- No published entries.');
    }
    renderedSections.push('');
  }

  return [
    '# AetherIoT',
    '',
    chinese
      ? '> 面向可靠物联网系统的开源边缘运行时、云端控制平面和互操作协议。'
      : '> Open-source edge, cloud, and interoperability building blocks for reliable IoT systems.',
    '',
    chinese
      ? '文档页面支持 Markdown。在任意文档地址后添加 `.md`，或发送 `Accept: text/markdown`。'
      : 'Documentation pages are available as Markdown. Append `.md` to any document URL or send `Accept: text/markdown`.',
    '',
    chinese
      ? '默认只读。静态文档不代表实时能力或执行授权；任何写操作都必须先读取安全策略并查询运行时能力。结果未知、超时或审计不完整的命令不得自动重试。AetherEdge 始终保留物理执行的最终决定权。'
      : 'Default to read-only. Static documentation is neither live capability evidence nor execution authorization; read the safety policy and query runtime capabilities before any write. Never automatically retry a command with an unknown, timed-out, or audit-incomplete outcome. AetherEdge retains final authority over physical execution.',
    '',
    ...renderedSections,
    '',
  ].join('\n');
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

  const htmlPages = await Promise.all(
    files.map(async (relPath) => {
      const slug = computeSlug(relPath);
      const htmlRelPath = slug === '' ? 'index.html' : path.posix.join(slug, 'index.html');
      return {
        path: htmlRelPath,
        html: await fs.readFile(path.join(DIST_DIR, htmlRelPath), 'utf8'),
      };
    })
  );
  const headingViolations = findHtmlHeadingViolations(htmlPages);
  if (headingViolations.length > 0) {
    const details = headingViolations
      .map(({ path: pagePath, headingCount }) => `  ${pagePath}: ${headingCount} h1 element(s)`)
      .join('\n');
    throw new Error(
      `build-docs: every documentation page must contain exactly one h1:\n${details}`
    );
  }
  const localizedUiViolations = findLocalizedUiViolations(htmlPages);
  if (localizedUiViolations.length > 0) {
    const details = localizedUiViolations
      .map(({ path: pagePath, locale, text }) => `  ${pagePath} (${locale}): ${text}`)
      .join('\n');
    throw new Error(`build-docs: framework UI locale leak(s) detected:\n${details}`);
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
  const localizedDocuments = partitionDocumentsByLocale(documents);
  const chineseIndex = renderLlmsIndex(
    localizedDocuments['zh-CN'],
    publicBaseUrl,
    'zh-CN'
  );
  const englishIndex = renderLlmsIndex(localizedDocuments.en, publicBaseUrl, 'en');
  const indexViolations = [
    ...findLlmsCoverageViolations(localizedDocuments['zh-CN'], chineseIndex),
    ...findLlmsCoverageViolations(localizedDocuments.en, englishIndex),
    ...findLlmsLinkViolations(chineseIndex).map((destination) => ({
      locale: 'zh-CN',
      destination,
    })),
    ...findLlmsLinkViolations(englishIndex).map((destination) => ({
      locale: 'en',
      destination,
    })),
  ];
  if (indexViolations.length > 0) {
    throw new Error(
      `build-docs: localized llms.txt coverage/link violation(s):\n${indexViolations
        .map((violation) => `  ${JSON.stringify(violation)}`)
        .join('\n')}`
    );
  }
  await fs.writeFile(
    path.join(DIST_DIR, 'llms.txt'),
    chineseIndex,
    'utf8'
  );
  await fs.mkdir(path.join(DIST_DIR, 'en'), { recursive: true });
  await fs.writeFile(
    path.join(DIST_DIR, 'en', 'llms.txt'),
    englishIndex,
    'utf8'
  );
  console.log(`build-docs: added ${documents.length} Markdown twins and 2 localized text indexes`);
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main().catch((error) => {
    console.error(error);
    process.exitCode = 1;
  });
}
/* v8 ignore stop */
