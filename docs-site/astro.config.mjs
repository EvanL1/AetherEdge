// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import starlightLinksValidator from 'starlight-links-validator';

export default defineConfig({
  site: 'https://docs.aetheriot.workers.dev',
  integrations: [
    starlight({
      title: 'Aether',
      description:
        'Documentation for the AI-native, industry-neutral IoT edge kernel and SDK.',
      social: [
        { icon: 'github', label: 'GitHub', href: 'https://github.com/EvanL1/AetherIot' },
      ],
      sidebar: [
        {
          label: 'Start Here',
          items: [
            { label: 'Agent Quickstart', slug: 'agent-quickstart' },
            { label: 'Getting Started', slug: 'guides/getting-started' },
          ],
        },
        { label: 'Concepts', items: [{ autogenerate: { directory: 'concepts' } }] },
        { label: 'Guides', items: [{ autogenerate: { directory: 'guides' } }] },
        { label: 'Reference', items: [{ autogenerate: { directory: 'reference' } }] },
        { label: 'SDK Crates', items: [{ autogenerate: { directory: 'crates' } }] },
        { label: 'Extensions', items: [{ autogenerate: { directory: 'extensions' } }] },
        { label: 'Security', items: [{ autogenerate: { directory: 'security' } }] },
      ],
      plugins: [starlightLinksValidator()],
    }),
  ],
});
