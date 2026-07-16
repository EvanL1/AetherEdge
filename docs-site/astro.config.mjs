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
      customCss: ['./src/styles/custom.css'],
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
        {
          label: 'AetherCloud',
          items: [
            { label: 'Product Overview', slug: 'aethercloud' },
            {
              label: 'Get Started',
              items: [{ autogenerate: { directory: 'aethercloud/get-started' } }],
            },
            {
              label: 'Concepts',
              items: [{ autogenerate: { directory: 'aethercloud/concepts' } }],
            },
            {
              label: 'Guides',
              items: [{ autogenerate: { directory: 'aethercloud/guides' } }],
            },
            {
              label: 'Reference',
              items: [{ autogenerate: { directory: 'aethercloud/reference' } }],
            },
          ],
        },
        {
          label: 'AetherContracts',
          items: [
            { label: 'Product Overview', slug: 'aethercontracts' },
            { label: 'Getting Started', slug: 'aethercontracts/getting-started' },
            { label: 'Compatibility', slug: 'aethercontracts/compatibility' },
            { label: 'Conformance', slug: 'aethercontracts/conformance' },
            {
              label: 'Specifications',
              items: [{ autogenerate: { directory: 'aethercontracts/spec' } }],
            },
            {
              label: 'Language Bindings',
              items: [{ autogenerate: { directory: 'aethercontracts/packages' } }],
            },
            { label: 'Migration', slug: 'aethercontracts/migration' },
          ],
        },
        { label: 'Tutorials', items: [{ autogenerate: { directory: 'tutorials' } }] },
        { label: 'Compatibility', items: [{ autogenerate: { directory: 'compatibility' } }] },
        { label: 'Roadmap', items: [{ autogenerate: { directory: 'roadmap' } }] },
      ],
      plugins: [starlightLinksValidator()],
    }),
  ],
});
