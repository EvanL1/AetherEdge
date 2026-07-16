import { fileURLToPath } from 'node:url';
import path from 'node:path';
import fs from 'node:fs/promises';
import fg from 'fast-glob';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const CONTENT_DIR = path.resolve(__dirname, '..', 'src', 'content', 'docs');
const CJK_PATTERN = /\p{Script=Han}|\p{Script=Hiragana}|\p{Script=Katakana}|\p{Script=Hangul}/u;

export function findCjkOccurrences(sourcePath, content) {
  return content
    .split('\n')
    .map((text, index) => ({ path: sourcePath, line: index + 1, text }))
    .filter(({ text }) => CJK_PATTERN.test(text));
}

export function localeForPath(sourcePath) {
  return sourcePath === 'en' || sourcePath.startsWith('en/') ? 'en' : 'zh-CN';
}

export function assertLocaleIsolation(documents) {
  const englishOccurrences = documents
    .filter(({ path: sourcePath }) => localeForPath(sourcePath) === 'en')
    .flatMap(({ path: sourcePath, content }) => findCjkOccurrences(sourcePath, content));
  const untranslatedChinese = documents
    .filter(({ path: sourcePath }) => localeForPath(sourcePath) === 'zh-CN')
    .filter(({ content }) => !CJK_PATTERN.test(content))
    .map(({ path: sourcePath }) => sourcePath);

  if (englishOccurrences.length === 0 && untranslatedChinese.length === 0) return;

  const englishDetails = englishOccurrences
    .map(({ path: sourcePath, line, text }) => `  ${sourcePath}:${line}: ${text.trim()}`)
    .join('\n');
  const chineseDetails = untranslatedChinese.map((sourcePath) => `  ${sourcePath}`).join('\n');
  const sections = [];
  if (englishDetails) {
    sections.push(`English publication contains CJK text:\n${englishDetails}`);
  }
  if (chineseDetails) {
    sections.push(`Chinese publication has no Chinese content:\n${chineseDetails}`);
  }
  throw new Error(`Published locale content must remain isolated:\n${sections.join('\n')}`);
}

/* v8 ignore start -- filesystem orchestration is exercised by npm run build. */
async function main() {
  const files = (await fg('**/*.md', { cwd: CONTENT_DIR, onlyFiles: true })).sort();
  const documents = await Promise.all(
    files.map(async (sourcePath) => ({
      path: sourcePath,
      content: await fs.readFile(path.join(CONTENT_DIR, sourcePath), 'utf8'),
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
