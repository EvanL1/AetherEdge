import { describe, expect, it } from 'vitest';
import {
  assertFilesFound,
  assertHtmlBuildPresent,
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
  it('renders a curated, grouped, absolute index for agents', () => {
    const documents = [
      { slug: '', title: 'Aether', description: 'Root page.' },
      {
        slug: 'agent-quickstart',
        title: 'Agent Quickstart',
        description: 'Install Aether.',
      },
      {
        slug: 'overview/platform',
        title: 'Platform Overview',
        description: 'Understand the product family.',
      },
      {
        slug: 'aetheredge/index',
        title: 'AetherEdge',
        description: 'Run the edge runtime.',
      },
      {
        slug: 'aethercloud/index',
        title: 'AetherCloud',
        description: 'Coordinate cloud workloads.',
      },
      {
        slug: 'aethercontracts/index',
        title: 'AetherContracts',
        description: 'Share public contracts.',
      },
      {
        slug: 'tutorials/edge-contracts-cloud',
        title: 'Edge to Cloud',
        description: 'Follow an end-to-end path.',
      },
      {
        slug: 'compatibility/version-matrix',
        title: 'Version Compatibility',
        description: 'Choose compatible versions.',
      },
      {
        slug: 'roadmap/status',
        title: 'Status and Roadmap',
        description: 'See implemented and planned capabilities.',
      },
      {
        slug: 'reference/cli',
        title: 'CLI Reference',
        description: 'Command reference.',
      },
    ];

    const output = renderLlmsIndex(documents, 'https://docs.aetheriot.workers.dev');
    expect(output).toMatch(/^# AetherIoT\n/);
    expect(output).toContain('## Overview');
    expect(output).toContain('## AetherEdge');
    expect(output).toContain('## AetherCloud');
    expect(output).toContain('## AetherContracts');
    expect(output).toContain('## Tutorials');
    expect(output).toContain('## Compatibility');
    expect(output).toContain('## Roadmap');
    expect(output).not.toContain('## Start Here');
    expect(output).not.toContain('## Reference');
    expect(output).toContain(
      '- [Agent Quickstart](https://docs.aetheriot.workers.dev/agent-quickstart): Install Aether.'
    );
    expect(output).not.toContain('[Aether](https://docs.aetheriot.workers.dev/)');
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

describe('assertHtmlBuildPresent', () => {
  it('rejects running the agent-doc emitter before the HTML build', () => {
    expect(() => assertHtmlBuildPresent(false)).toThrow(/HTML build/);
  });

  it('accepts an existing HTML build', () => {
    expect(() => assertHtmlBuildPresent(true)).not.toThrow();
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
