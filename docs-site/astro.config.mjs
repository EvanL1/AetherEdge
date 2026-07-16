// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import starlightLinksValidator from 'starlight-links-validator';

export default defineConfig({
  site: 'https://docs.aetheriot.workers.dev',
  integrations: [
    starlight({
      title: 'AetherIoT',
      description:
        'Unified documentation for AetherEdge, AetherCloud, and AetherContracts.',
      social: [
        { icon: 'github', label: 'GitHub', href: 'https://github.com/EvanL1/AetherEdge' },
      ],
      sidebar: [
        {
          label: 'Overview',
          items: [
            { label: 'Platform', slug: 'overview/platform' },
            { label: 'Deployment Topologies', slug: 'overview/deployment-topologies' },
            { label: 'User Journeys', slug: 'overview/user-journeys' },
          ],
        },
        {
          label: 'AetherEdge',
          items: [
            { label: 'Product Overview', slug: 'aetheredge' },
            { label: 'Agent Quickstart', slug: 'agent-quickstart' },
            { label: 'Getting Started', slug: 'guides/getting-started' },
            { label: 'Concepts', items: [{ autogenerate: { directory: 'concepts' } }] },
            { label: 'Guides', items: [{ autogenerate: { directory: 'guides' } }] },
            { label: 'Reference', items: [{ autogenerate: { directory: 'reference' } }] },
            { label: 'SDK Crates', items: [{ autogenerate: { directory: 'crates' } }] },
            { label: 'Extensions', items: [{ autogenerate: { directory: 'extensions' } }] },
            { label: 'Security', items: [{ autogenerate: { directory: 'security' } }] },
          ],
        },
        { label: 'AetherCloud', items: [{ label: 'Product Overview', slug: 'aethercloud' }] },
        {
          label: 'AetherContracts',
          items: [{ label: 'Product Overview', slug: 'aethercontracts' }],
        },
        { label: 'Tutorials', items: [{ autogenerate: { directory: 'tutorials' } }] },
        { label: 'Compatibility', items: [{ autogenerate: { directory: 'compatibility' } }] },
        { label: 'Roadmap', items: [{ autogenerate: { directory: 'roadmap' } }] },
      ],
      plugins: [starlightLinksValidator()],
    }),
  ],
});
