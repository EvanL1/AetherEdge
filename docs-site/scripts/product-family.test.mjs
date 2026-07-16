import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';

const repositoryRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..');

function read(relativePath) {
  return readFileSync(path.join(repositoryRoot, relativePath), 'utf8');
}

describe('AetherIoT product-family documentation', () => {
  it('defines one umbrella project and three core products', () => {
    const overview = read('docs/overview/platform.md');

    expect(overview).toContain('AetherIoT is the open-source, AI-native project identity');
    expect(overview).toContain('AetherEdge       deterministic edge runtime');
    expect(overview).toContain('AetherCloud      evolving agent, fusion');
    expect(overview).toContain('AetherContracts  typed specifications');
    expect(overview).toContain('AetherEMS            energy-management solution');
    expect(overview).toContain('[AI-native platform](ai-native-platform.md)');
  });

  it('pins a tested compatibility baseline without claiming production CloudLink', () => {
    const matrix = read('docs/compatibility/version-matrix.md');

    expect(matrix).toContain('`v0.5.0`');
    expect(matrix).toContain('`v0.1.0-alpha.3`');
    expect(matrix).toContain('Experimental integration baseline');
    expect(matrix).toContain('It is not production');
  });

  it('renames repository-facing identity while preserving software and protocol names', () => {
    const migration = read('docs/migration/aetheriot-to-aetheredge.md');

    expect(migration).toContain('https://github.com/EvanL1/AetherEdge');
    expect(migration).toContain('The `aether` CLI and `aether-*` binaries');
    expect(migration).toContain('CloudLink, Thing Model, Schema, TCK');
    expect(migration).toContain('Never rewrite published artifacts');
  });

  it('publishes detailed Cloud and Contracts source collections', () => {
    const sources = JSON.parse(read('docs-site/content.sources.json'));
    expect(sources.sources.map(({ id }) => id)).toEqual([
      'aetheredge',
      'aethercloud',
      'aethercontracts',
      'site-en',
      'site-zh-cn',
    ]);

    const cloudManifest = read('docs-site/content.aethercloud.manifest.txt');
    expect(cloudManifest).toContain('docs/get-started/*');
    expect(cloudManifest).toContain('docs/concepts/!(current-state-audit).md');
    expect(cloudManifest).not.toContain('\ndocs/concepts/*\n');
    expect(cloudManifest).toContain('docs/guides/*');
    expect(cloudManifest).toContain('docs/reference/*');

    const contractsManifest = read('docs-site/content.aethercontracts.manifest.txt');
    expect(contractsManifest).toContain('docs/getting-started.md');
    expect(contractsManifest).toContain('docs/compatibility.md');
    expect(contractsManifest).toContain('docs/conformance.md');
    expect(contractsManifest).toContain('spec/*');
    expect(contractsManifest).toContain('packages/*/README.md');
  });

  it('gives both products detailed generated sidebars', () => {
    const config = read('docs-site/astro.config.mjs');

    expect(config).toContain("directory: 'aethercloud/concepts'");
    expect(config).toContain("directory: 'aethercloud/guides'");
    expect(config).toContain("directory: 'aethercloud/reference'");
    expect(config).toContain("directory: 'aethercontracts/spec'");
    expect(config).toContain("directory: 'aethercontracts/packages'");
  });

  it('makes pagination direction labels primary and keeps page names compact', () => {
    const config = read('docs-site/astro.config.mjs');
    const styles = read('docs-site/src/styles/custom.css');

    expect(config).toContain("customCss: ['./src/styles/custom.css']");
    expect(styles).toMatch(/\.pagination-links a > span[\s\S]*font-size:\s*1rem/);
    expect(styles).toMatch(/\.pagination-links \.link-title[\s\S]*font-size:\s*0\.9375rem/);
    expect(styles).toContain('text-wrap: balance');
    expect(styles).not.toContain('var(--sl-text-2xl)');
  });
});
