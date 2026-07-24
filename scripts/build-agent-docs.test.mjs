import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import path from 'node:path';
import { describe, it } from 'node:test';
import { fileURLToPath } from 'node:url';
import {
  buildDocumentRecord,
  canonicalUrlFor,
  classifyDocument,
  findManifestViolations,
  findProductIndexCoverageViolations,
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
    expect(classifyDocument('docs/adr/0012-agent-first-application-surface.md').section).toBe(
      'optional'
    );
    expect(classifyDocument('crates/aether-cloudlink/README.md').section).toBe('optional');
  });

  it('builds a version-three record with orthogonal status fields', () => {
    const record = buildDocumentRecord({
      path: 'docs/guides/deployment.md',
      content: '# Deployment\n\nDeploy and verify the edge runtime.\n',
      updated: '2026-07-18',
      published: true,
    });

    expect(record).toMatchObject({
      id: 'edge-docs-guides-deployment',
      path: 'docs/guides/deployment.md',
      canonical_url: 'https://docs.aetheriot.dev/en/guides/deployment.md',
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
      'https://docs.aetheriot.dev/en/guides/deployment.md'
    );
    expect(canonicalUrlFor('crates/aether-cloudlink/README.md', { published: true })).toBe(
      'https://docs.aetheriot.dev/en/crates/aether-cloudlink.md'
    );
    expect(
      canonicalUrlFor('docs/adr/0001-ai-native-edge-kernel.md', { published: true })
    ).toBe(
      'https://github.com/EvanL1/AetherEdge/blob/main/docs/adr/0001-ai-native-edge-kernel.md'
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

describe('Edge llms.txt generation', () => {
  it('renders every catalog entry exactly once with safety gates and Optional context', () => {
    const manifest = {
      schema_version: 3,
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
          path: 'docs/adr/0012-agent-first-application-surface.md',
          content: '# Agent-first application surface\n\nRecord the application boundary.\n',
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
      '[Connect devices](https://docs.aetheriot.dev/en/guides/connect-devices.md): Connect heterogeneous devices.'
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
    expect(await findManifestViolations(manifest, repoRoot)).toEqual([]);
    expect(findProductIndexCoverageViolations(manifest, index)).toEqual([]);
    expect([...index.matchAll(/\]\(([^)]+)\)/g)].every((match) => URL.canParse(match[1]))).toBe(
      true
    );
    expect(index).not.toMatch(/[\u3400-\u9fff]/u);
  });
});
