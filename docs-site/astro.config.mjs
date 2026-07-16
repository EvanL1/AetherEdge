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
      locales: {
        root: { label: '简体中文', lang: 'zh-CN' },
        en: { label: 'English', lang: 'en' },
      },
      defaultLocale: 'root',
      social: [
        { icon: 'github', label: 'GitHub', href: 'https://github.com/EvanL1/AetherEdge' },
      ],
      sidebar: [
        {
          label: 'Overview',
          translations: { 'zh-CN': '概览' },
          items: [
            {
              label: 'Platform',
              translations: { 'zh-CN': '平台' },
              slug: 'overview/platform',
            },
            {
              label: 'Deployment Topologies',
              translations: { 'zh-CN': '部署拓扑' },
              slug: 'overview/deployment-topologies',
            },
            {
              label: 'User Journeys',
              translations: { 'zh-CN': '用户旅程' },
              slug: 'overview/user-journeys',
            },
          ],
        },
        {
          label: 'AetherEdge',
          items: [
            {
              label: 'Product Overview',
              translations: { 'zh-CN': '产品总览' },
              slug: 'aetheredge',
            },
            {
              label: 'Agent Quickstart',
              translations: { 'zh-CN': '智能体快速入门' },
              slug: 'agent-quickstart',
            },
            {
              label: 'Getting Started',
              translations: { 'zh-CN': '入门' },
              slug: 'guides/getting-started',
            },
            {
              label: 'Concepts',
              translations: { 'zh-CN': '概念' },
              items: [{ autogenerate: { directory: 'concepts' } }],
            },
            {
              label: 'Guides',
              translations: { 'zh-CN': '指南' },
              items: [{ autogenerate: { directory: 'guides' } }],
            },
            {
              label: 'Reference',
              translations: { 'zh-CN': '参考' },
              items: [{ autogenerate: { directory: 'reference' } }],
            },
            {
              label: 'SDK Crates',
              translations: { 'zh-CN': 'SDK 包' },
              items: [{ autogenerate: { directory: 'crates' } }],
            },
            {
              label: 'Extensions',
              translations: { 'zh-CN': '扩展' },
              items: [{ autogenerate: { directory: 'extensions' } }],
            },
            {
              label: 'Security',
              translations: { 'zh-CN': '安全' },
              items: [{ autogenerate: { directory: 'security' } }],
            },
          ],
        },
        {
          label: 'AetherCloud',
          items: [
            {
              label: 'Product Overview',
              translations: { 'zh-CN': '产品总览' },
              slug: 'aethercloud',
            },
            {
              label: 'Get Started',
              translations: { 'zh-CN': '入门' },
              items: [{ autogenerate: { directory: 'aethercloud/get-started' } }],
            },
            {
              label: 'Concepts',
              translations: { 'zh-CN': '概念' },
              items: [{ autogenerate: { directory: 'aethercloud/concepts' } }],
            },
            {
              label: 'Guides',
              translations: { 'zh-CN': '指南' },
              items: [{ autogenerate: { directory: 'aethercloud/guides' } }],
            },
            {
              label: 'Reference',
              translations: { 'zh-CN': '参考' },
              items: [{ autogenerate: { directory: 'aethercloud/reference' } }],
            },
          ],
        },
        {
          label: 'AetherContracts',
          items: [
            {
              label: 'Product Overview',
              translations: { 'zh-CN': '产品总览' },
              slug: 'aethercontracts',
            },
            {
              label: 'Getting Started',
              translations: { 'zh-CN': '入门' },
              slug: 'aethercontracts/getting-started',
            },
            {
              label: 'Compatibility',
              translations: { 'zh-CN': '兼容性' },
              slug: 'aethercontracts/compatibility',
            },
            {
              label: 'Conformance',
              translations: { 'zh-CN': '符合性' },
              slug: 'aethercontracts/conformance',
            },
            {
              label: 'Specifications',
              translations: { 'zh-CN': '协议规格' },
              items: [{ autogenerate: { directory: 'aethercontracts/spec' } }],
            },
            {
              label: 'Language Bindings',
              translations: { 'zh-CN': '语言绑定' },
              items: [{ autogenerate: { directory: 'aethercontracts/packages' } }],
            },
            {
              label: 'Migration',
              translations: { 'zh-CN': '迁移' },
              slug: 'aethercontracts/migration',
            },
          ],
        },
        {
          label: 'Tutorials',
          translations: { 'zh-CN': '教程' },
          items: [{ autogenerate: { directory: 'tutorials' } }],
        },
        {
          label: 'Compatibility',
          translations: { 'zh-CN': '兼容性' },
          items: [{ autogenerate: { directory: 'compatibility' } }],
        },
        {
          label: 'Roadmap',
          translations: { 'zh-CN': '路线图' },
          items: [{ autogenerate: { directory: 'roadmap' } }],
        },
      ],
      plugins: [starlightLinksValidator()],
    }),
  ],
});
