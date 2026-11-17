import { createHash } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { basename, resolve } from "node:path";
import process from "node:process";

// Reproduces the committed AetherContracts consumer pin from an extracted
// release tree. Usage, from a checkout of the immutable release tag:
//
//   node scripts/sync-aether-contracts-alpha4.mjs <path-to-release-root>
//
// The release root is the directory that holds `contract-manifest.json`. Every
// imported byte is checked against that manifest before it is written.

const version = "0.1.0-alpha.4";
const releaseRoot = resolve(process.argv[2] ?? "");
const consumerRoot = resolve(import.meta.dirname, "..");
const manifestBytes = await readFile(resolve(releaseRoot, "contract-manifest.json"));
const manifest = JSON.parse(manifestBytes.toString("utf8"));

if (manifest.release_version !== version) {
  throw new Error(`expected AetherContracts ${version}`);
}

const digest = (bytes) => createHash("sha256").update(bytes).digest("hex");
const declared = new Map(manifest.artifacts.map((entry) => [entry.path, entry.sha256]));

// The alpha.4 closure is the CloudLink core adopted from alpha.3 plus the
// aether.integration and Integration Control closures that alpha.4 adds. It is
// expressed as rules rather than a flat path list so an upstream addition
// inside a consumed family is picked up, and an upstream removal fails the
// digest check below instead of silently shrinking the lock.
const exactSources = new Set([
  ".github/actions/verify-consumer/action.yml",
  "compatibility/cloudlink-v1alpha1-gates.json",
  "compatibility/failure-codes.json",
  "compatibility/integration-control-v1alpha1.json",
  "profiles/mqtt/v1alpha1/profile.json",
  "schemas/distribution/v1alpha1/consumer-lock.schema.json",
  "schemas/tck/v1alpha1/fixture-manifest.schema.json",
  "schemas/tck/v1alpha1/scenario.schema.json",
  "scripts/verify-consumer-lock.mjs",
  "spec/cloudlink-v1alpha1.md",
  "spec/distribution-v1alpha1.md",
  "spec/integration-control-v1alpha1.md",
  "spec/tck-v1alpha1.md",
  "tck/lib/scenario-runner.mjs",
  "tck/lib/strict-json.mjs",
  "tck/scenarios/core.json",
]);
const sourcePrefixes = [
  "fixtures/cloudlink/v1alpha1/",
  "fixtures/cloudlink-integration/v1alpha1/",
  "fixtures/integration/v1alpha1/",
  "fixtures/integration-control/v1alpha1/",
  "profiles/cloudlink/v1alpha1/",
  "profiles/integration/v1alpha1/",
  "profiles/integration-control/v1alpha1/",
  "schemas/cloudlink/v1alpha1/",
  "schemas/integration/v1alpha1/",
  "schemas/integration-control/v1alpha1/",
];
const selected = [...declared.keys()]
  .filter(
    (source) =>
      exactSources.has(source) || sourcePrefixes.some((prefix) => source.startsWith(prefix)),
  )
  .sort();

const unmatched = [...exactSources].filter((source) => !declared.has(source));
if (unmatched.length > 0) {
  throw new Error(
    `the published release does not declare ${unmatched.length} required artifact(s): ${unmatched.join(", ")}`,
  );
}

// CloudLink wire fixtures and schemas keep this repository's established
// `contracts/cloudlink/v1/` layout, which predates the versioned import tree.
// Everything else, including the whole aether.integration closure, mirrors the
// upstream path under the versioned directory.
function destination(source) {
  if (source === "fixtures/cloudlink/v1alpha1/fixture-manifest.json") {
    return "contracts/cloudlink/v1/fixture-manifest.json";
  }
  if (source.startsWith("fixtures/cloudlink/v1alpha1/")) {
    return `contracts/cloudlink/v1/fixtures/${basename(source)}`;
  }
  if (source.startsWith("schemas/cloudlink/v1alpha1/")) {
    return `contracts/cloudlink/v1/${basename(source)}`;
  }
  return `contracts/aether-contracts/v${version}/${source}`;
}

const imports = [];
for (const source of selected) {
  const bytes = await readFile(resolve(releaseRoot, source));
  const actual = digest(bytes);
  if (actual !== declared.get(source)) {
    throw new Error(`${source} differs from the release manifest`);
  }
  const target = destination(source);
  const absoluteTarget = resolve(consumerRoot, target);
  await mkdir(resolve(absoluteTarget, ".."), { recursive: true, mode: 0o700 });
  await writeFile(absoluteTarget, bytes, { mode: 0o600 });
  imports.push({ source, destination: target, sha256: actual });
}

const manifestLocalPath =
  `contracts/aether-contracts/v${version}/contract-manifest.json`;
await mkdir(resolve(consumerRoot, manifestLocalPath, ".."), {
  recursive: true,
  mode: 0o700,
});
await writeFile(resolve(consumerRoot, manifestLocalPath), manifestBytes, { mode: 0o600 });

const lock = {
  schema: "aether.contracts.consumer-lock.v1alpha1",
  status: "complete-consumer",
  repository: "https://github.com/EvanL1/AetherContracts",
  release: {
    version,
    tag: `v${version}`,
    tag_object: "166c26a3f204c9d000a4e07829bc2a67e6246657",
    commit: "8c858ba978aa183a3c534c34f62596f4902461ae",
    bundle: {
      name: `AetherContracts-${version}.tar.gz`,
      url: `https://github.com/EvanL1/AetherContracts/releases/download/v${version}/AetherContracts-${version}.tar.gz`,
      root: `AetherContracts-${version}`,
      size: 203186,
      sha256: "13d3cf0702976471e4206ac011de74733c03f97c2b263adb214df353279509b3",
      limits: {
        maximum_path_bytes: 512,
        maximum_file_bytes: 8388608,
        maximum_total_file_bytes: 67108864,
        maximum_entries: 4096,
      },
    },
  },
  manifest: {
    release_path: "contract-manifest.json",
    local_path: manifestLocalPath,
    sha256: digest(manifestBytes),
  },
  policy: {
    conformance_claim: "distribution-only",
    production_release: false,
    legacy_default: true,
    physical_control: false,
  },
  adoption: {
    // The upstream consumer-lock schema closes `modules` to
    // cloudlink/distribution/tck/thing-model. The aether.integration closure
    // is carried by the `cloudlink` module rather than a module of its own,
    // which matches how upstream names those contracts
    // (`aether.cloudlink.integration.v1alpha1`).
    scope: "cloudlink-integration-alpha4",
    modules: ["cloudlink", "distribution", "tck"],
    closure: "required-artifacts",
    required_artifacts: selected,
  },
  imports,
  pending_imports: [],
};

await writeFile(
  resolve(consumerRoot, "aether-contracts.lock.json"),
  `${JSON.stringify(lock, null, 2)}\n`,
  { mode: 0o600 },
);
process.stdout.write(`imported ${imports.length} exact AetherContracts ${version} artifacts\n`);
