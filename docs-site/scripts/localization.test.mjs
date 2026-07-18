import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';

const docsSiteRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

function read(relativePath) {
  return readFileSync(path.join(docsSiteRoot, relativePath), 'utf8');
}

function visibleMarkdownProse(content) {
  const lines = content.split('\n');
  const visible = [];
  let fence = null;

  for (const line of lines) {
    const marker = line.trim().match(/^(```|~~~)/);
    if (marker) {
      fence = fence === null ? marker[1] : null;
      continue;
    }
    if (fence !== null) continue;
    visible.push(line.replace(/`[^`]*`/g, ''));
  }

  return visible.join('\n');
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

  it('publishes integration, recovery, governance, and migration translations once each', () => {
    const manifestEntries = read('content.zh-cn.manifest.txt')
      .split('\n')
      .map((line) => line.trim())
      .filter((line) => line && !line.startsWith('#'));
    const expected = [
      'locales/zh-CN/guides/home-assistant.md',
      'locales/zh-CN/guides/edge-contracts-cloud.md',
      'locales/zh-CN/aethercloud/concepts/home-assistant-integration.md',
      'locales/zh-CN/aethercloud/concepts/home-assistant-governed-control.md',
      'locales/zh-CN/aethercontracts/spec/integration-v1alpha1.md',
      'locales/zh-CN/aethercontracts/spec/integration-control-v1alpha1.md',
      'locales/zh-CN/aethercontracts/integration.md',
      'locales/zh-CN/aethercontracts/migrations/alpha3-to-alpha4.md',
      'locales/zh-CN/aethercontracts/SECURITY.md',
      'locales/zh-CN/aethercontracts/GOVERNANCE.md',
      'locales/zh-CN/crates/aether-integration-contract.md',
      'locales/zh-CN/crates/aether-integration-control.md',
      'locales/zh-CN/recovery/configuration-rollback.md',
      'locales/zh-CN/recovery/gateway-identity-recovery.md',
      'locales/zh-CN/recovery/safe-stop-and-control-revocation.md',
      'locales/zh-CN/recovery/cloudlink-spool-recovery.md',
      'locales/zh-CN/aethercloud/recovery/credential-revocation-and-reenrollment.md',
      'locales/zh-CN/aethercloud/recovery/cloudlink-offline-and-reconnect.md',
      'locales/zh-CN/aethercloud/recovery/unknown-command-outcome.md',
      'locales/zh-CN/aethercloud/recovery/database-backup-and-restore.md',
      'locales/zh-CN/aethercloud/recovery/emergency-revoke.md',
      'locales/zh-CN/aethercloud/recovery/integration-safe-degradation.md',
      'locales/zh-CN/aethercloud/recovery/infrastructure-lock-failure.md',
    ];

    for (const page of expected) {
      expect(manifestEntries.filter((entry) => entry === page)).toHaveLength(1);
    }
  });

  it('verifies both locales in deployment without rejecting the Chinese publication', () => {
    const workflow = read('../.github/workflows/docs-site-deploy.yml');

    expect(workflow).toContain('test -f dist/en/index.html');
    expect(workflow).toContain('test -f dist/en/llms.txt');
    expect(workflow).toContain('node scripts/check-language.mjs dist');
    expect(workflow).not.toContain('rg --pcre2');
    expect(workflow).not.toContain('Published agent documentation must be English-only.');
  });

  it('keeps agent indexes off user homepages and never publishes a full-corpus file', () => {
    const chineseHome = read('locales/zh-CN/index.md');
    const englishHome = read('locales/en/index.md');
    const builder = read('scripts/build-docs.mjs');
    const workflow = read('../.github/workflows/docs-site-deploy.yml');

    expect(chineseHome).not.toContain('llms.txt');
    expect(englishHome).not.toContain('llms.txt');
    expect(builder).not.toContain('renderLlmsFull');
    expect(builder).not.toContain('llms-full.txt');
    expect(workflow).toContain('test ! -e dist/llms-full.txt');
    expect(workflow).toContain('test ! -e dist/en/llms-full.txt');
  });

  it('keeps known untranslated labels and fragments out of Chinese user documentation', () => {
    const manifestEntries = read('content.zh-cn.manifest.txt')
      .split('\n')
      .map((line) => line.trim())
      .filter((line) => line && !line.startsWith('#'));
    const visibleDocuments = manifestEntries.map((relativePath) => ({
      relativePath,
      content: visibleMarkdownProse(read(relativePath)),
    }));
    const forbiddenProse = [
      /\bBroker\b/,
      /(?<!JSON )\bSchema\b/,
      /\bsidecar\b/,
      /\bPlanning\b/,
      /\bDisconnect\b/,
      /\bHealth\b/,
      /\bAgent Skill\b/,
      /\bWeb UI\b/,
      /\bGitHub Releases\b/,
      /\bActive Pack\b/,
      /\. The local server resolves the configured subject from\b/,
    ];
    const proseFailures = visibleDocuments.flatMap(({ relativePath, content }) =>
      forbiddenProse
        .filter((pattern) => pattern.test(content))
        .map((pattern) => `${relativePath}: ${pattern}`)
    );

    expect(proseFailures).toEqual([]);

    const mcpTools = read('locales/zh-CN/reference/mcp-tools.md');
    expect(mcpTools).not.toMatch(/^\|.*\|\s*(?:yes|no)\s*\|/m);
    expect(mcpTools).not.toMatch(
      /^\|.*\|\s*(?:string|boolean|integer)(?:\/null)?\s*\|/m
    );
    expect(mcpTools).not.toMatch(/^\|.*\|\s*[^|]+\/null\s*\|/m);
    expect(mcpTools).not.toContain('**WRITE**');

    const dataProcessing = read('locales/zh-CN/reference/data-processing-contracts.md');
    expect(dataProcessing).not.toMatch(/^\|.*\|\s*(?:yes|no)\s*\|/m);
    expect(dataProcessing).not.toContain('UTC 时间result');

    const configuration = read('locales/zh-CN/reference/configuration.md');
    expect(configuration).not.toMatch(/\|\s*unset\s*\|/);
    expect(configuration).not.toContain('tmpfs path');
    expect(configuration).not.toContain('harness');

    expect(read('locales/zh-CN/compatibility/version-matrix.md')).not.toContain(
      '| Surface |'
    );
    expect(read('locales/zh-CN/concepts/data-model.md')).not.toContain('| Writer |');
    expect(read('locales/zh-CN/guides/deployment.md')).not.toContain(
      '| Container | Image | Role |'
    );
    expect(read('locales/zh-CN/reference/http-api.md')).not.toContain(
      '| 招摇的用户界面 | 打开API JSON |'
    );
  });
});
