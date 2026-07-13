import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import fs from 'node:fs/promises';
import fg from 'fast-glob';
import { computeSlug, slugToSitePath } from './slug.mjs';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(__dirname, '..', '..');
const DOCS_SITE_ROOT = path.resolve(__dirname, '..');
const CONTENT_DIR = path.join(DOCS_SITE_ROOT, 'src', 'content', 'docs');
const MANIFEST_PATH = path.join(DOCS_SITE_ROOT, 'content.manifest.txt');
const HAND_AUTHORED = new Set(['index.md', 'agent-quickstart.md']);
const DESCRIPTION_MAX_LEN = 155;

export function computeDestPath(sourcePath) {
  if (sourcePath.startsWith('docs/')) {
    return sourcePath.slice('docs/'.length);
  }
  if (sourcePath.endsWith('/README.md')) {
    return sourcePath.slice(0, -'/README.md'.length) + '.md';
  }
  return path.basename(sourcePath);
}

function extractTitleAndBody(content) {
  const lines = content.split('\n');
  const titleIndex = lines.findIndex((line) => /^#\s+\S/.test(line));
  if (titleIndex === -1) {
    return { title: 'Untitled', description: '' };
  }
  const title = lines[titleIndex].replace(/^#\s+/, '').trim();

  const paragraphLines = [];
  for (let i = titleIndex + 1; i < lines.length; i++) {
    const line = lines[i];
    if (line.trim() === '') {
      if (paragraphLines.length > 0) break;
      continue;
    }
    if (
      /^#{1,6}\s/.test(line) ||
      line.startsWith('>') ||
      line.startsWith('```') ||
      /^(\*{3,}|-{3,}|_{3,})\s*$/.test(line.trim())
    ) {
      if (paragraphLines.length > 0) break;
      continue;
    }
    paragraphLines.push(line.trim());
  }
  let description = paragraphLines.join(' ').trim();
  // Strip Markdown link syntax down to just the link text — otherwise raw
  // "[text](url)" leaks into <meta name="description"> and llms.txt.
  description = description.replace(/\[([^\]]*)\]\([^)]*\)/g, '$1');
  if (description.length > DESCRIPTION_MAX_LEN) {
    const candidate = description.slice(0, DESCRIPTION_MAX_LEN - 1).trimEnd();
    const lastSpace = candidate.lastIndexOf(' ');
    const boundary = lastSpace >= DESCRIPTION_MAX_LEN / 2 ? lastSpace : candidate.length;
    description = candidate.slice(0, boundary).trimEnd() + '…';
  }
  return { title, description };
}

export function synthesizeFrontmatter(content, gitDate) {
  if (content.startsWith('---\n')) {
    return content;
  }
  const { title, description } = extractTitleAndBody(content);
  const frontmatterLines = ['---', `title: ${JSON.stringify(title)}`];
  if (description) {
    frontmatterLines.push(`description: ${JSON.stringify(description)}`);
  }
  if (gitDate) {
    frontmatterLines.push(`updated: ${gitDate}`);
  }
  frontmatterLines.push('---', '', '');
  return frontmatterLines.join('\n') + content;
}

const MD_LINK_RE = /\[([^\]]*)\]\((?!https?:\/\/|mailto:|#|\/)([^)\s]+)\)/g;
const GITHUB_BLOB_BASE = 'https://github.com/EvanL1/AetherIot/blob/main';

// Rewrites every relative Markdown link into a stable published form. Links
// whose target is in the synced manifest become extensionless document
// routes derived from computeDestPath + computeSlug. Links whose target is
// NOT synced (e.g. excluded
// docs/domain/* pages) become absolute GitHub URLs instead, so they don't
// become dead links once mirrored. Callers must run findBrokenExternalLinks
// first — this function does not itself verify that the GitHub-blob branch
// points at a file that actually exists.
export function rewriteRelativeLinks(content, sourceRelPath, syncedSourceSet) {
  const sourceDir = path.posix.dirname(sourceRelPath);
  return content.replace(MD_LINK_RE, (full, text, target) => {
    const [targetPath, anchor] = target.split('#');
    if (!targetPath) return full; // same-page anchor like [text](#section)
    const resolved = path.posix.normalize(path.posix.join(sourceDir, targetPath));
    const suffix = anchor ? `#${anchor}` : '';
    if (syncedSourceSet.has(resolved)) {
      const destRelPath = computeDestPath(resolved);
      const sitePath = slugToSitePath(computeSlug(destRelPath));
      return `[${text}](${sitePath}${suffix})`;
    }
    return `[${text}](${GITHUB_BLOB_BASE}/${resolved}${suffix})`;
  });
}

// Pure: finds every relative-link target in content that resolves OUTSIDE
// the synced manifest (i.e. the candidates rewriteRelativeLinks would turn
// into a GitHub blob URL). Does not touch the filesystem.
function findExternalLinkTargets(content, sourceRelPath, syncedSourceSet) {
  const sourceDir = path.posix.dirname(sourceRelPath);
  const targets = [];
  for (const match of content.matchAll(MD_LINK_RE)) {
    const [, text, target] = match;
    const [targetPath] = target.split('#');
    if (!targetPath) continue; // same-page anchor like [text](#section)
    const resolved = path.posix.normalize(path.posix.join(sourceDir, targetPath));
    if (!syncedSourceSet.has(resolved)) {
      targets.push({ text, resolved });
    }
  }
  return targets;
}

async function pathExistsInRepo(repoRelativePath) {
  try {
    await fs.access(path.join(REPO_ROOT, repoRelativePath));
    return true;
  } catch {
    return false;
  }
}

// starlight-links-validator only checks internal (site-relative) links —
// its sameSitePolicy: 'ignore' default never validates https:// targets, so
// the GitHub-blob-URL branch of rewriteRelativeLinks has no build-time
// safety net once astro build runs. This closes that gap by verifying, at
// sync time, that every link resolving outside the synced manifest still
// points at a file that actually exists in the repo — catching typos (e.g.
// docs/doman/... missing an 'a') before they ship as silently-dead links.
export async function findBrokenExternalLinks(content, sourceRelPath, syncedSourceSet) {
  const targets = findExternalLinkTargets(content, sourceRelPath, syncedSourceSet);
  const problems = [];
  for (const { text, resolved } of targets) {
    if (!(await pathExistsInRepo(resolved))) {
      problems.push({ source: sourceRelPath, text, resolved });
    }
  }
  return problems;
}

/* v8 ignore next -- covered through the end-to-end sync command. */
function readManifestPatterns(manifestText) {
  return manifestText
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line.length > 0 && !line.startsWith('#'));
}

// sourceDestPairs: array of [sourceRelPath, destRelPath]. Returns one entry
// per problem destination: either two-or-more sources mapping to the same
// dest, or a dest that would silently overwrite a hand-authored page.
export function findCollisions(sourceDestPairs) {
  const sourcesByDest = new Map();
  for (const [source, dest] of sourceDestPairs) {
    if (!sourcesByDest.has(dest)) {
      sourcesByDest.set(dest, []);
    }
    sourcesByDest.get(dest).push(source);
  }

  const collisions = [];
  for (const [dest, sources] of sourcesByDest) {
    if (sources.length > 1 || HAND_AUTHORED.has(dest)) {
      collisions.push({ dest, sources });
    }
  }
  return collisions;
}

/* v8 ignore start -- CLI filesystem orchestration is exercised by npm run build. */
function gitLastModifiedDate(repoRelativePath) {
  try {
    const out = execFileSync('git', ['log', '-1', '--format=%cs', '--', repoRelativePath], {
      cwd: REPO_ROOT,
      encoding: 'utf8',
    }).trim();
    return out || null;
  } catch {
    return null;
  }
}

async function clearGeneratedContent() {
  let entries;
  try {
    entries = await fs.readdir(CONTENT_DIR, { withFileTypes: true });
  } catch (err) {
    if (err.code === 'ENOENT') {
      await fs.mkdir(CONTENT_DIR, { recursive: true });
      return;
    }
    throw err;
  }
  await Promise.all(
    entries.map(async (entry) => {
      if (HAND_AUTHORED.has(entry.name)) return;
      await fs.rm(path.join(CONTENT_DIR, entry.name), { recursive: true, force: true });
    })
  );
}

async function syncFile(sourceRelPath, raw, syncedSourceSet) {
  const destRelPath = computeDestPath(sourceRelPath);
  const destAbsPath = path.join(CONTENT_DIR, destRelPath);

  const rewritten = rewriteRelativeLinks(raw, sourceRelPath, syncedSourceSet);
  const gitDate = gitLastModifiedDate(sourceRelPath);
  const withFrontmatter = synthesizeFrontmatter(rewritten, gitDate);

  await fs.mkdir(path.dirname(destAbsPath), { recursive: true });
  await fs.writeFile(destAbsPath, withFrontmatter, 'utf8');
  return destRelPath;
}

async function main() {
  const manifestText = await fs.readFile(MANIFEST_PATH, 'utf8');
  const patterns = readManifestPatterns(manifestText);

  const perPatternMatches = await Promise.all(
    patterns.map((pattern) => fg(pattern, { cwd: REPO_ROOT, onlyFiles: true, dot: false }))
  );
  const emptyPatterns = patterns.filter((_, i) => perPatternMatches[i].length === 0);
  if (emptyPatterns.length > 0) {
    throw new Error(
      `sync-content: manifest pattern(s) matched zero files (typo, or content moved?):\n` +
        emptyPatterns.map((p) => `  ${p}`).join('\n')
    );
  }

  const sources = [...new Set(perPatternMatches.flat())].sort();
  const sourceDestPairs = sources.map((source) => [source, computeDestPath(source)]);

  const collisions = findCollisions(sourceDestPairs);
  if (collisions.length > 0) {
    const details = collisions
      .map(({ dest, sources: collidingSources }) => `  ${dest} <- ${collidingSources.join(', ')}`)
      .join('\n');
    throw new Error(`sync-content: destination path collision(s) detected:\n${details}`);
  }

  const syncedSourceSet = new Set(sources);

  const rawContents = await Promise.all(
    sources.map((source) => fs.readFile(path.join(REPO_ROOT, source), 'utf8'))
  );

  const brokenLinkGroups = await Promise.all(
    sources.map((source, i) => findBrokenExternalLinks(rawContents[i], source, syncedSourceSet))
  );
  const brokenLinks = brokenLinkGroups.flat();
  if (brokenLinks.length > 0) {
    const details = brokenLinks
      .map(({ source, text, resolved }) => `  ${source}: [${text}](${resolved}) — file not found in repo`)
      .join('\n');
    throw new Error(
      `sync-content: relative link(s) resolve to excluded content that does not exist on disk:\n${details}`
    );
  }

  await clearGeneratedContent();
  const written = await Promise.all(
    sources.map((source, i) => syncFile(source, rawContents[i], syncedSourceSet))
  );

  console.log(`sync-content: wrote ${written.length} file(s) from ${patterns.length} manifest pattern(s)`);
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main().catch((err) => {
    console.error(err);
    process.exitCode = 1;
  });
}
/* v8 ignore stop */
