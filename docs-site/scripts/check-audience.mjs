import { fileURLToPath } from 'node:url';
import path from 'node:path';
import fs from 'node:fs/promises';
import fg from 'fast-glob';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const CONTENT_DIR = path.resolve(__dirname, '..', 'src', 'content', 'docs');
const INTERNAL_REFERENCE_PATTERN = /\bADR-\d{4}\b|(?:^|[/])docs\/adr\//i;

export function findInternalArchitectureReferences(sourcePath, content) {
  return content
    .split('\n')
    .map((text, index) => ({ path: sourcePath, line: index + 1, text }))
    .filter(({ text }) => INTERNAL_REFERENCE_PATTERN.test(text));
}

export function assertUserFacingDocumentation(documents) {
  const references = documents.flatMap(({ path: sourcePath, content }) =>
    findInternalArchitectureReferences(sourcePath, content)
  );
  if (references.length === 0) return;

  const details = references
    .map(({ path: sourcePath, line, text }) => `  ${sourcePath}:${line}: ${text.trim()}`)
    .join('\n');
  throw new Error(`Public documentation contains maintainer-only architecture references:\n${details}`);
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
