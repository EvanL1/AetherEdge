import { describe, expect, it } from 'vitest';
import {
  assertFilesFound,
  findOutputCollisions,
  renderDocument,
  renderLlmsFull,
  renderLlmsIndex,
  slugToOutputRelPath,
} from './build-docs.mjs';

describe('slugToOutputRelPath', () => {
  it('maps the empty slug to index.md', () => {
    expect(slugToOutputRelPath('')).toBe('index.md');
  });

  it('appends .md to a normal slug', () => {
    expect(slugToOutputRelPath('guides/getting-started')).toBe('guides/getting-started.md');
  });
});

describe('renderDocument', () => {
  it('removes build frontmatter and turns its title into a Markdown heading', () => {
    const source = '---\ntitle: "Agent Quickstart"\ndescription: "Install Aether."\n---\n\nFirst step.\n';

    expect(renderDocument(source)).toEqual({
      title: 'Agent Quickstart',
      description: 'Install Aether.',
      markdown: '# Agent Quickstart\n\nFirst step.\n',
    });
  });

  it('does not duplicate an existing level-one heading', () => {
    const source = '---\ntitle: "Architecture"\n---\n\n# Architecture\n\nDetails.\n';

    expect(renderDocument(source).markdown).toBe('# Architecture\n\nDetails.\n');
  });

  it('rejects documents without a title', () => {
    expect(() => renderDocument('Body only.\n')).toThrow(/title/);
  });
});

describe('renderLlmsIndex', () => {
  it('renders an absolute plain-text index for agents', () => {
    const documents = [
      {
        slug: 'agent-quickstart',
        title: 'Agent Quickstart',
        description: 'Install Aether.',
      },
    ];

    expect(renderLlmsIndex(documents, 'https://docs.aether-edge.workers.dev')).toContain(
      '- [Agent Quickstart](https://docs.aether-edge.workers.dev/agent-quickstart): Install Aether.'
    );
  });
});

describe('renderLlmsFull', () => {
  it('combines every rendered Markdown document without HTML', () => {
    const documents = [
      { title: 'Aether', markdown: '# Aether\n\nOverview.\n' },
      { title: 'Quickstart', markdown: '# Quickstart\n\nInstall.\n' },
    ];

    const output = renderLlmsFull(documents);
    expect(output).toContain('# Aether');
    expect(output).toContain('# Quickstart');
    expect(output).not.toContain('<html');
  });
});

describe('assertFilesFound', () => {
  it('throws when zero files are found', () => {
    expect(() => assertFilesFound([])).toThrow(/no markdown files found/);
  });
});

describe('findOutputCollisions', () => {
  it('flags sources that map to the same output path', () => {
    const collisions = findOutputCollisions([
      ['concepts/Architecture.md', 'concepts/architecture.md'],
      ['concepts/architecture.md', 'concepts/architecture.md'],
    ]);

    expect(collisions).toEqual([
      {
        outRelPath: 'concepts/architecture.md',
        sources: ['concepts/Architecture.md', 'concepts/architecture.md'],
      },
    ]);
  });
});
