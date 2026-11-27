import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { describe, it } from 'node:test';
import { fileURLToPath } from 'node:url';
import {
  buildDocumentRecord,
  buildManifest,
  canonicalUrlFor,
  classifyDocument,
  contentDigest,
  findManifestViolations,
  findProductIndexCoverageViolations,
  readPreviousManifest,
  renderProductLlmsIndex,
} from './build-agent-docs.mjs';

const testDir = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(testDir, '..');

function expect(actual) {
  const toContain = (expected) => assert.ok(actual.includes(expected));
  const toMatch = (expected) => assert.match(actual, expected);

  return {
    toBe: (expected) => assert.equal(actual, expected),
    toContain,
    toEqual: (expected) => assert.deepEqual(actual, expected),
    toMatch,
    toMatchObject: (expected) => {
      const selected = Object.fromEntries(
        Object.keys(expected).map((key) => [key, actual[key]])
      );
      assert.deepEqual(selected, expected);
    },
    not: {
      toContain: (expected) => assert.ok(!actual.includes(expected)),
      toMatch: (expected) => assert.doesNotMatch(actual, expected),
    },
  };
}

describe('Edge agent document metadata', () => {
  it('uses the shared task taxonomy without hiding deep references', () => {
    expect(classifyDocument('docs/guides/connect-devices.md').section).toBe('agent-tasks');
    expect(classifyDocument('docs/guides/deployment.md').section).toBe('operations');
    expect(classifyDocument('docs/guides/safe-operations.md').section).toBe('safety');
    expect(classifyDocument('docs/recovery/configuration-rollback.md').section).toBe('recovery');
    expect(classifyDocument('docs/reference/cli.md').section).toBe('reference');
    expect(classifyDocument('docs/compatibility/version-matrix.md').section).toBe('status');
    expect(classifyDocument('crates/aether-cloudlink/README.md').section).toBe('optional');
  });

  it('builds a version-four record with orthogonal status fields', () => {
    const record = buildDocumentRecord({
      path: 'docs/guides/deployment.md',
      content: '# Deployment\n\nDeploy and verify the edge runtime.\n',
      updated: '2026-07-18',
      published: true,
    });

    expect(record).toMatchObject({
      id: 'edge-docs-guides-deployment',
      path: 'docs/guides/deployment.md',
      canonical_url: 'https://docs.aetheriot.ai/guides/deployment.md',
      title: 'Deployment',
      description: 'Deploy and verify the edge runtime.',
      locale: 'en',
      translation_of: null,
      document_role: 'operations',
      agent_profiles: ['coding-agent', 'operator-agent'],
      intents: ['deploy-and-operate-edge'],
      implementation_status: 'partial',
      production_readiness: 'experimental',
      context_sensitivity: 'public',
      updated: '2026-07-18',
      priority: 'core',
      capability_refs: [],
      preconditions: ['approved-runtime-manifest', 'operator-maintenance-window'],
      recovery_route: 'docs/recovery/configuration-rollback.md',
      human_escalation: null,
      verification: ['verify-runtime-health', 'verify-read-only-observations'],
    });
    expect(JSON.stringify(record)).not.toContain('mixed');
    expect(JSON.stringify(record)).not.toContain('normative');
  });

  it('routes high-risk capability documentation to recovery and human escalation', () => {
    const record = buildDocumentRecord({
      path: 'docs/guides/connect-devices.md',
      content: '# Connect devices\n\nCommission channels and map device points.\n',
      updated: '2026-07-18',
      published: true,
    });

    expect(record.capability_refs).toEqual(['io.channel.manage', 'io.channel.reconcile']);
    expect(record.recovery_route).toBe('docs/recovery/configuration-rollback.md');
    expect(record.human_escalation).toBe('required-for-unknown-or-unsafe-physical-outcome');
  });

  it('uses the unified site only for published English pages and raw URLs for machine files', () => {
    expect(canonicalUrlFor('docs/guides/deployment.md', { published: true })).toBe(
      'https://docs.aetheriot.ai/guides/deployment.md'
    );
    expect(canonicalUrlFor('crates/aether-cloudlink/README.md', { published: true })).toBe(
      'https://docs.aetheriot.ai/crates/aether-cloudlink.md'
    );
    expect(canonicalUrlFor('AGENTS.md', { published: true })).toBe(
      'https://github.com/EvanL1/AetherEdge/blob/main/AGENTS.md'
    );
    expect(canonicalUrlFor('ai/safety-policy.yaml')).toBe(
      'https://raw.githubusercontent.com/EvanL1/AetherEdge/main/ai/safety-policy.yaml'
    );
    expect(canonicalUrlFor('ai/docs-manifest.schema.json')).toBe(
      'https://raw.githubusercontent.com/EvanL1/AetherEdge/main/ai/docs-manifest.schema.json'
    );
  });
});

describe('Edge catalog dates', () => {
  async function sourceTree(files) {
    const root = await fs.mkdtemp(path.join(os.tmpdir(), 'agent-docs-'));
    for (const [relativePath, content] of Object.entries(files)) {
      const target = path.join(root, relativePath);
      await fs.mkdir(path.dirname(target), { recursive: true });
      await fs.writeFile(target, content, 'utf8');
    }
    return root;
  }

  const byPath = (manifest, relativePath) =>
    manifest.documents.find((document) => document.path === relativePath);

  it('hashes content the same way whatever the line endings', () => {
    expect(contentDigest('# Title\n\nBody.\n')).toBe(contentDigest('# Title\r\n\r\nBody.\r\n'));
    expect(contentDigest('# Title\n\nBody.\n')).toMatch(/^[0-9a-f]{64}$/);
    assert.notEqual(contentDigest('# Title\n\nBody.\n'), contentDigest('# Title\n\nOther.\n'));
  });

  it('dates a document by when its content changed, without a Git history', async () => {
    const root = await sourceTree({
      'docs/guides/stable.md': '# Stable\n\nThis page stays as it is.\n',
      'docs/guides/edited.md': '# Edited\n\nThis page is about to change.\n',
    });
    try {
      const first = await buildManifest(root, { today: '2030-01-01' });
      expect(first.documents.map((document) => document.updated)).toEqual([
        '2030-01-01',
        '2030-01-01',
      ]);
      expect(byPath(first, 'docs/guides/stable.md').content_sha256).toBe(
        contentDigest('# Stable\n\nThis page stays as it is.\n')
      );

      await fs.writeFile(
        path.join(root, 'docs/guides/edited.md'),
        '# Edited\n\nThis page has changed.\n',
        'utf8'
      );
      const second = await buildManifest(root, { previous: first, today: '2030-02-02' });
      expect(byPath(second, 'docs/guides/stable.md').updated).toBe('2030-01-01');
      expect(byPath(second, 'docs/guides/edited.md').updated).toBe('2030-02-02');

      const later = await buildManifest(root, { previous: second, today: '2031-03-03' });
      expect(later).toEqual(second);
    } finally {
      await fs.rm(root, { recursive: true, force: true });
    }
  });

  it('does not keep a date that is not a calendar date', async () => {
    const root = await sourceTree({ 'docs/guides/page.md': '# Page\n\nA short page.\n' });
    try {
      const first = await buildManifest(root, { today: '2030-01-01' });
      for (const invalid of ['yesterday', '2030-02-31', '']) {
        const previous = structuredClone(first);
        previous.documents[0].updated = invalid;
        const next = await buildManifest(root, { previous, today: '2030-05-05' });
        expect(next.documents[0].updated).toBe('2030-05-05');
      }
    } finally {
      await fs.rm(root, { recursive: true, force: true });
    }
  });

  it('reproduces the checked-in manifest on any later day', async () => {
    const checkedIn = JSON.parse(
      await fs.readFile(path.join(repoRoot, 'ai', 'docs-manifest.json'), 'utf8')
    );
    expect(await buildManifest(repoRoot, { previous: checkedIn, today: '2099-12-31' })).toEqual(
      checkedIn
    );
  });

  it('reports an entry whose digest no longer matches its file', async () => {
    const manifest = JSON.parse(
      await fs.readFile(path.join(repoRoot, 'ai', 'docs-manifest.json'), 'utf8')
    );
    const stale = manifest.documents.find((document) => document.path === 'AGENTS.md');
    stale.content_sha256 = '0'.repeat(64);
    stale.updated = '2026-02-31';
    expect(await findManifestViolations(manifest, repoRoot)).toEqual([
      'AGENTS.md: updated must be a calendar date',
      'AGENTS.md: content_sha256 does not match the file',
    ]);
  });

  it('refuses a catalog it cannot parse instead of dating every entry today', async () => {
    const root = await sourceTree({
      'ai/docs-manifest.json':
        '{\n<<<<<<< HEAD\n  "schema_version": 4,\n=======\n  "schema_version": 4,\n>>>>>>> branch\n}\n',
    });
    try {
      await assert.rejects(
        readPreviousManifest(path.join(root, 'ai', 'docs-manifest.json')),
        /ai\/docs-manifest\.json cannot be read.*resolve it to one side/s
      );
      expect(await readPreviousManifest(path.join(root, 'ai', 'missing.json'))).toBe(null);
    } finally {
      await fs.rm(root, { recursive: true, force: true });
    }
  });
});

describe('Edge llms.txt generation', () => {
  it('renders every catalog entry exactly once with safety gates and Optional context', () => {
    const manifest = {
      schema_version: 4,
      product: 'AetherEdge',
      documents: [
        buildDocumentRecord({
          path: 'docs/guides/connect-devices.md',
          content: '# Connect devices\n\nConnect heterogeneous devices.\n',
          updated: '2026-07-18',
          published: true,
        }),
        buildDocumentRecord({
          path: 'docs/recovery/gateway-identity.md',
          content: '# Recover gateway identity\n\nRecover a trusted gateway identity.\n',
          updated: '2026-07-18',
          published: true,
        }),
        buildDocumentRecord({
          path: 'crates/aether-cloudlink/README.md',
          content: '# aether-cloudlink\n\nCloudLink protocol semantics.\n',
          updated: '2026-07-18',
        }),
      ],
    };

    const output = renderProductLlmsIndex(manifest);
    expect(output).toContain('## Agent Task Manual');
    expect(output).toContain('## Recovery');
    expect(output).toContain('## Optional');
    expect(output).toContain('Default to read-only.');
    expect(output).toContain('Static documentation does not grant execution authority.');
    expect(output).toContain(
      '[Connect devices](https://docs.aetheriot.ai/guides/connect-devices.md): Connect heterogeneous devices.'
    );
    expect(output).not.toContain('llms-full.txt');
    expect(
      [...output.matchAll(/\]\(([^)]+)\)/g)].every((match) => URL.canParse(match[1]))
    ).toBe(true);
    expect(findProductIndexCoverageViolations(manifest, output)).toEqual([]);
  });

  it('keeps the checked-in manifest, files, and generated index synchronized', async () => {
    const manifest = JSON.parse(
      await fs.readFile(path.join(repoRoot, 'ai', 'docs-manifest.json'), 'utf8')
    );
    const schema = JSON.parse(
      await fs.readFile(path.join(repoRoot, 'ai', 'docs-manifest.schema.json'), 'utf8')
    );
    const index = await fs.readFile(path.join(repoRoot, 'llms.txt'), 'utf8');

    expect(manifest.$schema).toBe(
      'https://raw.githubusercontent.com/EvanL1/AetherEdge/main/ai/docs-manifest.schema.json'
    );
    expect(schema.$id).toBe(
      'https://raw.githubusercontent.com/EvanL1/AetherEdge/main/ai/docs-manifest.schema.json'
    );
    expect(schema.required).toContain('$schema');
    expect(schema.$defs.document.required).toContain('canonical_url');
    expect(manifest.documents.every((document) => URL.canParse(document.canonical_url))).toBe(true);
    const canonicalUrl = new RegExp(schema.$defs.document.properties.canonical_url.pattern);
    expect(
      manifest.documents
        .filter((document) => !canonicalUrl.test(document.canonical_url))
        .map((document) => document.path)
    ).toEqual([]);
    expect(schema.properties.schema_version.const).toBe(manifest.schema_version);
    expect(await findManifestViolations(manifest, repoRoot)).toEqual([]);
    expect(findProductIndexCoverageViolations(manifest, index)).toEqual([]);
    expect([...index.matchAll(/\]\(([^)]+)\)/g)].every((match) => URL.canParse(match[1]))).toBe(
      true
    );
    expect(index).not.toMatch(/[\u3400-\u9fff]/u);
  });
});
