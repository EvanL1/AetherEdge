import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';

const docsSiteRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

function read(relativePath) {
  return readFileSync(path.join(docsSiteRoot, relativePath), 'utf8');
}

describe('bilingual documentation', () => {
  it('serves Simplified Chinese at the root and English under /en', () => {
    const config = read('astro.config.mjs');

    expect(config).toContain("root: { label: '简体中文', lang: 'zh-CN' }");
    expect(config).toContain("en: { label: 'English', lang: 'en' }");
    expect(config).toContain("defaultLocale: 'root'");
  });

  it('publishes English mirrors only inside the English locale', () => {
    const sources = JSON.parse(read('content.sources.json'));
    const byId = Object.fromEntries(sources.sources.map((source) => [source.id, source]));

    expect(byId.aetheredge.destinationPrefix).toBe('en');
    expect(byId.aethercloud.destinationPrefix).toBe('en/aethercloud');
    expect(byId.aethercontracts.destinationPrefix).toBe('en/aethercontracts');
    expect(byId['site-zh-cn'].destinationPrefix).toBe('');
  });

  it('localizes navigation labels without translating product identities', () => {
    const config = read('astro.config.mjs');

    expect(config).toContain("translations: { 'zh-CN': '概览' }");
    expect(config).toContain("translations: { 'zh-CN': '入门' }");
    expect(config).toContain("translations: { 'zh-CN': '协议规格' }");
    expect(config).toContain("translations: { 'zh-CN': '语言绑定' }");
  });

  it('includes Chinese versions of the reported Contracts pages and detailed Cloud docs', () => {
    const manifest = read('content.zh-cn.manifest.txt');

    expect(manifest).toContain('locales/zh-CN/aethercontracts/spec/thing-model-v1alpha1.md');
    expect(manifest).toContain('locales/zh-CN/aethercontracts/packages/cpp.md');
    expect(manifest).toContain('locales/zh-CN/aethercloud/concepts/architecture.md');
    expect(manifest).toContain('locales/zh-CN/aethercloud/guides/plan-infrastructure.md');

    expect(read('locales/zh-CN/aethercontracts/spec/thing-model-v1alpha1.md')).toContain(
      '# Thing Model v1 alpha 1 说明'
    );
    expect(read('locales/zh-CN/aethercontracts/packages/cpp.md')).toContain(
      '# AetherContracts C++ 基础库'
    );
  });

  it('verifies both locales in deployment without rejecting the Chinese publication', () => {
    const workflow = read('../.github/workflows/docs-site-deploy.yml');

    expect(workflow).toContain('test -f dist/en/index.html');
    expect(workflow).toContain('test -f dist/en/llms.txt');
    expect(workflow).toContain('English agent documentation must not contain CJK text.');
    expect(workflow).not.toContain('Published agent documentation must be English-only.');
  });
});
