import { execFileSync } from 'node:child_process';
import fs from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(scriptDir, '..');
const manifestPath = path.join(repoRoot, 'ai', 'docs-manifest.json');
const indexPath = path.join(repoRoot, 'llms.txt');
const repositoryUrl = 'https://github.com/EvanL1/AetherEdge';
const githubBlobBase = `${repositoryUrl}/blob/main`;
const rawRepositoryBase =
  'https://raw.githubusercontent.com/EvanL1/AetherEdge/main';
const publicDocumentationBase = 'https://docs.aetheriot.dev';
const manifestSchemaUrl = `${rawRepositoryBase}/ai/docs-manifest.schema.json`;

const sectionOrder = [
  'agent-tasks',
  'operations',
  'safety',
  'recovery',
  'reference',
  'status',
  'optional',
];

const sectionLabels = {
  'agent-tasks': 'Agent Task Manual',
  operations: 'Deployment and Operations',
  safety: 'Safety and Governance',
  recovery: 'Recovery',
  reference: 'Platform Reference',
  status: 'Compatibility and Status',
  optional: 'Optional',
};
const sectionByRole = {
  'agent-task': 'agent-tasks',
  operations: 'operations',
  safety: 'safety',
  decision: 'safety',
  recovery: 'recovery',
  reference: 'reference',
  status: 'status',
};

const allowedDocumentRoles = new Set([
  'agent-task',
  'operations',
  'safety',
  'recovery',
  'reference',
  'decision',
  'status',
]);
const allowedAgentProfiles = new Set(['coding-agent', 'operator-agent', 'runtime-agent']);
const allowedImplementationStatuses = new Set([
  'implemented',
  'partial',
  'planned',
  'deprecated',
]);
const allowedProductionReadiness = new Set([
  'production-ready',
  'experimental',
  'not-production-ready',
  'not-applicable',
]);
const allowedContextSensitivity = new Set([
  'public',
  'internal',
  'redacted-only',
  'sensitive-never-load',
]);
const allowedPriorities = new Set(['core', 'optional']);

const rootDocuments = ['AGENTS.md', 'ARCHITECTURE.md', 'README.md'];
const documentRoots = ['ai', 'contracts', 'crates', 'docs', 'extensions', 'skills'];
const englishMetadataOverrides = {
  'README.md': {
    title: 'AetherEdge',
    description:
      'Repository overview, current product status, installation paths, architecture boundaries, and development entry points.',
  },
  'docs/AETHER_CLI_GUIDE.md': {
    title: 'Legacy CLI guide',
    description:
      'Migration-period CLI guide retained for historical lookup; the current CLI reference remains authoritative.',
  },
  'docs/API_REFERENCE.md': {
    title: 'Legacy API reference entry',
    description:
      'Compatibility entry that routes readers to service-owned OpenAPI instead of duplicating endpoint definitions.',
  },
  'docs/CONFIG_FORMAT_GUIDE.md': {
    title: 'Legacy AetherEMS configuration formats',
    description:
      'Historical AetherEMS configuration material retained for migration; it is not AetherEdge configuration authority.',
  },
  'docs/GETTING_STARTED_DEVELOPMENT.md': {
    title: 'Legacy AetherEMS development quickstart',
    description:
      'Historical pre-split development instructions retained for migration and not current AetherEdge onboarding.',
  },
  'docs/README.md': {
    title: 'Repository documentation map',
    description:
      'Navigation for repository documentation, current authority records, compatibility notes, and historical material.',
  },
  'docs/adr/0020-home-assistant-edge-bridge.md': {
    title: 'ADR-0020: Integrate Home Assistant through an Edge bridge',
    description:
      'Accepted staged decision for delegated topology, observations, governed actions, resynchronization, backpressure, and authority.',
  },
  'docs/architecture/openclaw-comparison.md': {
    title: 'Historical AetherEMS and OpenClaw comparison',
    description:
      'Pre-split architecture comparison retained as historical product research, not current AetherEdge authority.',
  },
  'docs/benchmarking.md': {
    title: 'Historical AetherEMS competitive research',
    description:
      'Pre-split energy-product research retained for history; AetherEMS owns current solution strategy.',
  },
  'docs/operations-log.md': {
    title: 'Historical AetherEMS operations log',
    description:
      'Pre-split operational notes retained as optional history and not a current runbook or architecture authority.',
  },
  'docs/plans/2026-05-21-onchange-trigger.md': {
    title: 'On-change trigger design',
    description:
      'Historical design for event-driven rule scheduling retained as optional implementation context.',
  },
  'docs/plans/2026-05-24-redis-removal-strategy.md': {
    title: 'Redis removal strategy',
    description:
      'Historical migration plan for removing Redis from the industry-neutral Edge runtime.',
  },
  'docs/plans/2026-05-28-point-watch-design.md': {
    title: 'PointWatch design',
    description:
      'Historical PointWatch design record retained as optional implementation context.',
  },
  'docs/superpowers/plans/2026-07-09-cli-web-parity.md': {
    title: 'Historical CLI and Web parity plan',
    description:
      'Superseded pre-split console plan; do not use it as current application-boundary authority.',
  },
  'docs/superpowers/specs/2026-07-09-cli-web-parity-design.md': {
    title: 'Historical CLI and Web parity design',
    description:
      'Superseded pre-split console design retained for migration history.',
  },
  'docs/superpowers/specs/2026-07-10-ai-native-docs-design.md': {
    title: 'Historical AI-native documentation design',
    description:
      'Superseded pre-split documentation design retained for migration history.',
  },
  'docs/superpowers/specs/2026-07-10-baremetal-install-design.md': {
    title: 'Historical bare-metal installation design',
    description:
      'Superseded design that bundled Redis, nginx, and a Web UI; current deployment guidance is authoritative.',
  },
  'docs/superpowers/specs/2026-07-11-aether-docs-site-design.md': {
    title: 'Unified documentation site design',
    description:
      'Historical design record for Cloudflare-hosted HTML, Markdown representations, and agent indexes.',
  },
  'docs/websocket-rule-monitor-api.md': {
    title: 'Legacy rule-monitor WebSocket API',
    description:
      'Historical rule-monitoring interface retained for compatibility lookup.',
  },
};
const governanceMetadataOverrides = {
  'docs/guides/ai-assistants.md': {
    capability_refs: [
      'device.write_point',
      'automation.rule.execute',
      'automation.rule.manage',
      'automation.routing.manage',
      'automation.instance.manage',
      'io.channel.manage',
      'io.channel.reconcile',
      'alarm.rule.manage',
      'alarm.alert.resolve',
    ],
    preconditions: ['authenticated-agent', 'live-capability-discovery', 'explicit-confirmation'],
    recovery_route: 'docs/recovery/safe-stop-and-control-revocation.md',
    human_escalation: 'required-for-unknown-or-unsafe-physical-outcome',
    verification: ['verify-terminal-audit', 'verify-authoritative-state'],
  },
  'docs/guides/connect-devices.md': {
    capability_refs: ['io.channel.manage', 'io.channel.reconcile'],
    preconditions: ['commissioned-site', 'current-configuration-revision', 'explicit-confirmation'],
    recovery_route: 'docs/recovery/configuration-rollback.md',
    human_escalation: 'required-for-unknown-or-unsafe-physical-outcome',
    verification: ['verify-channel-status', 'verify-read-only-observations'],
  },
  'docs/guides/data-processors.md': {
    capability_refs: [
      'data_processing.tasks.list',
      'data_processing.processors.health',
      'data_processing.process',
    ],
    preconditions: ['validated-task-and-binding', 'bounded-processor-route'],
    recovery_route: null,
    human_escalation: 'required-for-unknown-non-idempotent-processor-outcome',
    verification: ['verify-derived-result-contract', 'verify-audit-record'],
  },
  'docs/guides/deployment.md': {
    capability_refs: [],
    preconditions: ['approved-runtime-manifest', 'operator-maintenance-window'],
    recovery_route: 'docs/recovery/configuration-rollback.md',
    human_escalation: null,
    verification: ['verify-runtime-health', 'verify-read-only-observations'],
  },
  'docs/guides/home-assistant.md': {
    capability_refs: [],
    preconditions: [
      'explicit-feature-opt-in',
      'edge-local-secret-reference',
      'live-topology-generation',
    ],
    recovery_route: 'docs/recovery/safe-stop-and-control-revocation.md',
    human_escalation: 'required-for-unknown-or-unsafe-physical-outcome',
    verification: ['verify-read-only-projection', 'verify-control-remains-default-off'],
  },
  'docs/guides/safe-operations.md': {
    capability_refs: [
      'device.write_point',
      'automation.rule.execute',
      'automation.rule.manage',
      'automation.routing.manage',
      'automation.instance.manage',
      'io.channel.manage',
      'io.channel.reconcile',
      'alarm.rule.manage',
      'alarm.alert.resolve',
    ],
    preconditions: ['authenticated-actor', 'live-capability-discovery', 'explicit-confirmation'],
    recovery_route: 'docs/recovery/safe-stop-and-control-revocation.md',
    human_escalation: 'required-for-unknown-or-unsafe-physical-outcome',
    verification: ['verify-terminal-audit', 'verify-authoritative-state'],
  },
  'docs/guides/writing-rules.md': {
    capability_refs: [
      'automation.rule.execute',
      'automation.rule.manage',
      'automation.routing.manage',
    ],
    preconditions: ['current-rule-revision', 'explicit-confirmation'],
    recovery_route: 'docs/recovery/safe-stop-and-control-revocation.md',
    human_escalation: 'required-for-unknown-or-unsafe-physical-outcome',
    verification: ['verify-rule-revision', 'verify-scheduler-reconciliation'],
  },
  'docs/reference/mcp-tools.md': {
    capability_refs: [
      'device.read_point',
      'device.write_point',
      'data_processing.tasks.list',
      'data_processing.processors.health',
      'data_processing.process',
      'automation.rule.execute',
      'automation.rule.manage',
      'automation.routing.manage',
      'automation.instance.manage',
      'io.channel.manage',
      'io.channel.reconcile',
      'alarm.rule.manage',
      'alarm.alert.resolve',
    ],
    preconditions: ['authenticated-agent', 'live-capability-discovery'],
    recovery_route: 'docs/recovery/safe-stop-and-control-revocation.md',
    human_escalation: 'required-for-unknown-or-unsafe-physical-outcome',
    verification: ['verify-tool-receipt', 'verify-terminal-audit'],
  },
  'docs/recovery/configuration-rollback.md': {
    capability_refs: ['io.channel.manage', 'io.channel.reconcile'],
    preconditions: ['known-good-configuration-artifact', 'exclusive-offline-maintenance'],
    recovery_route: null,
    human_escalation: 'required-when-authority-or-commit-state-is-unknown',
    verification: ['verify-configuration-revisions', 'verify-shm-topology-generation'],
  },
  'docs/recovery/safe-stop-and-control-revocation.md': {
    capability_refs: [
      'automation.rule.manage',
      'automation.routing.manage',
      'io.channel.manage',
    ],
    preconditions: ['site-safety-procedure', 'current-authoritative-revision'],
    recovery_route: null,
    human_escalation: 'always-available-independent-human-safety-path',
    verification: ['verify-physical-feedback', 'verify-control-permission-revocation'],
  },
};

function normalizedPath(value) {
  return value.split(path.sep).join('/');
}

function globPatternToRegExp(pattern) {
  let expression = '^';
  for (let index = 0; index < pattern.length; index += 1) {
    const character = pattern[index];
    if (character === '*' && pattern[index + 1] === '*') {
      expression += '.*';
      index += 1;
    } else if (character === '*') {
      expression += '[^/]*';
    } else if (character === '?') {
      expression += '[^/]';
    } else {
      expression += character.replace(/[\\^$.*+?()[\]{}|]/g, '\\$&');
    }
  }
  return new RegExp(`${expression}$`);
}

async function discoverPublishedPaths(root, candidatePaths) {
  let manifest;
  try {
    manifest = await fs.readFile(
      path.join(root, 'ai', 'public-docs.manifest.txt'),
      'utf8'
    );
  } catch (error) {
    if (error.code === 'ENOENT') return new Set();
    throw error;
  }
  const patterns = manifest
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line !== '' && !line.startsWith('#'))
    .map(globPatternToRegExp);
  return new Set(
    candidatePaths.filter((relativePath) =>
      patterns.some((pattern) => pattern.test(relativePath))
    )
  );
}

async function walk(directory) {
  const entries = await fs.readdir(directory, { withFileTypes: true });
  const nested = await Promise.all(
    entries
      .filter((entry) => !['node_modules', 'target', '.git'].includes(entry.name))
      .map(async (entry) => {
        const entryPath = path.join(directory, entry.name);
        if (entry.isDirectory()) return walk(entryPath);
        return [entryPath];
      })
  );
  return nested.flat();
}

function isAgentReadablePath(relativePath) {
  if (relativePath === 'ai/docs-manifest.json') return false;
  if (relativePath === 'ai/docs-manifest.schema.json') return false;
  if (relativePath.endsWith('.md')) return true;
  return (
    relativePath === 'ai/catalog.yaml' ||
    relativePath === 'ai/safety-policy.yaml' ||
    /^ai\/evals\/[^/]+\.ya?ml$/.test(relativePath)
  );
}

export async function discoverAgentReadablePaths(root = repoRoot) {
  const existingRootDocuments = [];
  for (const relativePath of rootDocuments) {
    try {
      await fs.access(path.join(root, relativePath));
      existingRootDocuments.push(relativePath);
    } catch {
      // A reduced source fixture may omit root documents.
    }
  }

  const nested = [];
  for (const directory of documentRoots) {
    const absoluteDirectory = path.join(root, directory);
    try {
      const files = await walk(absoluteDirectory);
      nested.push(
        ...files
          .map((filePath) => normalizedPath(path.relative(root, filePath)))
          .filter(isAgentReadablePath)
      );
    } catch (error) {
      if (error.code !== 'ENOENT') throw error;
    }
  }

  return [...new Set([...existingRootDocuments, ...nested])].sort();
}

function parseFrontmatter(content) {
  const match = content.match(/^---\n([\s\S]*?)\n---\n?([\s\S]*)$/);
  if (!match) return { metadata: '', body: content };
  return { metadata: match[1], body: match[2] };
}

function parseScalar(value) {
  const trimmed = value.trim();
  if (trimmed.startsWith('"')) {
    try {
      return JSON.parse(trimmed);
    } catch {
      return trimmed.slice(1, -1);
    }
  }
  if (trimmed.startsWith("'") && trimmed.endsWith("'")) return trimmed.slice(1, -1);
  return trimmed;
}

function cleanInlineMarkdown(value) {
  return value
    .replace(/\[([^\]]+)\]\([^)]+\)/g, '$1')
    .replace(/[`*_]/g, '')
    .replace(/\s+/g, ' ')
    .trim();
}

function truncateDescription(value) {
  if (value.length <= 240) return value;
  const candidate = value.slice(0, 239);
  const boundary = candidate.lastIndexOf(' ');
  return `${candidate.slice(0, boundary > 120 ? boundary : candidate.length).trimEnd()}…`;
}

function extractTitleAndDescription(relativePath, content) {
  const override = englishMetadataOverrides[relativePath];
  if (override) return override;

  if (!relativePath.endsWith('.md')) {
    const title = path.basename(relativePath);
    return {
      title,
      description: `Machine-readable AetherEdge ${title} resource.`,
    };
  }

  const { metadata, body } = parseFrontmatter(content);
  const frontmatterTitle = metadata.match(/^title:\s*(.+)$/m);
  const heading = body.match(/^#\s+(.+)$/m);
  const title = cleanInlineMarkdown(
    frontmatterTitle ? parseScalar(frontmatterTitle[1]) : heading?.[1] ?? relativePath
  );
  const frontmatterDescription = metadata.match(/^description:\s*(.+)$/m);
  if (frontmatterDescription) {
    return {
      title,
      description: cleanInlineMarkdown(parseScalar(frontmatterDescription[1])),
    };
  }

  const withoutHeading = body.replace(/^#\s+.+$/m, '');
  const paragraphs = withoutHeading.split(/\n\s*\n/);
  const paragraph =
    paragraphs.find((candidate) => {
      const trimmed = candidate.trim();
      return (
        trimmed !== '' &&
        !trimmed.startsWith('#') &&
        !trimmed.startsWith('```') &&
        !trimmed.startsWith('|') &&
        !trimmed.startsWith('- ') &&
        !/^\d+\.\s/.test(trimmed)
      );
    }) ?? '';
  const description = cleanInlineMarkdown(paragraph.replace(/^>\s?/gm, ' '));
  return {
    title,
    description:
      truncateDescription(description) ||
      `Read the authoritative AetherEdge resource at ${relativePath}.`,
  };
}

function isOptionalPath(relativePath) {
  return (
    relativePath.startsWith('crates/') ||
    relativePath.startsWith('extensions/') ||
    relativePath.startsWith('contracts/') ||
    relativePath.startsWith('docs/adr/') ||
    relativePath.startsWith('docs/domain/') ||
    relativePath.startsWith('docs/plans/') ||
    relativePath.startsWith('docs/superpowers/') ||
    relativePath === 'docs/operations-log.md'
  );
}

export function classifyDocument(relativePath) {
  const lower = relativePath.toLowerCase();
  const optional = isOptionalPath(relativePath);

  if (optional) {
    return {
      section: 'optional',
      documentRole: lower.startsWith('docs/adr/') ? 'decision' : 'reference',
      intent: 'inspect-edge-implementation',
    };
  }
  if (
    lower.includes('/recovery/') ||
    /(recover|rollback|reconnect|restore|revocation)/.test(path.basename(lower))
  ) {
    return {
      section: 'recovery',
      documentRole: 'recovery',
      intent: 'recover-edge-runtime',
    };
  }
  if (
    lower.includes('/compatibility/') ||
    lower.includes('/roadmap/') ||
    lower.includes('status')
  ) {
    return {
      section: 'status',
      documentRole: 'status',
      intent: 'check-edge-compatibility-and-status',
    };
  }
  if (
    relativePath === 'AGENTS.md' ||
    lower.includes('/security/') ||
    lower.includes('safe-operations') ||
    lower === 'ai/invariants.md' ||
    lower === 'ai/safety-policy.yaml'
  ) {
    return {
      section: 'safety',
      documentRole: 'safety',
      intent: 'govern-edge-command',
    };
  }
  if (
    lower.includes('/deployment') ||
    lower.includes('/migration/') ||
    lower.includes('/configuration') ||
    lower.includes('operations') ||
    lower.includes('getting-started-development')
  ) {
    return {
      section: 'operations',
      documentRole: 'operations',
      intent: 'deploy-and-operate-edge',
    };
  }
  if (
    lower.startsWith('docs/guides/') ||
    lower.startsWith('ai/runbooks/') ||
    lower.startsWith('skills/')
  ) {
    return {
      section: 'agent-tasks',
      documentRole: 'agent-task',
      intent: 'complete-edge-task',
    };
  }
  return {
    section: 'reference',
    documentRole: 'reference',
    intent: 'understand-edge-platform',
  };
}

function agentProfilesFor(section) {
  if (section === 'recovery') return ['operator-agent', 'runtime-agent'];
  if (section === 'safety') return ['coding-agent', 'operator-agent', 'runtime-agent'];
  if (section === 'reference' || section === 'status' || section === 'optional') {
    return ['coding-agent', 'operator-agent'];
  }
  return ['coding-agent', 'operator-agent'];
}

function implementationStatusFor(relativePath, classification) {
  if (relativePath.startsWith('docs/plans/')) return 'planned';
  if (relativePath.startsWith('docs/superpowers/plans/')) return 'planned';
  if (classification.documentRole === 'safety' && relativePath === 'AGENTS.md') {
    return 'implemented';
  }
  if (relativePath === 'README.md' || relativePath === 'ARCHITECTURE.md') return 'implemented';
  return 'partial';
}

function productionReadinessFor(classification, implementationStatus) {
  if (
    classification.documentRole === 'decision' ||
    classification.documentRole === 'reference' ||
    classification.documentRole === 'status' ||
    classification.documentRole === 'safety'
  ) {
    return 'not-applicable';
  }
  if (implementationStatus === 'planned') return 'not-production-ready';
  return 'experimental';
}

function contextSensitivityFor(relativePath) {
  if (
    relativePath === 'AGENTS.md' ||
    relativePath.startsWith('ai/') ||
    relativePath.startsWith('skills/') ||
    relativePath.startsWith('docs/adr/') ||
    relativePath.startsWith('docs/plans/') ||
    relativePath.startsWith('docs/superpowers/')
  ) {
    return 'internal';
  }
  return 'public';
}

function documentId(relativePath) {
  const stem = relativePath.replace(/\.(md|ya?ml)$/i, '');
  return `edge-${stem.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '')}`;
}

function publishedMarkdownSlug(relativePath) {
  let destination = relativePath;
  if (destination.startsWith('docs/')) {
    destination = destination.slice('docs/'.length);
  } else if (destination.endsWith('/README.md')) {
    destination = `${destination.slice(0, -'/README.md'.length)}.md`;
  }
  const segments = path.posix
    .join('en', destination)
    .replace(/\.md$/i, '')
    .split('/')
    .filter(Boolean)
    .map((segment) =>
      segment
        .normalize('NFC')
        .toLowerCase()
        .replace(/[^\p{Letter}\p{Mark}\p{Number}_ -]/gu, '')
        .replaceAll(' ', '-')
    );
  if (segments.at(-1) === 'index') segments.pop();
  return segments.join('/');
}

export function canonicalUrlFor(relativePath, options = {}) {
  const encodedPath = relativePath
    .split('/')
    .map((segment) => encodeURIComponent(segment))
    .join('/');
  if (/\.(?:json|ya?ml)$/i.test(relativePath)) {
    return `${rawRepositoryBase}/${encodedPath}`;
  }
  if (
    options.published === true &&
    relativePath.endsWith('.md') &&
    contextSensitivityFor(relativePath) === 'public'
  ) {
    return `${publicDocumentationBase}/${publishedMarkdownSlug(relativePath)}.md`;
  }
  return `${githubBlobBase}/${encodedPath}`;
}

export function buildDocumentRecord({ path: relativePath, content, updated, published = false }) {
  const classification = classifyDocument(relativePath);
  const { title, description } = extractTitleAndDescription(relativePath, content);
  const implementationStatus = implementationStatusFor(relativePath, classification);
  const governance = governanceMetadataOverrides[relativePath] ?? {
    capability_refs: [],
    preconditions: [],
    recovery_route: null,
    human_escalation: null,
    verification: [],
  };
  return {
    id: documentId(relativePath),
    path: relativePath,
    canonical_url: canonicalUrlFor(relativePath, { published }),
    title,
    description,
    locale: 'en',
    translation_of: null,
    document_role: classification.documentRole,
    agent_profiles: agentProfilesFor(classification.section),
    intents: [classification.intent],
    implementation_status: implementationStatus,
    production_readiness: productionReadinessFor(classification, implementationStatus),
    context_sensitivity: contextSensitivityFor(relativePath),
    updated,
    priority: classification.section === 'optional' ? 'optional' : 'core',
    media_type: relativePath.endsWith('.md') ? 'text/markdown' : 'application/yaml',
    capability_refs: governance.capability_refs,
    preconditions: governance.preconditions,
    recovery_route: governance.recovery_route,
    human_escalation: governance.human_escalation,
    verification: governance.verification,
  };
}

function lastCommittedDate(root, relativePath) {
  try {
    const value = execFileSync('git', ['log', '-1', '--format=%cs', '--', relativePath], {
      cwd: root,
      encoding: 'utf8',
    }).trim();
    return value || '2026-07-18';
  } catch {
    return '2026-07-18';
  }
}

export async function buildManifest(root = repoRoot) {
  const paths = await discoverAgentReadablePaths(root);
  const publishedPaths = await discoverPublishedPaths(root, paths);
  const documents = await Promise.all(
    paths.map(async (relativePath) =>
      buildDocumentRecord({
        path: relativePath,
        content: await fs.readFile(path.join(root, relativePath), 'utf8'),
        updated: lastCommittedDate(root, relativePath),
        published: publishedPaths.has(relativePath),
      })
    )
  );
  return {
    $schema: manifestSchemaUrl,
    schema_version: 3,
    project: 'AetherEdge',
    repository: repositoryUrl,
    scope:
      'Complete agent-readable AetherEdge repository catalog. The public documentation site applies a separate publication allowlist.',
    generated_by: 'scripts/build-agent-docs.mjs',
    documents,
  };
}

function valuesAreAllowed(values, allowed) {
  return Array.isArray(values) && values.length > 0 && values.every((value) => allowed.has(value));
}

function parseSafetyCapabilities(source) {
  const capabilities = new Map();
  const capabilityBlock = source.split(/^capabilities:\s*$/m)[1] ?? '';
  let current = null;
  for (const line of capabilityBlock.split('\n')) {
    const capability = line.match(/^  ([a-z0-9_.-]+):\s*$/);
    if (capability) {
      current = capability[1];
      capabilities.set(current, {});
      continue;
    }
    const property = line.match(/^    ([a-z_]+):\s*(.+?)\s*$/);
    if (current && property) {
      capabilities.get(current)[property[1]] = property[2];
    }
  }
  return capabilities;
}

export async function findManifestViolations(manifest, root = repoRoot) {
  const violations = [];
  if (manifest.$schema !== manifestSchemaUrl) {
    violations.push(`$schema must be ${manifestSchemaUrl}`);
  }
  if (manifest.schema_version !== 3) violations.push('schema_version must be 3');
  if (manifest.project !== 'AetherEdge') violations.push('project must be AetherEdge');
  if (!Array.isArray(manifest.documents)) {
    return [...violations, 'documents must be an array'];
  }

  const ids = new Set();
  const paths = new Set();
  const requiredStrings = [
    'id',
    'path',
    'canonical_url',
    'title',
    'description',
    'locale',
    'document_role',
    'implementation_status',
    'production_readiness',
    'context_sensitivity',
    'updated',
    'priority',
  ];
  let safetyCapabilities = new Map();
  const discovered = await discoverAgentReadablePaths(root);
  const publishedPaths = await discoverPublishedPaths(root, discovered);
  try {
    safetyCapabilities = parseSafetyCapabilities(
      await fs.readFile(path.join(root, 'ai', 'safety-policy.yaml'), 'utf8')
    );
  } catch {
    violations.push('ai/safety-policy.yaml must exist and be readable');
  }
  for (const document of manifest.documents) {
    const label = document.path ?? document.id ?? '<unknown>';
    for (const field of requiredStrings) {
      if (typeof document[field] !== 'string' || document[field].trim() === '') {
        violations.push(`${label}: ${field} must be a non-empty string`);
      }
    }
    const expectedCanonicalUrl = canonicalUrlFor(document.path, {
      published: publishedPaths.has(document.path),
    });
    if (document.canonical_url !== expectedCanonicalUrl) {
      violations.push(`${label}: canonical_url must be ${expectedCanonicalUrl}`);
    }
    if (document.translation_of !== null && typeof document.translation_of !== 'string') {
      violations.push(`${label}: translation_of must be a document id or null`);
    }
    if (!allowedDocumentRoles.has(document.document_role)) {
      violations.push(`${label}: invalid document_role ${document.document_role}`);
    }
    if (!valuesAreAllowed(document.agent_profiles, allowedAgentProfiles)) {
      violations.push(`${label}: invalid or empty agent_profiles`);
    }
    if (!Array.isArray(document.intents) || document.intents.length === 0) {
      violations.push(`${label}: intents must be non-empty`);
    }
    if (!allowedImplementationStatuses.has(document.implementation_status)) {
      violations.push(`${label}: invalid implementation_status ${document.implementation_status}`);
    }
    if (!allowedProductionReadiness.has(document.production_readiness)) {
      violations.push(`${label}: invalid production_readiness ${document.production_readiness}`);
    }
    if (!allowedContextSensitivity.has(document.context_sensitivity)) {
      violations.push(`${label}: invalid context_sensitivity ${document.context_sensitivity}`);
    }
    if (!allowedPriorities.has(document.priority)) {
      violations.push(`${label}: invalid priority ${document.priority}`);
    }
    for (const field of ['capability_refs', 'preconditions', 'verification']) {
      if (!Array.isArray(document[field])) {
        violations.push(`${label}: ${field} must be an array`);
      }
    }
    if (document.recovery_route !== null && typeof document.recovery_route !== 'string') {
      violations.push(`${label}: recovery_route must be a path or null`);
    }
    if (document.human_escalation !== null && typeof document.human_escalation !== 'string') {
      violations.push(`${label}: human_escalation must be a string or null`);
    }
    for (const capabilityRef of document.capability_refs ?? []) {
      if (!safetyCapabilities.has(capabilityRef)) {
        violations.push(`${label}: unknown capability_ref ${capabilityRef}`);
      }
      if (
        safetyCapabilities.get(capabilityRef)?.risk === 'high' &&
        ((document.document_role !== 'recovery' && document.recovery_route === null) ||
          document.human_escalation === null)
      ) {
        violations.push(
          `${label}: high-risk capability ${capabilityRef} requires recovery and human escalation`
        );
      }
    }
    if (typeof document.recovery_route === 'string') {
      try {
        await fs.access(path.join(root, document.recovery_route));
      } catch {
        violations.push(`${label}: recovery_route does not exist`);
      }
    }
    if (ids.has(document.id)) violations.push(`${label}: duplicate id ${document.id}`);
    if (paths.has(document.path)) violations.push(`${label}: duplicate path ${document.path}`);
    ids.add(document.id);
    paths.add(document.path);
    try {
      await fs.access(path.join(root, document.path));
    } catch {
      violations.push(`${label}: path does not exist`);
    }
  }

  for (const document of manifest.documents) {
    if (document.translation_of !== null && !ids.has(document.translation_of)) {
      violations.push(`${document.path}: translation_of does not resolve`);
    }
  }

  for (const relativePath of discovered) {
    if (!paths.has(relativePath)) violations.push(`${relativePath}: missing from manifest`);
  }
  for (const relativePath of paths) {
    if (!discovered.includes(relativePath)) {
      violations.push(`${relativePath}: manifest entry is outside the catalog scope`);
    }
  }
  return violations;
}

export function renderProductLlmsIndex(manifest) {
  const lines = [
    '# AetherEdge',
    '',
    '> Complete agent index for the deterministic AetherIoT edge runtime, Kernel, CLI, SDK, contracts, and implementation references.',
    '',
    'Default to read-only. Static documentation does not grant execution authority. Before any write, load the safety policy and query the live application capability catalog. Never automatically retry a command whose outcome is unknown, timed out, or missing audit evidence. AetherEdge retains final authority over physical execution.',
    '',
    'Use the narrowest document that answers the task. The catalog covers every agent-readable Edge document; deep implementation records remain available under Optional.',
    '',
    `- [Machine-readable document catalog](${rawRepositoryBase}/ai/docs-manifest.json)`,
    `- [Document catalog schema](${manifestSchemaUrl})`,
    '',
  ];

  for (const section of sectionOrder) {
    lines.push(`## ${sectionLabels[section]}`, '');
    const documents = manifest.documents
      .filter((document) => {
        const documentSection =
          document.priority === 'optional' ? 'optional' : sectionByRole[document.document_role];
        return documentSection === section;
      })
      .sort((left, right) => left.path.localeCompare(right.path));
    if (documents.length === 0) {
      lines.push('- No catalog entries.', '');
      continue;
    }
    for (const document of documents) {
      lines.push(
        `- [${document.title}](${document.canonical_url})${
          document.description ? `: ${document.description}` : ''
        }`
      );
    }
    lines.push('');
  }
  return `${lines.join('\n').trim()}\n`;
}

export function findProductIndexCoverageViolations(manifest, index) {
  const destinations = [...index.matchAll(/\]\(([^)]+)\)/g)].map((match) => match[1]);
  return manifest.documents.flatMap((document) => {
    const count = destinations.filter(
      (destination) => destination.split('#', 1)[0] === document.canonical_url
    ).length;
    if (count === 1) return [];
    return [{ path: document.path, count }];
  });
}

async function main() {
  const mode = process.argv[2] ?? '--check';
  if (!['--check', '--write'].includes(mode)) {
    throw new Error('Usage: node scripts/build-agent-docs.mjs [--check|--write]');
  }

  const manifest = await buildManifest(repoRoot);
  const index = renderProductLlmsIndex(manifest);
  const manifestText = `${JSON.stringify(manifest, null, 2)}\n`;
  if (mode === '--write') {
    await fs.writeFile(manifestPath, manifestText, 'utf8');
    await fs.writeFile(indexPath, index, 'utf8');
  } else {
    const [checkedManifest, checkedIndex] = await Promise.all([
      fs.readFile(manifestPath, 'utf8'),
      fs.readFile(indexPath, 'utf8'),
    ]);
    if (checkedManifest !== manifestText) {
      throw new Error('ai/docs-manifest.json is stale; run with --write');
    }
    if (checkedIndex !== index) {
      throw new Error('llms.txt is stale; run with --write');
    }
  }

  const violations = await findManifestViolations(manifest, repoRoot);
  violations.push(...findProductIndexCoverageViolations(manifest, index).map(JSON.stringify));
  if (violations.length > 0) {
    throw new Error(`Agent documentation violations:\n${violations.join('\n')}`);
  }
  console.log(
    `build-agent-docs: ${mode === '--write' ? 'wrote' : 'verified'} ${
      manifest.documents.length
    } entries`
  );
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main().catch((error) => {
    console.error(error);
    process.exitCode = 1;
  });
}
