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
const SOURCE_CONFIG_PATH = path.join(DOCS_SITE_ROOT, 'content.sources.json');
const DESCRIPTION_MAX_LEN = 155;

export function computeDestPath(sourcePath, options = {}) {
  const stripPrefix = options.stripPrefix ?? 'docs/';
  const destinationPrefix = options.destinationPrefix ?? '';
  let destination;

  if (stripPrefix && sourcePath.startsWith(stripPrefix)) {
    destination = sourcePath.slice(stripPrefix.length);
  } else if (sourcePath.endsWith('/README.md')) {
    destination = sourcePath.slice(0, -'/README.md'.length) + '.md';
  } else {
    destination = sourcePath;
  }

  return destinationPrefix
    ? path.posix.join(destinationPrefix, destination)
    : destination;
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
    const existing = content.match(/^---\n([\s\S]*?)\n---\n?([\s\S]*)$/);
    if (!existing || /^title\s*:/m.test(existing[1])) {
      return content;
    }

    const metadata = existing[1];
    const body = existing[2];
    const { title, description } = extractTitleAndBody(body);
    const additions = [`title: ${JSON.stringify(title)}`];
    if (description && !/^description\s*:/m.test(metadata)) {
      additions.push(`description: ${JSON.stringify(description)}`);
    }
    if (gitDate && !/^updated\s*:/m.test(metadata)) {
      additions.push(`updated: ${gitDate}`);
    }
    return `---\n${additions.join('\n')}\n${metadata}\n---\n${body}`;
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
const GITHUB_BLOB_BASE = 'https://github.com/EvanL1/AetherEdge/blob/main';

export function addSourceAttribution(content, sourceRelPath, source) {
  if (!source) return content;

  const sourceUrl = `${source.githubBlobBase}/${sourceRelPath}`;
  const notice =
    `> Authoritative source: [${source.label}](${sourceUrl}). ` +
    'This page is mirrored into the unified AetherIoT documentation.';
  const heading = content.match(/^#\s+.+$/m);

  if (!heading || heading.index === undefined) {
    return `${notice}\n\n${content}`;
  }

  const insertAt = heading.index + heading[0].length;
  return `${content.slice(0, insertAt)}\n\n${notice}${content.slice(insertAt)}`;
}

// Rewrites every relative Markdown link into a stable published form. Links
// whose target is in the synced manifest become extensionless document
// routes derived from computeDestPath + computeSlug. Links whose target is
// NOT synced (e.g. excluded
// docs/domain/* pages) become absolute GitHub URLs instead, so they don't
// become dead links once mirrored. Callers must run findBrokenExternalLinks
// first — this function does not itself verify that the GitHub-blob branch
// points at a file that actually exists.
export function rewriteRelativeLinks(content, sourceRelPath, syncedSourceSet, options = {}) {
  const sourceDir = path.posix.dirname(sourceRelPath);
  const githubBlobBase = options.githubBlobBase ?? GITHUB_BLOB_BASE;
  return content.replace(MD_LINK_RE, (full, text, target) => {
    const [targetPath, anchor] = target.split('#');
    if (!targetPath) return full; // same-page anchor like [text](#section)
    const resolved = path.posix.normalize(path.posix.join(sourceDir, targetPath));
    const suffix = anchor ? `#${anchor}` : '';
    if (syncedSourceSet.has(resolved)) {
      const destRelPath = computeDestPath(resolved, options);
      const sitePath = slugToSitePath(computeSlug(destRelPath));
      return `[${text}](${sitePath}${suffix})`;
    }
    return `[${text}](${githubBlobBase}/${resolved}${suffix})`;
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

async function pathExistsInRepo(repoRelativePath, repoRoot = REPO_ROOT) {
  try {
    await fs.access(path.join(repoRoot, repoRelativePath));
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
export async function findBrokenExternalLinks(
  content,
  sourceRelPath,
  syncedSourceSet,
  options = {}
) {
  const targets = findExternalLinkTargets(content, sourceRelPath, syncedSourceSet);
  const problems = [];
  for (const { text, resolved } of targets) {
    if (!(await pathExistsInRepo(resolved, options.repoRoot))) {
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
// for each destination produced by two or more source files.
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
    if (sources.length > 1) {
      collisions.push({ dest, sources });
    }
  }
  return collisions;
}

/* v8 ignore start -- CLI filesystem orchestration is exercised by npm run build. */
function gitLastModifiedDate(repoRoot, repoRelativePath) {
  try {
    const out = execFileSync('git', ['log', '-1', '--format=%cs', '--', repoRelativePath], {
      cwd: repoRoot,
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
      await fs.rm(path.join(CONTENT_DIR, entry.name), { recursive: true, force: true });
    })
  );
}

async function syncFile(source, sourceRelPath, raw, syncedSourceSet) {
  const options = {
    destinationPrefix: source.destinationPrefix,
    githubBlobBase: source.githubBlobBase,
    stripPrefix: source.stripPrefix,
  };
  const destRelPath = computeDestPath(sourceRelPath, options);
  const destAbsPath = path.join(CONTENT_DIR, destRelPath);

  const rewritten = rewriteRelativeLinks(raw, sourceRelPath, syncedSourceSet, options);
  const attributed = addSourceAttribution(
    rewritten,
    sourceRelPath,
    source.attribution === false ? null : source
  );
  const gitDate = gitLastModifiedDate(source.repoRoot, sourceRelPath);
  const withFrontmatter = synthesizeFrontmatter(attributed, gitDate);

  await fs.mkdir(path.dirname(destAbsPath), { recursive: true });
  await fs.writeFile(destAbsPath, withFrontmatter, 'utf8');
  return destRelPath;
}

async function main() {
  const sourceConfig = JSON.parse(await fs.readFile(SOURCE_CONFIG_PATH, 'utf8'));
  const sourceContexts = await Promise.all(
    sourceConfig.sources.map(async (source) => {
      const configuredRoot = source.rootEnv ? process.env[source.rootEnv] : null;
      const repoRoot = configuredRoot
        ? path.resolve(configuredRoot)
        : path.resolve(DOCS_SITE_ROOT, source.root);
      const manifestPath = path.resolve(DOCS_SITE_ROOT, source.manifest);
      const manifestText = await fs.readFile(manifestPath, 'utf8');
      const patterns = readManifestPatterns(manifestText);
      const perPatternMatches = await Promise.all(
        patterns.map((pattern) => fg(pattern, { cwd: repoRoot, onlyFiles: true, dot: false }))
      );
      const emptyPatterns = patterns.filter((_, i) => perPatternMatches[i].length === 0);
      if (emptyPatterns.length > 0) {
        throw new Error(
          `sync-content: ${source.id} manifest pattern(s) matched zero files ` +
            `(typo, missing source checkout, or content moved?):\n` +
            emptyPatterns.map((pattern) => `  ${pattern}`).join('\n')
        );
      }

      const files = [...new Set(perPatternMatches.flat())].sort();
      return { ...source, repoRoot, files, syncedSourceSet: new Set(files), patterns };
    })
  );

  const entries = sourceContexts.flatMap((source) =>
    source.files.map((sourceRelPath) => ({ source, sourceRelPath }))
  );
  const sourceDestPairs = entries.map(({ source, sourceRelPath }) => [
    `${source.id}:${sourceRelPath}`,
    computeDestPath(sourceRelPath, {
      destinationPrefix: source.destinationPrefix,
      stripPrefix: source.stripPrefix,
    }),
  ]);

  const collisions = findCollisions(sourceDestPairs);
  if (collisions.length > 0) {
    const details = collisions
      .map(({ dest, sources: collidingSources }) => `  ${dest} <- ${collidingSources.join(', ')}`)
      .join('\n');
    throw new Error(`sync-content: destination path collision(s) detected:\n${details}`);
  }

  const rawContents = await Promise.all(
    entries.map(({ source, sourceRelPath }) =>
      fs.readFile(path.join(source.repoRoot, sourceRelPath), 'utf8')
    )
  );

  const brokenLinkGroups = await Promise.all(
    entries.map(({ source, sourceRelPath }, index) =>
      findBrokenExternalLinks(rawContents[index], sourceRelPath, source.syncedSourceSet, {
        repoRoot: source.repoRoot,
      })
    )
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
    entries.map(({ source, sourceRelPath }, index) =>
      syncFile(source, sourceRelPath, rawContents[index], source.syncedSourceSet)
    )
  );

  const patternCount = sourceContexts.reduce((total, source) => total + source.patterns.length, 0);
  console.log(
    `sync-content: wrote ${written.length} file(s) from ` +
      `${sourceContexts.length} sources and ${patternCount} manifest pattern(s)`
  );
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main().catch((err) => {
    console.error(err);
    process.exitCode = 1;
  });
}
/* v8 ignore stop */
