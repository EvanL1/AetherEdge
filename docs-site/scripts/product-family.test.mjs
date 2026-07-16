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

    expect(overview).toContain('AetherIoT is the open-source project identity');
    expect(overview).toContain('AetherEdge       edge runtime');
    expect(overview).toContain('AetherCloud      cloud fusion');
    expect(overview).toContain('AetherContracts  public specifications');
    expect(overview).toContain('AetherEMS            energy-management solution');
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
});
