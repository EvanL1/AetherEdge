import { describe, expect, it } from 'vitest';
import {
  assertFilesFound,
  assertHtmlBuildPresent,
  findHtmlHeadingViolations,
  findLlmsCoverageViolations,
  findLlmsLinkViolations,
  findLocalizedUiViolations,
  findOutputCollisions,
  inferLlmsSection,
  partitionDocumentsByLocale,
  renderDocument,
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
  it('renders every published page once through the agent task taxonomy', () => {
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
        slug: 'guides/edge-contracts-cloud',
        title: 'Edge to Cloud',
        description: 'Complete a governed integration task.',
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
      {
        slug: 'recovery/unknown-command-outcome',
        title: 'Unknown Command Outcome',
        description: 'Recover without unsafe retries.',
      },
      {
        slug: 'security/control-authority',
        title: 'Control Authority',
        description: 'Preserve final edge authority.',
      },
      {
        slug: 'extensions/store-local',
        title: 'Local Store Extension',
        description: 'Deep implementation reference.',
      },
    ];

    const output = renderLlmsIndex(documents, 'https://docs.aetheriot.workers.dev');
    expect(output).toMatch(/^# AetherIoT\n/);
    expect(output).toContain('## Agent Task Manual');
    expect(output).toContain('## Deployment and Operations');
    expect(output).toContain('## Safety and Governance');
    expect(output).toContain('## Recovery');
    expect(output).toContain('## Platform Reference');
    expect(output).toContain('## Compatibility and Status');
    expect(output).toContain('## Optional');
    expect(output).not.toContain('## Tutorials');
    expect(output).not.toContain('llms-full.txt');
    expect(output).toContain(
      '- [Edge to Cloud](https://docs.aetheriot.workers.dev/guides/edge-contracts-cloud.md): Complete a governed integration task.'
    );
    expect(output).toContain(
      '- [Agent Quickstart](https://docs.aetheriot.workers.dev/agent-quickstart.md): Install Aether.'
    );
    expect(output).not.toContain('[Aether](https://docs.aetheriot.workers.dev/)');
    expect(findLlmsCoverageViolations(documents, output)).toEqual([]);
  });

  it('renders a Chinese-only task taxonomy for the root locale', () => {
    const documents = [
      { slug: '', title: 'AetherIoT 中文文档', description: '统一中文文档。' },
      { slug: 'overview/platform', title: '平台概览', description: '了解产品关系。' },
      {
        slug: 'guides/deployment',
        title: '部署边缘运行时',
        description: '部署并验证边缘运行时。',
      },
      {
        slug: 'security/safe-operations',
        title: '安全操作',
        description: '确认权限、风险和审计要求。',
      },
      {
        slug: 'recovery/gateway-identity',
        title: '恢复网关身份',
        description: '重新建立可信身份。',
      },
      {
        slug: 'compatibility/version-matrix',
        title: '版本兼容性',
        description: '选择经过验证的版本组合。',
      },
    ];

    const output = renderLlmsIndex(
      documents,
      'https://docs.aetheriot.workers.dev',
      'zh-CN'
    );
    expect(output).toContain('## 智能体任务手册');
    expect(output).toContain('## 部署与运维');
    expect(output).toContain('## 安全与治理');
    expect(output).toContain('## 故障恢复');
    expect(output).toContain('## 平台参考');
    expect(output).toContain('## 兼容性与状态');
    expect(output).toContain('文档页面支持 Markdown');
    expect(output).not.toContain('## Agent Task Manual');
    expect(output).not.toContain('## Tutorials');
    expect(output).not.toContain('/en/');
    expect(findLlmsCoverageViolations(documents, output)).toEqual([]);
  });
});

describe('inferLlmsSection', () => {
  it('routes safety and recovery before generic guide/reference rules', () => {
    expect(inferLlmsSection({ slug: 'guides/safe-operations' })).toBe('safety');
    expect(inferLlmsSection({ slug: 'aethercloud/recovery/cloudlink-reconnect' })).toBe(
      'recovery'
    );
    expect(inferLlmsSection({ slug: 'aethercloud/guides/deploy' })).toBe('operations');
    expect(inferLlmsSection({ slug: 'aethercontracts/spec/cloudlink-v1alpha1' })).toBe(
      'reference'
    );
  });

  it('keeps deep implementation material discoverable as optional context', () => {
    expect(inferLlmsSection({ slug: 'crates/aether-cloudlink' })).toBe('optional');
    expect(inferLlmsSection({ slug: 'extensions/store-local' })).toBe('optional');
  });
});

describe('findLlmsLinkViolations', () => {
  it('requires direct Markdown document links and rejects a full-corpus endpoint', () => {
    expect(
      findLlmsLinkViolations(
        [
          '# AetherIoT',
          '- [Good](https://docs.aetheriot.workers.dev/reference/cli.md)',
          '- [HTML route](https://docs.aetheriot.workers.dev/reference/http-api)',
          '- [Forbidden corpus](https://docs.aetheriot.workers.dev/llms-full.txt)',
        ].join('\n')
      )
    ).toEqual([
      'https://docs.aetheriot.workers.dev/reference/http-api',
      'https://docs.aetheriot.workers.dev/llms-full.txt',
    ]);
  });
});

describe('findLlmsCoverageViolations', () => {
  it('treats same-named Edge and Cloud pages as distinct full paths', () => {
    const documents = [
      { slug: 'concepts/architecture' },
      { slug: 'aethercloud/concepts/architecture' },
    ];
    const index = [
      '- [Edge](https://docs.aetheriot.workers.dev/concepts/architecture.md)',
      '- [Cloud](https://docs.aetheriot.workers.dev/aethercloud/concepts/architecture.md)',
    ].join('\n');

    expect(findLlmsCoverageViolations(documents, index)).toEqual([]);
  });
});

describe('partitionDocumentsByLocale', () => {
  it('separates English documents and normalizes their locale-relative slugs', () => {
    const partitions = partitionDocumentsByLocale([
      { slug: '', title: '中文首页' },
      { slug: 'aethercloud/index', title: '中文云端' },
      { slug: 'en', title: 'English home' },
      { slug: 'en/aethercloud/index', title: 'English cloud' },
    ]);

    expect(partitions['zh-CN'].map(({ slug }) => slug)).toEqual(['', 'aethercloud/index']);
    expect(partitions.en.map(({ slug, publicSlug }) => ({ slug, publicSlug }))).toEqual([
      { slug: '', publicSlug: 'en' },
      { slug: 'aethercloud/index', publicSlug: 'en/aethercloud/index' },
    ]);
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

describe('findHtmlHeadingViolations', () => {
  it('requires every generated documentation page to contain exactly one h1', () => {
    expect(
      findHtmlHeadingViolations([
        { path: 'index.html', html: '<main><h1>AetherIoT</h1></main>' },
        { path: 'empty/index.html', html: '<main><p>No title</p></main>' },
        {
          path: 'duplicate/index.html',
          html: '<main><h1>Page title</h1><article><h1>Repeated title</h1></article></main>',
        },
      ])
    ).toEqual([
      { path: 'empty/index.html', headingCount: 0 },
      { path: 'duplicate/index.html', headingCount: 2 },
    ]);
  });
});

describe('findLocalizedUiViolations', () => {
  it('accepts framework controls localized to each page language', () => {
    expect(
      findLocalizedUiViolations([
        {
          path: 'guides/example/index.html',
          html:
            '<button title="复制到剪贴板" data-copied="已复制！"></button><span>终端窗口</span><span>上一页</span>',
        },
        {
          path: 'en/guides/example/index.html',
          html:
            '<button title="Copy to clipboard" data-copied="Copied!"></button><span>Terminal window</span><span>Next</span>',
        },
      ])
    ).toEqual([]);
  });

  it('rejects Chinese and English framework controls leaking across locales', () => {
    expect(
      findLocalizedUiViolations([
        {
          path: 'guides/example/index.html',
          html: '<button title="Copy to clipboard"></button><span>Next</span>',
        },
        {
          path: 'en/guides/example/index.html',
          html: '<button data-copied="已复制！"></button><span>上一页</span>',
        },
      ])
    ).toEqual([
      {
        path: 'guides/example/index.html',
        locale: 'zh-CN',
        text: 'title="Copy to clipboard"',
      },
      {
        path: 'guides/example/index.html',
        locale: 'zh-CN',
        text: '>Next<',
      },
      {
        path: 'en/guides/example/index.html',
        locale: 'en',
        text: 'data-copied="已复制！"',
      },
      {
        path: 'en/guides/example/index.html',
        locale: 'en',
        text: '>上一页<',
      },
    ]);
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
