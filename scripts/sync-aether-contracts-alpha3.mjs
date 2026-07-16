import { createHash } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { basename, resolve } from "node:path";

const version = "0.1.0-alpha.3";
const releaseRoot = resolve(process.argv[2] ?? "");
const consumerRoot = resolve(import.meta.dirname, "..");
const manifestBytes = await readFile(resolve(releaseRoot, "contract-manifest.json"));
const manifest = JSON.parse(manifestBytes.toString("utf8"));

if (manifest.release_version !== version) {
  throw new Error(`expected AetherContracts ${version}`);
}

const digest = (bytes) => createHash("sha256").update(bytes).digest("hex");
const declared = new Map(manifest.artifacts.map((entry) => [entry.path, entry.sha256]));
const selected = [...declared.keys()].filter(
  (source) =>
    source === ".github/actions/verify-consumer/action.yml" ||
    source === "compatibility/cloudlink-v1alpha1-gates.json" ||
    source === "compatibility/failure-codes.json" ||
    source === "profiles/cloudlink/v1alpha1/authentication.json" ||
    source === "profiles/cloudlink/v1alpha1/core.json" ||
    source === "profiles/mqtt/v1alpha1/profile.json" ||
    source === "schemas/distribution/v1alpha1/consumer-lock.schema.json" ||
    source === "schemas/tck/v1alpha1/fixture-manifest.schema.json" ||
    source === "schemas/tck/v1alpha1/scenario.schema.json" ||
    source === "scripts/verify-consumer-lock.mjs" ||
    source === "spec/cloudlink-v1alpha1.md" ||
    source === "spec/distribution-v1alpha1.md" ||
    source === "spec/tck-v1alpha1.md" ||
    source === "tck/lib/scenario-runner.mjs" ||
    source === "tck/lib/strict-json.mjs" ||
    source === "tck/scenarios/core.json" ||
    source.startsWith("fixtures/cloudlink/v1alpha1/") ||
    source.startsWith("schemas/cloudlink/v1alpha1/"),
).sort();

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
    tag_object: "2a7539284e65d43fb3b81abf74009c63e1a28d33",
    commit: "c5aad674f0844138e778963118e786e430ffb365",
    bundle: {
      name: `AetherContracts-${version}.tar.gz`,
      url: `https://github.com/EvanL1/AetherContracts/releases/download/v${version}/AetherContracts-${version}.tar.gz`,
      root: `AetherContracts-${version}`,
      size: 115663,
      sha256: "0946391d015f00579751c007a6b7925d003455a731d2f6ae295d706ddcb5dfb2",
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
    scope: "cloudlink-alpha3",
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
