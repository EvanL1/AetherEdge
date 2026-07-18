import { fileURLToPath } from 'node:url';
import path from 'node:path';
import fs from 'node:fs/promises';
import fg from 'fast-glob';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const CONTENT_DIR = path.resolve(__dirname, '..', 'src', 'content', 'docs');
const CJK_PATTERN = /\p{Script=Han}|\p{Script=Hiragana}|\p{Script=Katakana}|\p{Script=Hangul}/u;
const ENGLISH_WORD_PATTERN = /[A-Za-z]+(?:['’-][A-Za-z]+)*/g;
const ENGLISH_SENTENCE_END_PATTERN = /[.!?](?:["')\]]*)$/;
const COMMON_ENGLISH_WORDS = new Set([
  'a',
  'an',
  'and',
  'are',
  'as',
  'at',
  'before',
  'by',
  'for',
  'from',
  'in',
  'into',
  'is',
  'must',
  'of',
  'on',
  'or',
  'read',
  'run',
  'should',
  'the',
  'this',
  'to',
  'use',
  'using',
  'with',
  'without',
]);
const TECHNICAL_NAME_WORDS = new Set([
  'aethercloud',
  'aethercontracts',
  'aetheredge',
  'aetherems',
  'aetheriot',
  'api',
  'assistant',
  'cli',
  'cloudlink',
  'home',
  'http',
  'https',
  'json',
  'matter',
  'mcp',
  'mqtt',
  'oauth',
  'postgresql',
  'schema',
  'sdk',
  'tck',
  'tls',
  'websocket',
  'yaml',
  'zigbee',
]);
const ALLOWED_ASCII_HEADINGS = new Set([
  'AetherCloud',
  'AetherContracts',
  'AetherEdge',
  'AetherEMS',
  'AetherIoT',
  'Claude Desktop',
  'CloudLink',
  'CloudLink v1 alpha 1',
  'Docker Compose',
  'FileOutbox',
  'Home Assistant',
  'HTTP API',
  'MCP',
  'OAuth',
  'SDK',
  'SnapshotCovariateSource',
]);

export function findCjkOccurrences(sourcePath, content) {
  return content
    .split('\n')
    .map((text, index) => ({ path: sourcePath, line: index + 1, text }))
    .filter(({ text }) => CJK_PATTERN.test(text));
}

function normalizedMarkdownProse(content) {
  return content
    .replace(/!\[([^\]]*)\]\([^)]*\)/g, '$1')
    .replace(/\[([^\]]*)\]\([^)]*\)/g, '$1')
    .replace(/https?:\/\/\S+/g, '')
    .replace(/`[^`]*`/g, '')
    .replace(/<[^>]+>/g, '')
    .replace(/^[\s>#*+-]+/g, '')
    .trim();
}

function isEnglishSentence(content) {
  const prose = normalizedMarkdownProse(content);
  if (!ENGLISH_SENTENCE_END_PATTERN.test(prose)) return false;
  const words = prose.match(ENGLISH_WORD_PATTERN) ?? [];
  if (words.length < 3) return false;
  const normalizedWords = words.map((word) => word.toLowerCase());
  if (!normalizedWords.some((word) => COMMON_ENGLISH_WORDS.has(word))) return false;
  const nonTechnicalWords = normalizedWords.filter(
    (word) =>
      !TECHNICAL_NAME_WORDS.has(word) &&
      word !== 'and' &&
      word !== 'or'
  );
  return !(
    nonTechnicalWords.length === 0 ||
    (nonTechnicalWords.length === 1 && ['are', 'is'].includes(nonTechnicalWords[0]))
  );
}

export function findEnglishProseOccurrences(sourcePath, content) {
  const lines = content.split('\n');
  const occurrences = [];
  let frontmatter = lines[0]?.trim() === '---';
  let fence = null;
  let paragraph = [];

  const flush = () => {
    if (paragraph.length === 0) return;
    const text = paragraph.map(({ text: line }) => line.trim()).join(' ');
    if (!CJK_PATTERN.test(text) && isEnglishSentence(text)) {
      occurrences.push({
        path: sourcePath,
        line: paragraph[0].line,
        text,
      });
    }
    paragraph = [];
  };

  lines.forEach((text, index) => {
    const trimmed = text.trim();
    if (frontmatter) {
      if (index > 0 && trimmed === '---') frontmatter = false;
      return;
    }

    const fenceMarker = trimmed.match(/^(```|~~~)\s*([A-Za-z0-9_+-]*)/);
    if (fenceMarker) {
      flush();
      if (fence === null) {
        const language = fenceMarker[2].toLowerCase();
        fence = ['markdown', 'md', 'plaintext', 'prompt', 'text'].includes(language)
          ? 'prose'
          : 'code';
      } else {
        fence = null;
      }
      return;
    }
    if (fence === 'code') return;
    if (trimmed === '') {
      flush();
      return;
    }
    if (/^(?:[-*+]|\d+\.)\s+/.test(trimmed) && paragraph.length > 0) {
      flush();
    }
    paragraph.push({ line: index + 1, text });
  });
  flush();
  return occurrences;
}

function isAllowedAsciiHeading(heading) {
  if (ALLOWED_ASCII_HEADINGS.has(heading)) return true;
  if (/^RUSTSEC-\d{4}-\d{4}(?:\s+\([^)]*\))?$/.test(heading)) return true;
  if (/^[a-z][a-z0-9]*(?:[._-][a-z0-9]+)*$/.test(heading)) return true;
  return /^aether(?:\s+[a-z0-9][a-z0-9-]*)+$/.test(heading);
}

export function findEnglishHeadingOccurrences(sourcePath, content) {
  const lines = content.split('\n');
  const occurrences = [];
  let frontmatter = lines[0]?.trim() === '---';
  let fence = null;

  lines.forEach((text, index) => {
    const trimmed = text.trim();
    if (frontmatter) {
      if (index > 0 && trimmed === '---') frontmatter = false;
      return;
    }
    const fenceMarker = trimmed.match(/^(```|~~~)/);
    if (fenceMarker) {
      fence = fence === null ? fenceMarker[1] : null;
      return;
    }
    if (fence !== null) return;

    const match = trimmed.match(/^#{1,6}\s+(.+?)(?:\s+#+)?$/);
    if (match === null) return;
    const rawHeading = match[1].trim();
    if (/^`[^`]+`$/.test(rawHeading)) return;
    const heading = rawHeading
      .replace(/!\[([^\]]*)\]\([^)]*\)/g, '$1')
      .replace(/\[([^\]]*)\]\([^)]*\)/g, '$1')
      .replace(/`([^`]*)`/g, '$1')
      .replace(/[*_~]/g, '')
      .trim();
    if (
      heading.length === 0 ||
      CJK_PATTERN.test(heading) ||
      !/[A-Za-z]/.test(heading) ||
      isAllowedAsciiHeading(heading)
    ) {
      return;
    }
    occurrences.push({ path: sourcePath, line: index + 1, text });
  });
  return occurrences;
}

export function localeForPath(sourcePath) {
  return sourcePath === 'en' || sourcePath === 'en.md' || sourcePath.startsWith('en/')
    ? 'en'
    : 'zh-CN';
}

export function assertLocaleIsolation(documents) {
  const chineseDocuments = documents.filter(
    ({ path: sourcePath }) => localeForPath(sourcePath) === 'zh-CN'
  );
  const englishOccurrences = documents
    .filter(({ path: sourcePath }) => localeForPath(sourcePath) === 'en')
    .flatMap(({ path: sourcePath, content }) => findCjkOccurrences(sourcePath, content));
  const untranslatedChinese = chineseDocuments
    .filter(({ content }) => !CJK_PATTERN.test(content))
    .map(({ path: sourcePath }) => sourcePath);
  const englishChineseProse = chineseDocuments.flatMap(({ path: sourcePath, content }) =>
    findEnglishProseOccurrences(sourcePath, content)
  );
  const englishChineseHeadings = chineseDocuments.flatMap(({ path: sourcePath, content }) =>
    findEnglishHeadingOccurrences(sourcePath, content)
  );

  if (
    englishOccurrences.length === 0 &&
    untranslatedChinese.length === 0 &&
    englishChineseProse.length === 0 &&
    englishChineseHeadings.length === 0
  ) {
    return;
  }

  const englishDetails = englishOccurrences
    .map(({ path: sourcePath, line, text }) => `  ${sourcePath}:${line}: ${text.trim()}`)
    .join('\n');
  const chineseDetails = untranslatedChinese.map((sourcePath) => `  ${sourcePath}`).join('\n');
  const englishChineseDetails = englishChineseProse
    .map(({ path: sourcePath, line, text }) => `  ${sourcePath}:${line}: ${text}`)
    .join('\n');
  const englishChineseHeadingDetails = englishChineseHeadings
    .map(({ path: sourcePath, line, text }) => `  ${sourcePath}:${line}: ${text}`)
    .join('\n');
  const sections = [];
  if (englishDetails) {
    sections.push(`English publication contains CJK text:\n${englishDetails}`);
  }
  if (chineseDetails) {
    sections.push(`Chinese publication has no Chinese content:\n${chineseDetails}`);
  }
  if (englishChineseDetails) {
    sections.push(
      `Chinese publication contains untranslated English prose:\n${englishChineseDetails}`
    );
  }
  if (englishChineseHeadingDetails) {
    sections.push(
      `Chinese publication contains an untranslated English heading:\n${englishChineseHeadingDetails}`
    );
  }
  throw new Error(`Published locale content must remain isolated:\n${sections.join('\n')}`);
}

/* v8 ignore start -- filesystem orchestration is exercised by npm run build. */
async function main() {
  const contentDir = process.argv[2]
    ? path.resolve(process.cwd(), process.argv[2])
    : CONTENT_DIR;
  const files = (await fg(['**/*.md', '**/*.txt'], { cwd: contentDir, onlyFiles: true })).sort();
  const documents = await Promise.all(
    files.map(async (sourcePath) => ({
      path: sourcePath,
      content: await fs.readFile(path.join(contentDir, sourcePath), 'utf8'),
    }))
  );
  assertLocaleIsolation(documents);
  const englishCount = documents.filter(({ path: sourcePath }) => localeForPath(sourcePath) === 'en').length;
  console.log(
    `check-language: verified ${documents.length - englishCount} Chinese and ${englishCount} English documents`
  );
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main().catch((error) => {
    console.error(error);
    process.exitCode = 1;
  });
}
/* v8 ignore stop */
