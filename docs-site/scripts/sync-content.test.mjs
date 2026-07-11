import { describe, expect, it } from 'vitest';
import {
  computeDestPath,
  findBrokenExternalLinks,
  findCollisions,
  rewriteRelativeLinks,
  synthesizeFrontmatter,
} from './sync-content.mjs';
import { computeSlug, slugToSitePath } from './slug.mjs';

describe('computeDestPath', () => {
  it('mirrors docs/ paths with the prefix stripped', () => {
    expect(computeDestPath('docs/concepts/architecture.md')).toBe('concepts/architecture.md');
  });

  it('mirrors flat docs/ files to the content root', () => {
    expect(computeDestPath('docs/CONFIG_FORMAT_GUIDE.md')).toBe('CONFIG_FORMAT_GUIDE.md');
  });

  it('rewrites a nested README.md to <dir>.md', () => {
    expect(computeDestPath('crates/aether-testkit/README.md')).toBe('crates/aether-testkit.md');
    expect(computeDestPath('extensions/redis-bridge/README.md')).toBe('extensions/redis-bridge.md');
  });

  it('places bare root files at the content root by basename', () => {
    expect(computeDestPath('AGENTS.md')).toBe('AGENTS.md');
    expect(computeDestPath('ARCHITECTURE.md')).toBe('ARCHITECTURE.md');
  });
});

describe('synthesizeFrontmatter', () => {
  it('passes through content that already has frontmatter', () => {
    const content = '---\ntitle: Existing\n---\n\n# Existing\n';
    expect(synthesizeFrontmatter(content, '2026-01-01')).toBe(content);
  });

  it('derives title from the first heading and description from the next paragraph', () => {
    const content = '# ADR-0001: Adopt an AI-native edge-kernel architecture\n\nAccepted for incremental migration on 2026-07-10. Runtime-composition clauses apply.\n\nMore text.\n';
    const out = synthesizeFrontmatter(content, '2026-07-10');
    expect(out).toContain('title: "ADR-0001: Adopt an AI-native edge-kernel architecture"');
    expect(out).toContain('description: "Accepted for incremental migration on 2026-07-10. Runtime-composition clauses apply."');
    expect(out).toContain('updated: 2026-07-10');
    expect(out.endsWith(content)).toBe(true);
  });

  it('truncates an overlong description with an ellipsis', () => {
    const longSentence = 'x'.repeat(200);
    const content = `# Title\n\n${longSentence}\n`;
    const out = synthesizeFrontmatter(content, '2026-07-10');
    const descLine = out.split('\n').find((l) => l.startsWith('description:'));
    expect(descLine.length).toBeLessThan(180);
    expect(descLine).toContain('…"');
  });

  it('falls back to "Untitled" when there is no heading', () => {
    const content = 'No heading here, just text.\n';
    const out = synthesizeFrontmatter(content, null);
    expect(out).toContain('title: "Untitled"');
    expect(out).not.toContain('updated:');
  });

  it('strips Markdown link syntax from the description, keeping only the link text', () => {
    // Reproduces the real docs/adr/0001-ai-native-edge-kernel.md bug: its
    // paragraph after the title contains a Markdown link, which used to leak
    // raw "[text](url)" syntax into the synthesized description.
    const content =
      '# ADR-0001: Adopt an AI-native edge-kernel architecture\n\n' +
      'Runtime-composition clauses were amended by [ADR-0003](0003-multi-process-shm-event-plane.md) on the same date.\n';
    const out = synthesizeFrontmatter(content, null);
    const descLine = out.split('\n').find((l) => l.startsWith('description:'));
    expect(descLine).toContain('ADR-0003');
    expect(descLine).not.toContain('[ADR-0003]');
    expect(descLine).not.toContain('](');
  });

  it('skips a horizontal rule when deriving the first prose description', () => {
    const content = '# Benchmarking\n\n---\n\nThe first useful paragraph.\n';
    const out = synthesizeFrontmatter(content, null);

    expect(out).toContain('description: "The first useful paragraph."');
  });
});

describe('findCollisions', () => {
  it('returns no collisions when every destination is unique and non-reserved', () => {
    const pairs = [
      ['docs/concepts/architecture.md', 'concepts/architecture.md'],
      ['AGENTS.md', 'AGENTS.md'],
      ['crates/aether-testkit/README.md', 'crates/aether-testkit.md'],
    ];
    expect(findCollisions(pairs)).toEqual([]);
  });

  it('flags multiple sources that compute to the same destination', () => {
    // Reproduces the real repo shape: root AGENTS.md plus a hypothetical
    // crates/*/AGENTS.md and extensions/*/AGENTS.md manifest addition would
    // all fall back to the bare basename "AGENTS.md".
    const pairs = [
      ['AGENTS.md', 'AGENTS.md'],
      ['crates/aether-testkit/AGENTS.md', 'AGENTS.md'],
      ['extensions/redis-bridge/AGENTS.md', 'AGENTS.md'],
    ];
    const collisions = findCollisions(pairs);
    expect(collisions).toHaveLength(1);
    expect(collisions[0].dest).toBe('AGENTS.md');
    expect(collisions[0].sources).toEqual([
      'AGENTS.md',
      'crates/aether-testkit/AGENTS.md',
      'extensions/redis-bridge/AGENTS.md',
    ]);
  });

  it('flags a single source that would overwrite a hand-authored page', () => {
    const pairs = [['docs/index.md', 'index.md']];
    const collisions = findCollisions(pairs);
    expect(collisions).toHaveLength(1);
    expect(collisions[0]).toEqual({ dest: 'index.md', sources: ['docs/index.md'] });
  });

  it('flags docs/agent-quickstart.md the same way', () => {
    const pairs = [['docs/agent-quickstart.md', 'agent-quickstart.md']];
    expect(findCollisions(pairs)).toEqual([
      { dest: 'agent-quickstart.md', sources: ['docs/agent-quickstart.md'] },
    ]);
  });
});

describe('rewriteRelativeLinks', () => {
  it('rewrites a link whose resolved target is not in the synced set to a GitHub URL', () => {
    const content = 'See [Control Strategies](../domain/control-strategies.md) for details.';
    const syncedSourceSet = new Set(['docs/guides/writing-rules.md']);
    const out = rewriteRelativeLinks(content, 'docs/guides/writing-rules.md', syncedSourceSet);
    expect(out).toBe(
      'See [Control Strategies](https://github.com/EvanL1/Aether/blob/main/docs/domain/control-strategies.md) for details.'
    );
  });

  it('preserves an anchor fragment when rewriting an excluded target', () => {
    const content = '[Safe Operations](../domain/safe-operations.md#some-heading)';
    const syncedSourceSet = new Set(['docs/guides/ai-assistants.md']);
    const out = rewriteRelativeLinks(content, 'docs/guides/ai-assistants.md', syncedSourceSet);
    expect(out).toBe(
      '[Safe Operations](https://github.com/EvanL1/Aether/blob/main/docs/domain/safe-operations.md#some-heading)'
    );
  });

  it('leaves a same-page anchor link unchanged', () => {
    const content = 'Jump to [the section](#section) below.';
    const syncedSourceSet = new Set(['docs/guides/ai-assistants.md']);
    expect(rewriteRelativeLinks(content, 'docs/guides/ai-assistants.md', syncedSourceSet)).toBe(content);
  });

  it('leaves an already-absolute link unchanged', () => {
    const content = 'See [example](https://example.com) for more.';
    const syncedSourceSet = new Set(['docs/guides/ai-assistants.md']);
    expect(rewriteRelativeLinks(content, 'docs/guides/ai-assistants.md', syncedSourceSet)).toBe(content);
  });

  it('reproduces the real ai-assistants.md -> domain/safe-operations.md case', () => {
    const content =
      'Before enabling writes, read [Safe Operations for AI Agents](../domain/safe-operations.md), which explains the control envelope.';
    const syncedSourceSet = new Set([
      'docs/guides/ai-assistants.md',
      'docs/guides/writing-rules.md',
      'docs/concepts/data-model.md',
      'AGENTS.md',
    ]);
    const out = rewriteRelativeLinks(content, 'docs/guides/ai-assistants.md', syncedSourceSet);
    expect(out).toBe(
      'Before enabling writes, read [Safe Operations for AI Agents](https://github.com/EvanL1/Aether/blob/main/docs/domain/safe-operations.md), which explains the control envelope.'
    );
  });

  it('rewrites a link whose resolved target is in the synced set to its document route', () => {
    const content = 'See [Rule Engine](../concepts/rule-engine.md) for details.';
    const syncedSourceSet = new Set(['docs/guides/writing-rules.md', 'docs/concepts/rule-engine.md']);
    const out = rewriteRelativeLinks(content, 'docs/guides/writing-rules.md', syncedSourceSet);
    expect(out).toBe('See [Rule Engine](/concepts/rule-engine) for details.');
  });

  it('preserves an anchor fragment when rewriting a synced target to a site path', () => {
    const content = '[Rule Engine](../concepts/rule-engine.md#some-heading)';
    const syncedSourceSet = new Set(['docs/guides/writing-rules.md', 'docs/concepts/rule-engine.md']);
    const out = rewriteRelativeLinks(content, 'docs/guides/writing-rules.md', syncedSourceSet);
    expect(out).toBe('[Rule Engine](/concepts/rule-engine#some-heading)');
  });

  it('rewrites a link to a synced README.md-shaped target to its collapsed <dir>/ site path', () => {
    const content = 'See [aether-ports](../aether-ports/README.md) for details.';
    const syncedSourceSet = new Set([
      'crates/aether-testkit/README.md',
      'crates/aether-ports/README.md',
    ]);
    const out = rewriteRelativeLinks(content, 'crates/aether-testkit/README.md', syncedSourceSet);
    expect(out).toBe('See [aether-ports](/crates/aether-ports) for details.');
  });
});

describe('findBrokenExternalLinks', () => {
  // A typo'd excluded-content link would otherwise ship as a silently-dead
  // GitHub URL. These use real repo paths (not a live-repo mutation) so the
  // existence check runs against real files/real absence.

  it('flags a relative link whose resolved target does not exist on disk', async () => {
    const content = '[Bad Link](../domain/typo-nonexistent-file.md)';
    const syncedSourceSet = new Set(['docs/guides/ai-assistants.md']);
    const problems = await findBrokenExternalLinks(content, 'docs/guides/ai-assistants.md', syncedSourceSet);
    expect(problems).toEqual([
      {
        source: 'docs/guides/ai-assistants.md',
        text: 'Bad Link',
        resolved: 'docs/domain/typo-nonexistent-file.md',
      },
    ]);
  });

  it('does not flag a relative link whose resolved target exists on disk, even though it is excluded from the manifest', async () => {
    const content = '[Safe Operations](../domain/safe-operations.md)';
    const syncedSourceSet = new Set(['docs/guides/ai-assistants.md']);
    const problems = await findBrokenExternalLinks(content, 'docs/guides/ai-assistants.md', syncedSourceSet);
    expect(problems).toEqual([]);
  });

  it('does not flag a link whose resolved target is in the synced set', async () => {
    const content = '[Rule Engine](../concepts/rule-engine.md)';
    const syncedSourceSet = new Set(['docs/guides/writing-rules.md', 'docs/concepts/rule-engine.md']);
    const problems = await findBrokenExternalLinks(content, 'docs/guides/writing-rules.md', syncedSourceSet);
    expect(problems).toEqual([]);
  });
});

describe('computeSlug', () => {
  it('strips the .md extension', () => {
    expect(computeSlug('concepts/architecture.md')).toBe('concepts/architecture');
  });

  it('collapses a root index.md to the empty slug', () => {
    expect(computeSlug('index.md')).toBe('');
  });

  it('strips a trailing /index segment', () => {
    expect(computeSlug('guides/index.md')).toBe('guides');
  });

  it('slugifies a punctuation-bearing segment the way github-slugger actually does, not a guess', () => {
    // github-slugger's slug("what's-new") strips the apostrophe rather than
    // hyphenating or dropping the whole word: "whats-new". The old
    // whole-string .toLowerCase() implementation never exercised this path
    // (no synced filename has punctuation beyond hyphens/underscores today)
    // but would have left the apostrophe in place, producing a wrong URL.
    expect(computeSlug("guides/what's-new.md")).toBe('guides/whats-new');
  });
});

describe('slugToSitePath', () => {
  it('maps the empty slug to the site root', () => {
    expect(slugToSitePath('')).toBe('/');
  });

  it('prefixes a non-empty slug with a slash', () => {
    expect(slugToSitePath('concepts/architecture')).toBe('/concepts/architecture');
  });
});
