// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import starlightLlmsTxt from 'starlight-llms-txt';
import starlightLinksValidator from 'starlight-links-validator';

export default defineConfig({
  site: 'https://aether-docs.liuyifanz-1996.workers.dev',
  integrations: [
    starlight({
      title: 'Aether',
      description:
        'AI-native, industry-neutral IoT edge kernel and SDK — architecture, guides, and reference.',
      social: [
        { icon: 'github', label: 'GitHub', href: 'https://github.com/EvanL1/Aether' },
      ],
      sidebar: [
        { label: 'Agent Quickstart', slug: 'agent-quickstart' },
        { label: 'Concepts', items: [{ autogenerate: { directory: 'concepts' } }] },
        { label: 'Guides', items: [{ autogenerate: { directory: 'guides' } }] },
        { label: 'Reference', items: [{ autogenerate: { directory: 'reference' } }] },
        { label: 'Architecture', items: [{ autogenerate: { directory: 'architecture' } }] },
        { label: 'Architecture Decisions', items: [{ autogenerate: { directory: 'adr' } }] },
        { label: 'Security', items: [{ autogenerate: { directory: 'security' } }] },
        { label: 'Crates', items: [{ autogenerate: { directory: 'crates' } }] },
        { label: 'Extensions', items: [{ autogenerate: { directory: 'extensions' } }] },
        {
          label: 'Project',
          items: [
            { label: 'AGENTS.md', slug: 'agents' },
            { label: 'ARCHITECTURE.md', slug: 'architecture' },
            { label: 'Configuration Format Guide', slug: 'config_format_guide' },
            { label: 'Development Getting Started', slug: 'getting_started_development' },
            { label: 'WebSocket Rule Monitor API', slug: 'websocket-rule-monitor-api' },
            { label: 'Benchmarking', slug: 'benchmarking' },
          ],
        },
      ],
      plugins: [
        starlightLlmsTxt({
          projectName: 'Aether',
          promote: ['agent-quickstart'],
          details:
            'Every page on this site is also available as raw Markdown: append `.md` to its URL, or send `Accept: text/markdown`.',
          optionalLinks: [
            {
              label: 'Agent Quickstart',
              url: '/agent-quickstart.md',
              description:
                'Copy-paste command sequence for an AI agent to install, start, and connect to Aether from zero.',
            },
          ],
        }),
        starlightLinksValidator(),
      ],
    }),
  ],
});
