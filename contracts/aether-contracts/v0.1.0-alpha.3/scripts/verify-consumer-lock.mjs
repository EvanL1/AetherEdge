#!/usr/bin/env node

import { createHash } from "node:crypto";
import { gunzipSync } from "node:zlib";
import {
  lstat,
  mkdtemp,
  mkdir,
  readFile,
  realpath,
  rm,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { pathToFileURL } from "node:url";

import { decodeJson } from "../tck/lib/strict-json.mjs";

const REPOSITORY = "https://github.com/EvanL1/AetherContracts";
const LOCK_SCHEMA = "aether.contracts.consumer-lock.v1alpha1";
const MAX_BUNDLE_BYTES = 52_428_800;
const MAX_ARCHIVE_PATH_BYTES = 512;
const MAX_ARCHIVE_FILE_BYTES = 8_388_608;
const MAX_ARCHIVE_TOTAL_FILE_BYTES = 67_108_864;
const MAX_ARCHIVE_ENTRIES = 4096;
const SHA256_PATTERN = /^[0-9a-f]{64}$/u;
const GIT_OBJECT_PATTERN = /^[0-9a-f]{40}$/u;
const SEMVER_PATTERN =
  /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-(?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*))*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/u;
const PATH_PATTERN = /^[A-Za-z0-9._/-]+$/u;

export const CONSUMER_LOCK_FAILURE_CODES = Object.freeze([
  "ACTION_COMMIT_MISMATCH",
  "ADOPTION_CLOSURE_MISMATCH",
  "ARGUMENT_INVALID",
  "ARCHIVE_LAYOUT_MISMATCH",
  "ARCHIVE_UNSAFE",
  "BUNDLE_DIGEST_MISMATCH",
  "BUNDLE_SIZE_MISMATCH",
  "BUNDLE_UNAVAILABLE",
  "CONSUMER_ARTIFACT_DIGEST_MISMATCH",
  "CONSUMER_ARTIFACT_INVALID",
  "CONSUMER_ARTIFACT_MISSING",
  "LOCK_MISSING",
  "LOCK_PATH_CONFLICT",
  "LOCK_SCHEMA_INVALID",
  "MANIFEST_ARTIFACT_MISMATCH",
  "MANIFEST_DIGEST_MISMATCH",
  "MANIFEST_IDENTITY_MISMATCH",
  "MANIFEST_INVALID",
  "MANIFEST_MISSING",
  "RELEASE_ARTIFACT_DIGEST_MISMATCH",
  "RELEASE_ARTIFACT_INVALID",
  "RELEASE_ARTIFACT_MISSING",
  "SAFETY_REQUIREMENT_MISMATCH",
]);

export class ConsumerLockFailure extends Error {
  constructor(code, message, path) {
    super(message);
    this.name = "ConsumerLockFailure";
    this.code = code;
    if (path !== undefined) {
      this.path = path;
    }
  }
}

function fail(code, message, path) {
  throw new ConsumerLockFailure(code, message, path);
}

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

function isRecord(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function assertRecord(value, path) {
  if (!isRecord(value)) {
    fail("LOCK_SCHEMA_INVALID", `${path} must be an object`, path);
  }
  return value;
}

function assertClosedObject(value, path, keys) {
  const record = assertRecord(value, path);
  const actual = Object.keys(record);
  for (const key of keys) {
    if (!Object.hasOwn(record, key)) {
      fail("LOCK_SCHEMA_INVALID", `${path}.${key} is required`, `${path}.${key}`);
    }
  }
  for (const key of actual) {
    if (!keys.includes(key)) {
      fail("LOCK_SCHEMA_INVALID", `${path}.${key} is not allowed`, `${path}.${key}`);
    }
  }
  return record;
}

function assertString(value, path, pattern) {
  if (typeof value !== "string" || (pattern !== undefined && !pattern.test(value))) {
    fail("LOCK_SCHEMA_INVALID", `${path} has an invalid value`, path);
  }
  return value;
}

function assertConstant(value, expected, path) {
  if (value !== expected) {
    fail("LOCK_SCHEMA_INVALID", `${path} must equal ${JSON.stringify(expected)}`, path);
  }
}

function assertBoundedInteger(value, path, maximum) {
  if (!Number.isSafeInteger(value) || value < 1 || value > maximum) {
    fail("LOCK_SCHEMA_INVALID", `${path} is outside its supported bound`, path);
  }
  return value;
}

function assertSafeRelativePath(value, path) {
  const candidate = assertString(value, path);
  if (
    candidate.length > 512 ||
    candidate.length === 0 ||
    isAbsolute(candidate) ||
    candidate.includes("\\") ||
    candidate.includes("//") ||
    !PATH_PATTERN.test(candidate) ||
    candidate.split("/").some((segment) => segment === "" || segment === "." || segment === "..")
  ) {
    fail("LOCK_SCHEMA_INVALID", `${path} must be a portable relative path`, path);
  }
  return candidate;
}

function validateImport(value, index) {
  const path = `imports[${String(index)}]`;
  const entry = assertClosedObject(value, path, ["source", "destination", "sha256"]);
  return {
    source: assertSafeRelativePath(entry.source, `${path}.source`),
    destination: assertSafeRelativePath(entry.destination, `${path}.destination`),
    sha256: assertString(entry.sha256, `${path}.sha256`, SHA256_PATTERN),
  };
}

function validatePendingImport(value, index) {
  const path = `pending_imports[${String(index)}]`;
  const entry = assertClosedObject(value, path, ["source", "sha256", "reason"]);
  const reason = assertString(entry.reason, `${path}.reason`);
  if (reason.length === 0 || reason.length > 512) {
    fail("LOCK_SCHEMA_INVALID", `${path}.reason must contain 1 to 512 characters`, `${path}.reason`);
  }
  return {
    source: assertSafeRelativePath(entry.source, `${path}.source`),
    sha256: assertString(entry.sha256, `${path}.sha256`, SHA256_PATTERN),
    reason,
  };
}

function validateLock(value) {
  const lock = assertClosedObject(value, "lock", [
    "schema",
    "status",
    "repository",
    "release",
    "manifest",
    "policy",
    "adoption",
    "imports",
    "pending_imports",
  ]);
  assertConstant(lock.schema, LOCK_SCHEMA, "lock.schema");
  assertConstant(lock.repository, REPOSITORY, "lock.repository");

  const status = assertString(lock.status, "lock.status");
  if (status !== "partial-consumer" && status !== "complete-consumer") {
    fail("LOCK_SCHEMA_INVALID", "lock.status is not supported", "lock.status");
  }

  const release = assertClosedObject(lock.release, "lock.release", [
    "version",
    "tag",
    "tag_object",
    "commit",
    "bundle",
  ]);
  const version = assertString(release.version, "lock.release.version", SEMVER_PATTERN);
  const tag = assertString(release.tag, "lock.release.tag");
  assertConstant(tag, `v${version}`, "lock.release.tag");
  const tagObject = assertString(
    release.tag_object,
    "lock.release.tag_object",
    GIT_OBJECT_PATTERN,
  );
  const commit = assertString(release.commit, "lock.release.commit", GIT_OBJECT_PATTERN);

  const bundle = assertClosedObject(release.bundle, "lock.release.bundle", [
    "name",
    "url",
    "root",
    "size",
    "sha256",
    "limits",
  ]);
  const bundleName = `AetherContracts-${version}.tar.gz`;
  const bundleRoot = `AetherContracts-${version}`;
  assertConstant(bundle.name, bundleName, "lock.release.bundle.name");
  assertConstant(bundle.root, bundleRoot, "lock.release.bundle.root");
  assertConstant(
    bundle.url,
    `${REPOSITORY}/releases/download/${tag}/${bundleName}`,
    "lock.release.bundle.url",
  );
  if (
    !Number.isSafeInteger(bundle.size) ||
    bundle.size < 1 ||
    bundle.size > MAX_BUNDLE_BYTES
  ) {
    fail("LOCK_SCHEMA_INVALID", "lock.release.bundle.size is outside its bound", "lock.release.bundle.size");
  }
  const bundleDigest = assertString(
    bundle.sha256,
    "lock.release.bundle.sha256",
    SHA256_PATTERN,
  );
  const limits = assertClosedObject(bundle.limits, "lock.release.bundle.limits", [
    "maximum_path_bytes",
    "maximum_file_bytes",
    "maximum_total_file_bytes",
    "maximum_entries",
  ]);
  const archiveLimits = {
    maximumPathBytes: assertBoundedInteger(
      limits.maximum_path_bytes,
      "lock.release.bundle.limits.maximum_path_bytes",
      MAX_ARCHIVE_PATH_BYTES,
    ),
    maximumFileBytes: assertBoundedInteger(
      limits.maximum_file_bytes,
      "lock.release.bundle.limits.maximum_file_bytes",
      MAX_ARCHIVE_FILE_BYTES,
    ),
    maximumTotalFileBytes: assertBoundedInteger(
      limits.maximum_total_file_bytes,
      "lock.release.bundle.limits.maximum_total_file_bytes",
      MAX_ARCHIVE_TOTAL_FILE_BYTES,
    ),
    maximumEntries: assertBoundedInteger(
      limits.maximum_entries,
      "lock.release.bundle.limits.maximum_entries",
      MAX_ARCHIVE_ENTRIES,
    ),
  };

  const manifest = assertClosedObject(lock.manifest, "lock.manifest", [
    "release_path",
    "local_path",
    "sha256",
  ]);
  assertConstant(manifest.release_path, "contract-manifest.json", "lock.manifest.release_path");
  const localManifestPath = assertSafeRelativePath(
    manifest.local_path,
    "lock.manifest.local_path",
  );
  const manifestDigest = assertString(
    manifest.sha256,
    "lock.manifest.sha256",
    SHA256_PATTERN,
  );

  const policy = assertClosedObject(lock.policy, "lock.policy", [
    "conformance_claim",
    "production_release",
    "legacy_default",
    "physical_control",
  ]);
  assertConstant(policy.conformance_claim, "distribution-only", "lock.policy.conformance_claim");
  assertConstant(policy.production_release, false, "lock.policy.production_release");
  assertConstant(policy.legacy_default, true, "lock.policy.legacy_default");
  assertConstant(policy.physical_control, false, "lock.policy.physical_control");

  const adoption = assertClosedObject(lock.adoption, "lock.adoption", [
    "scope",
    "modules",
    "closure",
    "required_artifacts",
  ]);
  const scope = assertString(
    adoption.scope,
    "lock.adoption.scope",
    /^[a-z0-9]+(?:-[a-z0-9]+)*$/u,
  );
  if (scope.length > 128) {
    fail("LOCK_SCHEMA_INVALID", "lock.adoption.scope exceeds 128 characters", "lock.adoption.scope");
  }
  assertConstant(adoption.closure, "required-artifacts", "lock.adoption.closure");
  if (!Array.isArray(adoption.modules) || adoption.modules.length === 0) {
    fail("LOCK_SCHEMA_INVALID", "lock.adoption.modules must be a non-empty array", "lock.adoption.modules");
  }
  const allowedModules = new Set(["cloudlink", "distribution", "tck", "thing-model"]);
  const modules = adoption.modules.map((module, index) => {
    const name = assertString(module, `lock.adoption.modules[${String(index)}]`);
    if (!allowedModules.has(name)) {
      fail("LOCK_SCHEMA_INVALID", `unsupported adoption module: ${name}`, `lock.adoption.modules[${String(index)}]`);
    }
    return name;
  });
  if (new Set(modules).size !== modules.length) {
    fail("ADOPTION_CLOSURE_MISMATCH", "lock.adoption.modules must be unique");
  }
  if (!Array.isArray(adoption.required_artifacts) || adoption.required_artifacts.length === 0) {
    fail(
      "LOCK_SCHEMA_INVALID",
      "lock.adoption.required_artifacts must be a non-empty array",
      "lock.adoption.required_artifacts",
    );
  }
  const requiredArtifacts = adoption.required_artifacts.map((source, index) =>
    assertSafeRelativePath(source, `lock.adoption.required_artifacts[${String(index)}]`),
  );
  if (new Set(requiredArtifacts).size !== requiredArtifacts.length) {
    fail("ADOPTION_CLOSURE_MISMATCH", "required adoption artifacts must be unique");
  }

  if (!Array.isArray(lock.imports) || lock.imports.length === 0) {
    fail("LOCK_SCHEMA_INVALID", "lock.imports must be a non-empty array", "lock.imports");
  }
  if (!Array.isArray(lock.pending_imports)) {
    fail("LOCK_SCHEMA_INVALID", "lock.pending_imports must be an array", "lock.pending_imports");
  }
  const imports = lock.imports.map(validateImport);
  const pendingImports = lock.pending_imports.map(validatePendingImport);
  const sources = new Set();
  const destinations = new Set();
  for (const entry of imports) {
    if (sources.has(entry.source) || destinations.has(entry.destination)) {
      fail("LOCK_PATH_CONFLICT", "import source and destination paths must be unique");
    }
    sources.add(entry.source);
    destinations.add(entry.destination);
  }
  for (const entry of pendingImports) {
    if (sources.has(entry.source)) {
      fail("LOCK_PATH_CONFLICT", "an artifact cannot be both imported and pending", entry.source);
    }
    sources.add(entry.source);
  }
  const requiredSet = new Set(requiredArtifacts);
  if (
    sources.size !== requiredSet.size ||
    [...sources].some((source) => !requiredSet.has(source))
  ) {
    fail(
      "ADOPTION_CLOSURE_MISMATCH",
      "imported and pending sources must exactly equal the required adoption closure",
    );
  }
  if (
    (status === "partial-consumer" && pendingImports.length === 0) ||
    (status === "complete-consumer" && pendingImports.length !== 0)
  ) {
    fail(
      "ADOPTION_CLOSURE_MISMATCH",
      "complete-consumer requires every adopted artifact to be imported",
      "lock.status",
    );
  }

  return {
    status,
    repository: REPOSITORY,
    release: {
      version,
      tag,
      tagObject,
      commit,
      bundle: {
        name: bundleName,
        url: bundle.url,
        root: bundleRoot,
        size: bundle.size,
        sha256: bundleDigest,
        limits: archiveLimits,
      },
    },
    manifest: {
      releasePath: "contract-manifest.json",
      localPath: localManifestPath,
      sha256: manifestDigest,
    },
    policy: {
      conformanceClaim: "distribution-only",
      productionRelease: false,
      legacyDefault: true,
      physicalControl: false,
    },
    adoption: {
      scope,
      modules,
      closure: "required-artifacts",
      requiredArtifacts,
    },
    imports,
    pendingImports,
  };
}

function validateManifest(value, lock) {
  const allowedKeys = new Set([
    "contract",
    "release_version",
    "source_authority",
    "production_release",
    "legacy_default",
    "physical_control",
    "formats",
    "modules",
    "bindings",
    "unresolved",
    "artifacts",
  ]);
  const manifest = assertRecord(value, "manifest");
  for (const key of Object.keys(manifest)) {
    if (!allowedKeys.has(key)) {
      fail("MANIFEST_INVALID", `manifest.${key} is not allowed`, `manifest.${key}`);
    }
  }
  for (const key of [
    "contract",
    "release_version",
    "production_release",
    "legacy_default",
    "physical_control",
    "artifacts",
  ]) {
    if (!Object.hasOwn(manifest, key)) {
      fail("MANIFEST_INVALID", `manifest.${key} is required`, `manifest.${key}`);
    }
  }
  if (manifest.contract !== "aether.contracts" || manifest.release_version !== lock.release.version) {
    fail("MANIFEST_IDENTITY_MISMATCH", "manifest identity does not match the locked release");
  }
  if (
    manifest.production_release !== lock.policy.productionRelease ||
    manifest.legacy_default !== lock.policy.legacyDefault ||
    manifest.physical_control !== lock.policy.physicalControl
  ) {
    fail("SAFETY_REQUIREMENT_MISMATCH", "manifest safety declarations do not match the lock");
  }
  if (!Array.isArray(manifest.artifacts)) {
    fail("MANIFEST_INVALID", "manifest.artifacts must be an array", "manifest.artifacts");
  }

  const artifacts = new Map();
  manifest.artifacts.forEach((value, index) => {
    const path = `manifest.artifacts[${String(index)}]`;
    const artifact = assertRecord(value, path);
    if (
      Object.keys(artifact).length !== 2 ||
      !Object.hasOwn(artifact, "path") ||
      !Object.hasOwn(artifact, "sha256")
    ) {
      fail("MANIFEST_INVALID", `${path} must contain only path and sha256`, path);
    }
    let source;
    try {
      source = assertSafeRelativePath(artifact.path, `${path}.path`);
    } catch (error) {
      if (error instanceof ConsumerLockFailure) {
        fail("MANIFEST_INVALID", error.message, error.path);
      }
      throw error;
    }
    if (artifacts.has(source)) {
      fail("MANIFEST_INVALID", `manifest repeats artifact ${source}`, source);
    }
    if (typeof artifact.sha256 !== "string" || !SHA256_PATTERN.test(artifact.sha256)) {
      fail("MANIFEST_INVALID", `${path}.sha256 is invalid`, `${path}.sha256`);
    }
    artifacts.set(source, artifact.sha256);
  });
  return artifacts;
}

function isWithin(root, candidate) {
  const path = relative(root, candidate);
  return path === "" || (!path.startsWith(`..${sep}`) && path !== ".." && !isAbsolute(path));
}

async function readRegularFile(root, relativePath, missingCode, invalidCode) {
  const canonicalRoot = await realpath(root).catch(() => {
    fail(missingCode, `root does not exist: ${root}`);
  });
  const absolutePath = resolve(canonicalRoot, relativePath);
  if (!isWithin(canonicalRoot, absolutePath)) {
    fail(invalidCode, `path escapes its root: ${relativePath}`, relativePath);
  }
  const stat = await lstat(absolutePath).catch(() => {
    fail(missingCode, `required file is missing: ${relativePath}`, relativePath);
  });
  if (!stat.isFile()) {
    fail(invalidCode, `required path is not a regular file: ${relativePath}`, relativePath);
  }
  const canonicalFile = await realpath(absolutePath);
  if (!isWithin(canonicalRoot, canonicalFile)) {
    fail(invalidCode, `path resolves outside its root: ${relativePath}`, relativePath);
  }
  return readFile(canonicalFile);
}

function decodeDocument(bytes, code, description) {
  try {
    return decodeJson(bytes);
  } catch (error) {
    fail(code, `${description} is not strict JSON: ${error instanceof Error ? error.message : String(error)}`);
  }
}

export function resolveConsumerLockPath(consumerRoot, lockRelativePath) {
  const root = resolve(consumerRoot);
  const portablePath = assertSafeRelativePath(lockRelativePath, "lock-path");
  const absolutePath = resolve(root, portablePath);
  if (!isWithin(root, absolutePath)) {
    fail("LOCK_SCHEMA_INVALID", "lock-path escapes the consumer root", "lock-path");
  }
  return absolutePath;
}

function lockRelativePathFromApi(consumerRoot, lockPath) {
  if (lockPath === undefined) {
    return "aether-contracts.lock.json";
  }
  if (!isAbsolute(lockPath)) {
    return lockPath;
  }
  const candidate = relative(resolve(consumerRoot), resolve(lockPath));
  return assertSafeRelativePath(candidate, "lock-path");
}

export function verifyActionCommit(actionCommit, releaseCommit) {
  if (
    typeof actionCommit !== "string" ||
    !GIT_OBJECT_PATTERN.test(actionCommit) ||
    actionCommit !== releaseCommit
  ) {
    fail(
      "ACTION_COMMIT_MISMATCH",
      "the composite Action commit must exactly match lock.release.commit",
      "lock.release.commit",
    );
  }
}

async function loadConsumerLock(consumerRoot, lockPath) {
  const lockRelativePath = lockRelativePathFromApi(consumerRoot, lockPath);
  const bytes = await readRegularFile(
    consumerRoot,
    lockRelativePath,
    "LOCK_MISSING",
    "LOCK_SCHEMA_INVALID",
  );
  return validateLock(decodeDocument(bytes, "LOCK_SCHEMA_INVALID", "consumer lock"));
}

function assertDigest(bytes, expected, code, path) {
  const actual = sha256(bytes);
  if (actual !== expected) {
    fail(code, `SHA-256 mismatch for ${path}: expected ${expected}, received ${actual}`, path);
  }
}

export function verifyBundleBytes(bytes, expected) {
  const view = Buffer.isBuffer(bytes)
    ? bytes
    : ArrayBuffer.isView(bytes)
      ? Buffer.from(bytes.buffer, bytes.byteOffset, bytes.byteLength)
      : bytes instanceof ArrayBuffer
        ? Buffer.from(bytes)
        : undefined;
  if (view === undefined) {
    throw new TypeError("bundle bytes must be an ArrayBuffer or byte view");
  }
  if (view.byteLength !== expected.size) {
    fail(
      "BUNDLE_SIZE_MISMATCH",
      `bundle size mismatch: expected ${String(expected.size)}, received ${String(view.byteLength)}`,
    );
  }
  assertDigest(view, expected.sha256, "BUNDLE_DIGEST_MISMATCH", "release bundle");
}

export async function verifyConsumerLock({ actionCommit, consumerRoot, lockPath, releaseRoot }) {
  const lock = await loadConsumerLock(consumerRoot, lockPath);
  if (actionCommit !== undefined) {
    verifyActionCommit(actionCommit, lock.release.commit);
  }
  const manifestBytes = await readRegularFile(
    consumerRoot,
    lock.manifest.localPath,
    "MANIFEST_MISSING",
    "MANIFEST_INVALID",
  );
  assertDigest(
    manifestBytes,
    lock.manifest.sha256,
    "MANIFEST_DIGEST_MISMATCH",
    lock.manifest.localPath,
  );
  const manifestArtifacts = validateManifest(
    decodeDocument(manifestBytes, "MANIFEST_INVALID", "consumer manifest"),
    lock,
  );

  for (const entry of [...lock.imports, ...lock.pendingImports]) {
    const declared = manifestArtifacts.get(entry.source);
    if (declared === undefined || declared !== entry.sha256) {
      fail(
        "MANIFEST_ARTIFACT_MISMATCH",
        `locked artifact does not match the release manifest: ${entry.source}`,
        entry.source,
      );
    }
  }

  for (const entry of lock.imports) {
    const bytes = await readRegularFile(
      consumerRoot,
      entry.destination,
      "CONSUMER_ARTIFACT_MISSING",
      "CONSUMER_ARTIFACT_INVALID",
    );
    assertDigest(
      bytes,
      entry.sha256,
      "CONSUMER_ARTIFACT_DIGEST_MISMATCH",
      entry.destination,
    );
  }

  if (releaseRoot !== undefined) {
    const releaseManifest = await readRegularFile(
      releaseRoot,
      lock.manifest.releasePath,
      "MANIFEST_MISSING",
      "MANIFEST_INVALID",
    );
    assertDigest(
      releaseManifest,
      lock.manifest.sha256,
      "MANIFEST_DIGEST_MISMATCH",
      lock.manifest.releasePath,
    );
    validateManifest(
      decodeDocument(releaseManifest, "MANIFEST_INVALID", "release manifest"),
      lock,
    );
    for (const entry of [...lock.imports, ...lock.pendingImports]) {
      const bytes = await readRegularFile(
        releaseRoot,
        entry.source,
        "RELEASE_ARTIFACT_MISSING",
        "RELEASE_ARTIFACT_INVALID",
      );
      assertDigest(bytes, entry.sha256, "RELEASE_ARTIFACT_DIGEST_MISMATCH", entry.source);
    }
  }

  return {
    imported: lock.imports.length,
    pending: lock.pendingImports.length,
    releaseCommit: lock.release.commit,
    releaseVersion: lock.release.version,
    scope: lock.adoption.scope,
    status: lock.status,
  };
}

async function readBoundedResponse(response, expectedSize) {
  const contentLength = response.headers.get("content-length");
  if (contentLength !== null && Number(contentLength) !== expectedSize) {
    fail("BUNDLE_SIZE_MISMATCH", "release response Content-Length does not match the lock");
  }
  if (response.body === null) {
    fail("BUNDLE_UNAVAILABLE", "release response has no body");
  }
  const chunks = [];
  let total = 0;
  for await (const chunk of response.body) {
    const bytes = Buffer.from(chunk);
    total += bytes.byteLength;
    if (total > expectedSize || total > MAX_BUNDLE_BYTES) {
      fail("BUNDLE_SIZE_MISMATCH", "release response exceeds the locked size");
    }
    chunks.push(bytes);
  }
  return Buffer.concat(chunks, total);
}

function assertArchivePath(path, root, maximumPathBytes) {
  const normalized = path.endsWith("/") ? path.slice(0, -1) : path;
  if (Buffer.byteLength(path, "utf8") > maximumPathBytes) {
    fail("ARCHIVE_LAYOUT_MISMATCH", `release archive path exceeds its byte limit: ${path}`, path);
  }
  if (
    normalized.length === 0 ||
    isAbsolute(normalized) ||
    normalized.includes("\\") ||
    normalized.includes("//") ||
    normalized.split("/").some((segment) => segment === "" || segment === "." || segment === "..") ||
    (normalized !== root && !normalized.startsWith(`${root}/`))
  ) {
    fail("ARCHIVE_UNSAFE", `release archive contains an unsafe path: ${path}`, path);
  }
  return normalized;
}

function tarString(header, offset, length, field) {
  const bytes = header.subarray(offset, offset + length);
  const nul = bytes.indexOf(0);
  const slice = nul === -1 ? bytes : bytes.subarray(0, nul);
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(slice);
  } catch {
    fail("ARCHIVE_UNSAFE", `release archive ${field} is not valid UTF-8`);
  }
}

function tarOctal(header, offset, length, field) {
  const value = tarString(header, offset, length, field).trim();
  if (!/^[0-7]+$/u.test(value)) {
    fail("ARCHIVE_UNSAFE", `release archive ${field} is not canonical octal`);
  }
  const parsed = Number.parseInt(value, 8);
  if (!Number.isSafeInteger(parsed) || parsed < 0) {
    fail("ARCHIVE_UNSAFE", `release archive ${field} is outside the safe range`);
  }
  return parsed;
}

function tarHeaderChecksum(header) {
  let checksum = 0;
  for (let index = 0; index < header.length; index += 1) {
    checksum += index >= 148 && index < 156 ? 0x20 : header[index];
  }
  return checksum;
}

function normalizedArchiveLimits(bundle) {
  const limits = bundle.limits;
  return {
    maximumPathBytes:
      limits.maximumPathBytes ?? limits.maximum_path_bytes,
    maximumFileBytes:
      limits.maximumFileBytes ?? limits.maximum_file_bytes,
    maximumTotalFileBytes:
      limits.maximumTotalFileBytes ?? limits.maximum_total_file_bytes,
    maximumEntries:
      limits.maximumEntries ?? limits.maximum_entries,
  };
}

function decodeTarArchive(tarBytes, root, limits) {
  const entries = [];
  const paths = new Set();
  let entryCount = 0;
  let offset = 0;
  let totalFileBytes = 0;
  let sawGlobalHeader = false;
  let sawTerminator = false;

  while (offset + 512 <= tarBytes.byteLength) {
    const header = tarBytes.subarray(offset, offset + 512);
    offset += 512;
    if (header.every((byte) => byte === 0)) {
      if (offset + 512 > tarBytes.byteLength) {
        fail("ARCHIVE_LAYOUT_MISMATCH", "release archive has an incomplete terminator");
      }
      const second = tarBytes.subarray(offset, offset + 512);
      if (!second.every((byte) => byte === 0)) {
        fail("ARCHIVE_LAYOUT_MISMATCH", "release archive has only one zero terminator block");
      }
      if (!tarBytes.subarray(offset + 512).every((byte) => byte === 0)) {
        fail("ARCHIVE_LAYOUT_MISMATCH", "release archive has data after its terminator");
      }
      sawTerminator = true;
      break;
    }

    const expectedChecksum = tarOctal(header, 148, 8, "header checksum");
    if (tarHeaderChecksum(header) !== expectedChecksum) {
      fail("ARCHIVE_UNSAFE", "release archive header checksum is invalid");
    }
    const name = tarString(header, 0, 100, "entry name");
    const prefix = tarString(header, 345, 155, "entry prefix");
    const path = prefix.length === 0 ? name : `${prefix}/${name}`;
    const size = tarOctal(header, 124, 12, "entry size");
    const typeByte = header[156];
    const type = typeByte === 0 ? "0" : String.fromCharCode(typeByte);
    const paddedSize = Math.ceil(size / 512) * 512;
    if (offset + paddedSize > tarBytes.byteLength) {
      fail("ARCHIVE_LAYOUT_MISMATCH", `release archive entry is truncated: ${path}`, path);
    }
    const body = tarBytes.subarray(offset, offset + size);
    offset += paddedSize;

    entryCount += 1;
    if (entryCount > limits.maximumEntries) {
      fail("ARCHIVE_LAYOUT_MISMATCH", "release archive exceeds its entry-count limit");
    }

    if (type === "g") {
      if (sawGlobalHeader || path !== "pax_global_header" || size > 4096) {
        fail("ARCHIVE_UNSAFE", "release archive contains an unsupported global PAX header");
      }
      const attributes = new TextDecoder("utf-8", { fatal: true }).decode(body);
      if (!/^\d+ comment=[^\n]+\n$/u.test(attributes)) {
        fail("ARCHIVE_UNSAFE", "release archive global PAX header is not the Git commit comment");
      }
      sawGlobalHeader = true;
      continue;
    }

    if (type !== "0" && type !== "5") {
      fail("ARCHIVE_UNSAFE", `release archive contains unsupported entry type ${type}`, path);
    }
    const normalized = assertArchivePath(path, root, limits.maximumPathBytes);
    if (paths.has(normalized)) {
      fail("ARCHIVE_UNSAFE", `release archive repeats path: ${normalized}`, normalized);
    }
    paths.add(normalized);

    if (type === "5") {
      if (size !== 0) {
        fail("ARCHIVE_UNSAFE", `release archive directory has a body: ${path}`, path);
      }
      entries.push({ path: normalized, type: "directory" });
      continue;
    }
    if (size > limits.maximumFileBytes) {
      fail("ARCHIVE_LAYOUT_MISMATCH", `release archive file exceeds its size limit: ${path}`, path);
    }
    totalFileBytes += size;
    if (totalFileBytes > limits.maximumTotalFileBytes) {
      fail("ARCHIVE_LAYOUT_MISMATCH", "release archive exceeds its total file-size limit");
    }
    entries.push({ body: Buffer.from(body), path: normalized, type: "file" });
  }

  if (!sawTerminator || entries.length === 0 || !paths.has(root)) {
    fail("ARCHIVE_LAYOUT_MISMATCH", "release archive does not contain one terminated locked root");
  }
  return entries;
}

export async function extractVerifiedBundle(bundleBytes, lock, temporaryRoot) {
  const bundle = lock.release.bundle;
  const limits = normalizedArchiveLimits(bundle);
  const maximumExpandedBytes =
    limits.maximumTotalFileBytes + (limits.maximumEntries * 1024) + 1024;
  let tarBytes;
  try {
    tarBytes = gunzipSync(bundleBytes, { maxOutputLength: maximumExpandedBytes });
  } catch (error) {
    fail(
      "ARCHIVE_LAYOUT_MISMATCH",
      `release archive cannot be decompressed within its bounds: ${error instanceof Error ? error.message : String(error)}`,
    );
  }
  const entries = decodeTarArchive(tarBytes, bundle.root, limits);
  const extractionRoot = join(temporaryRoot, "extracted");
  await mkdir(extractionRoot, { mode: 0o700 });
  try {
    for (const entry of entries.filter((candidate) => candidate.type === "directory")) {
      await mkdir(join(extractionRoot, entry.path), { mode: 0o700, recursive: true });
    }
    for (const entry of entries.filter((candidate) => candidate.type === "file")) {
      const destination = join(extractionRoot, entry.path);
      await mkdir(dirname(destination), { mode: 0o700, recursive: true });
      await writeFile(destination, entry.body, { flag: "wx", mode: 0o600 });
    }
  } catch (error) {
    if (error instanceof ConsumerLockFailure) {
      throw error;
    }
    fail("ARCHIVE_UNSAFE", `release archive cannot be extracted safely: ${error instanceof Error ? error.message : String(error)}`);
  }
  return join(extractionRoot, bundle.root);
}

async function verifyOnline({ actionCommit, consumerRoot, lockPath }) {
  const lock = await loadConsumerLock(consumerRoot, lockPath);
  if (actionCommit !== undefined) {
    verifyActionCommit(actionCommit, lock.release.commit);
  }
  let response;
  try {
    response = await fetch(lock.release.bundle.url, {
      headers: { accept: "application/gzip" },
      redirect: "follow",
      signal: AbortSignal.timeout(30_000),
    });
  } catch (error) {
    fail("BUNDLE_UNAVAILABLE", `release bundle download failed: ${error instanceof Error ? error.message : String(error)}`);
  }
  if (!response.ok) {
    fail("BUNDLE_UNAVAILABLE", `release bundle returned HTTP ${String(response.status)}`);
  }
  const bundleBytes = await readBoundedResponse(response, lock.release.bundle.size);
  verifyBundleBytes(bundleBytes, lock.release.bundle);

  const temporaryRoot = await mkdtemp(join(tmpdir(), "aether-contract-release-"));
  try {
    const releaseRoot = await extractVerifiedBundle(bundleBytes, lock, temporaryRoot);
    return await verifyConsumerLock({ actionCommit, consumerRoot, lockPath, releaseRoot });
  } finally {
    await rm(temporaryRoot, { force: true, recursive: true });
  }
}

function parseArguments(argv) {
  const options = {
    consumerRoot: process.cwd(),
    lockRelativePath: "aether-contracts.lock.json",
    releaseRoot: undefined,
    actionCommit: undefined,
    online: false,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--online") {
      options.online = true;
    } else if (
      argument === "--consumer-root" ||
      argument === "--lock-path" ||
      argument === "--release-root" ||
      argument === "--action-commit"
    ) {
      const value = argv[index + 1];
      if (value === undefined) {
        fail("ARGUMENT_INVALID", `${argument} requires a value`);
      }
      index += 1;
      if (argument === "--consumer-root") {
        options.consumerRoot = resolve(value);
      } else if (argument === "--lock-path") {
        options.lockRelativePath = value;
      } else if (argument === "--release-root") {
        options.releaseRoot = resolve(value);
      } else {
        options.actionCommit = value;
      }
    } else {
      fail("ARGUMENT_INVALID", `unknown argument: ${argument}`);
    }
  }
  options.consumerRoot = resolve(options.consumerRoot);
  options.lockPath = resolveConsumerLockPath(
    options.consumerRoot,
    options.lockRelativePath,
  );
  delete options.lockRelativePath;
  if (options.online && options.releaseRoot !== undefined) {
    fail("ARGUMENT_INVALID", "--online and --release-root are mutually exclusive");
  }
  return options;
}

async function main() {
  const options = parseArguments(process.argv.slice(2));
  const result = options.online
    ? await verifyOnline(options)
    : await verifyConsumerLock(options);
  process.stdout.write(`${JSON.stringify({ ok: true, ...result })}\n`);
}

const invokedPath = process.argv[1] === undefined ? undefined : resolve(process.argv[1]);
if (invokedPath !== undefined && import.meta.url === pathToFileURL(invokedPath).href) {
  main().catch((error) => {
    const code = error instanceof ConsumerLockFailure ? error.code : "VERIFIER_INTERNAL_ERROR";
    const message = error instanceof Error ? error.message : String(error);
    const path = error instanceof ConsumerLockFailure ? error.path : undefined;
    process.stderr.write(`${JSON.stringify({ ok: false, code, message, ...(path === undefined ? {} : { path }) })}\n`);
    process.exitCode = 1;
  });
}
