import { createHash } from "node:crypto";
import { readdir, readFile } from "node:fs/promises";
import { isDeepStrictEqual } from "node:util";

import Ajv2020 from "ajv/dist/2020.js";

import { decodeJson } from "./strict-json.mjs";

const repositoryRoot = new URL("../../", import.meta.url);
const maximumUint64 = 18_446_744_073_709_551_615n;
const canonicalUnsignedPattern = /^(0|[1-9][0-9]*)$/;

export const CLOUDLINK_REPLAY_IDENTITY_FIELDS = Object.freeze([
  "gateway_id",
  "delivery.stream_id",
  "delivery.stream_epoch",
  "delivery.position",
]);

const cloudLinkCurrentSessionFields = Object.freeze([
  "gateway_id",
  "session_id",
  "session_epoch",
  "credential_generation",
]);

function repositoryUrl(relativePath) {
  if (typeof relativePath !== "string" || relativePath.length === 0) {
    throw new TypeError("repository JSON path must be a non-empty string");
  }
  const url = new URL(relativePath, repositoryRoot);
  if (!url.href.startsWith(repositoryRoot.href)) {
    throw new Error(`repository JSON path escapes the repository: ${relativePath}`);
  }
  return url;
}

async function readRepositoryJson(relativePath) {
  return decodeJson(await readFile(repositoryUrl(relativePath)));
}

let scenarioValidatorPromise;

async function scenarioValidator() {
  scenarioValidatorPromise ??= (async () => {
    const schema = await readRepositoryJson("schemas/tck/v1alpha1/scenario.schema.json");
    return new Ajv2020({ allErrors: true, strict: false }).compile(schema);
  })();
  return scenarioValidatorPromise;
}

export async function validateScenarioSet(scenarioSet) {
  const validate = await scenarioValidator();
  if (!validate(scenarioSet)) {
    throw new Error(
      `TCK scenario schema validation failed: ${JSON.stringify(validate.errors)}`,
    );
  }
  const ids = new Set();
  for (const scenario of scenarioSet.scenarios) {
    if (ids.has(scenario.id)) {
      throw new Error(`TCK scenario set contains duplicate scenario id: ${scenario.id}`);
    }
    ids.add(scenario.id);
  }
  return scenarioSet;
}

export async function loadCoreScenarioSet() {
  const scenarioSet = await readRepositoryJson("tck/scenarios/core.json");
  return validateScenarioSet(scenarioSet);
}

function validateUint64(value) {
  if (typeof value !== "string") {
    return { accepted: false, failure_code: "INTEGER_NON_CANONICAL" };
  }
  if (new TextEncoder().encode(value).byteLength > 20) {
    return { accepted: false, failure_code: "INTEGER_OUT_OF_RANGE" };
  }
  if (!canonicalUnsignedPattern.test(value)) {
    return { accepted: false, failure_code: "INTEGER_NON_CANONICAL" };
  }
  if (BigInt(value) > maximumUint64) {
    return { accepted: false, failure_code: "INTEGER_OUT_OF_RANGE" };
  }
  return { accepted: true };
}

function rawScenarioBytes(input) {
  if (input.encoding === "utf8" && typeof input.raw === "string") {
    return new TextEncoder().encode(input.raw);
  }
  if (
    input.encoding === "hex" &&
    typeof input.raw === "string" &&
    input.raw.length % 2 === 0 &&
    /^[0-9a-f]*$/.test(input.raw)
  ) {
    const bytes = new Uint8Array(input.raw.length / 2);
    for (let index = 0; index < input.raw.length; index += 2) {
      bytes[index / 2] = Number.parseInt(input.raw.slice(index, index + 2), 16);
    }
    return bytes;
  }
  throw new Error("raw JSON scenario must declare utf8 text or lowercase hexadecimal bytes");
}

function executeRawJsonScenario(scenario) {
  try {
    decodeJson(rawScenarioBytes(scenario.input));
    return { accepted: true };
  } catch (error) {
    if (
      error instanceof SyntaxError &&
      typeof error.code === "string"
    ) {
      return { accepted: false, failure_code: error.code };
    }
    throw error;
  }
}

let cloudLinkValidatorsPromise;

async function cloudLinkValidators() {
  cloudLinkValidatorsPromise ??= (async () => {
    const directory = "schemas/cloudlink/v1alpha1/";
    const names = (await readdir(repositoryUrl(directory)))
      .filter((name) => name.endsWith(".schema.json"))
      .sort();
    const schemas = await Promise.all(
      names.map((name) => readRepositoryJson(`${directory}${name}`)),
    );
    const ajv = new Ajv2020({ allErrors: true, strict: false });
    for (const schema of schemas) {
      ajv.addSchema(schema);
    }
    return ajv;
  })();
  return cloudLinkValidatorsPromise;
}

let cloudLinkFixtureManifestPromise;

async function cloudLinkFixtureManifest() {
  cloudLinkFixtureManifestPromise ??= readRepositoryJson(
    "fixtures/cloudlink/v1alpha1/fixture-manifest.json",
  );
  return cloudLinkFixtureManifestPromise;
}

function cloudLinkFixtureName(relativePath) {
  const prefix = "fixtures/cloudlink/v1alpha1/";
  if (!relativePath.startsWith(prefix) || relativePath.includes("..")) {
    throw new Error(`CloudLink fixture path is outside the fixture set: ${relativePath}`);
  }
  return relativePath.slice(prefix.length);
}

export async function readCloudLinkFixture(relativePath) {
  cloudLinkFixtureName(relativePath);
  return readRepositoryJson(relativePath);
}

async function validateCloudLinkFixture(relativePath, semanticMutation) {
  const fixtureName = cloudLinkFixtureName(relativePath);
  const manifest = await cloudLinkFixtureManifest();
  const entry = manifest.fixtures.find((candidate) => candidate.file === fixtureName);
  if (entry === undefined || typeof entry.schema_id !== "string") {
    throw new Error(`CloudLink fixture has no declared entry schema: ${relativePath}`);
  }

  let fixture = await readCloudLinkFixture(relativePath);
  if (semanticMutation !== undefined) {
    fixture = structuredClone(fixture);
    if (semanticMutation.top_level_overrides !== undefined) {
      fixture = {
        ...fixture,
        ...semanticMutation.top_level_overrides,
      };
    }
    if (semanticMutation.payload_overrides !== undefined) {
      fixture.payload = {
        ...fixture.payload,
        ...semanticMutation.payload_overrides,
      };
    }
    if (semanticMutation.delivery_overrides !== undefined) {
      if (fixture.delivery === null || typeof fixture.delivery !== "object") {
        throw new Error("delivery semantic mutation requires a delivery object");
      }
      fixture.delivery = {
        ...fixture.delivery,
        ...semanticMutation.delivery_overrides,
      };
    }
    if (semanticMutation.duplicate_cursor_index !== undefined) {
      const cursor = fixture.cursors[semanticMutation.duplicate_cursor_index];
      if (cursor === undefined) {
        throw new Error("cursor semantic mutation references a missing cursor");
      }
      fixture.cursors.push(structuredClone(cursor));
    }
    if (semanticMutation.recompute_business_digest === true) {
      fixture.delivery.digest = businessDigestForEnvelope(fixture);
    }
  }
  const ajv = await cloudLinkValidators();
  const validate = ajv.getSchema(entry.schema_id);
  if (validate === undefined) {
    throw new Error(`CloudLink fixture schema is unavailable: ${entry.schema_id}`);
  }
  return {
    fixture,
    accepted: validate(fixture),
    errors: validate.errors,
    schema_id: entry.schema_id,
  };
}

let thingModelValidatorPromise;
let thingModelFixtureManifestPromise;

async function thingModelValidator() {
  thingModelValidatorPromise ??= (async () => {
    const schema = await readRepositoryJson(
      "schemas/thing-model/v1alpha1/thing-model.schema.json",
    );
    return new Ajv2020({ allErrors: true, strict: false }).compile(schema);
  })();
  return thingModelValidatorPromise;
}

async function validateThingModelFixture(relativePath) {
  const prefix = "fixtures/thing-model/v1alpha1/";
  if (!relativePath.startsWith(prefix) || relativePath.includes("..")) {
    throw new Error(`Thing Model fixture path is outside the fixture set: ${relativePath}`);
  }
  thingModelFixtureManifestPromise ??= readRepositoryJson(
    "fixtures/thing-model/v1alpha1/fixture-manifest.json",
  );
  const manifest = await thingModelFixtureManifestPromise;
  const fixtureName = relativePath.slice(prefix.length);
  const entry = manifest.fixtures.find((candidate) => candidate.file === fixtureName);
  if (entry?.schema_id !== "https://contracts.aether.dev/schemas/thing-model/v1alpha1/thing-model.schema.json") {
    throw new Error(`Thing Model fixture has no declared entry schema: ${relativePath}`);
  }
  const fixture = await readRepositoryJson(relativePath);
  const validate = await thingModelValidator();
  return { fixture, accepted: validate(fixture), errors: validate.errors };
}

export function hasThingModelKeyConflict(thingModel) {
  const keys = new Set();
  for (const namespace of ["properties", "points", "capabilities"]) {
    const definitions = thingModel[namespace];
    if (!Array.isArray(definitions)) {
      continue;
    }
    for (const definition of definitions) {
      if (keys.has(definition.key)) {
        return true;
      }
      keys.add(definition.key);

      if (namespace === "capabilities" && Array.isArray(definition.parameters)) {
        const parameterKeys = new Set();
        for (const parameter of definition.parameters) {
          if (parameterKeys.has(parameter.key)) {
            return true;
          }
          parameterKeys.add(parameter.key);
        }
      }
    }
  }
  return false;
}

async function executeThingModelScenario(scenario) {
  const evidence = await validateThingModelFixture(scenario.input.fixture);
  if (!evidence.accepted) {
    return { wire_accepted: false };
  }
  if (hasThingModelKeyConflict(evidence.fixture)) {
    return {
      wire_accepted: true,
      context_accepted: false,
      failure_code: "KEY_CONFLICT",
      state_changed: false,
      successful_receipt_permitted: false,
    };
  }
  return { wire_accepted: true, context_accepted: true, state_changed: false };
}

function replayIdentity(message) {
  const delivery = message.delivery;
  if (delivery === null || typeof delivery !== "object") {
    throw new Error("CloudLink replay candidate has no delivery identity");
  }
  return {
    gateway_id: message.gateway_id,
    "delivery.stream_id": delivery.stream_id,
    "delivery.stream_epoch": delivery.stream_epoch,
    "delivery.position": delivery.position,
  };
}

function hasSameReplayIdentity(candidate, prior) {
  const candidateIdentity = replayIdentity(candidate);
  const priorIdentity = replayIdentity(prior);
  return CLOUDLINK_REPLAY_IDENTITY_FIELDS.every(
    (field) => candidateIdentity[field] === priorIdentity[field],
  );
}

function canonicalJson(value) {
  if (typeof value === "number" && !Number.isFinite(value)) {
    throw new TypeError("RFC 8785 canonical JSON requires finite numbers");
  }
  if (
    value === null ||
    typeof value === "boolean" ||
    typeof value === "number" ||
    typeof value === "string"
  ) {
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) {
    return `[${value.map((entry) => canonicalJson(entry)).join(",")}]`;
  }
  if (typeof value === "object") {
    return `{${Object.keys(value)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`)
      .join(",")}}`;
  }
  throw new TypeError("business digest projection contains a non-JSON value");
}

export function businessDigestForEnvelope(envelope) {
  const projection = {
    protocol_version: envelope.protocol_version,
    message_kind: envelope.message_kind,
    payload: envelope.payload,
  };
  return `sha256:${createHash("sha256").update(canonicalJson(projection), "utf8").digest("hex")}`;
}

export function thingModelPublicationDigest(thingModel) {
  return `sha256:${createHash("sha256").update(canonicalJson(thingModel), "utf8").digest("hex")}`;
}

export function runtimeManifestChecksum(manifest) {
  if (manifest === null || typeof manifest !== "object" || Array.isArray(manifest)) {
    throw new TypeError("Runtime Manifest checksum input must be an object");
  }
  const projection = Object.fromEntries(
    Object.entries(manifest).filter(([key]) => key !== "checksum"),
  );
  return createHash("sha256").update(canonicalJson(projection), "utf8").digest("hex");
}

function staleSessionResult(candidate, currentSession) {
  if (
    currentSession === null ||
    typeof currentSession !== "object" ||
    cloudLinkCurrentSessionFields.some(
      (field) => typeof currentSession[field] !== "string",
    )
  ) {
    throw new Error(
      "CloudLink fixture context reducer requires explicit gateway, session, epoch, and credential bindings",
    );
  }
  if (
    cloudLinkCurrentSessionFields.some(
      (field) => candidate[field] !== currentSession[field],
    )
  ) {
    return {
      accepted: false,
      failure_code: "STALE_SESSION",
      state_changed: false,
      successful_receipt_permitted: false,
    };
  }
  return undefined;
}

function digestMismatchResult(candidate) {
  if (
    candidate !== null &&
    typeof candidate === "object" &&
    candidate.delivery !== null &&
    typeof candidate.delivery === "object" &&
    candidate.payload !== undefined &&
    candidate.delivery.digest !== businessDigestForEnvelope(candidate)
  ) {
    return {
      accepted: false,
      failure_code: "DIGEST_MISMATCH",
      state_changed: false,
      successful_receipt_permitted: false,
    };
  }
  return undefined;
}

function expiryRejection(failureCode) {
  return {
    accepted: false,
    failure_code: failureCode,
    state_changed: false,
    successful_receipt_permitted: false,
  };
}

function explicitContextUint64(value, name) {
  if (!validateUint64(value).accepted) {
    throw new Error(`${name} must be an explicit canonical uint64 string`);
  }
  return BigInt(value);
}

export function evaluateExpiryContext(candidate, evaluationTimeMs) {
  if (candidate.expires_at_ms === undefined) {
    return undefined;
  }

  const sentAt = explicitContextUint64(candidate.sent_at_ms, "sent_at_ms");
  const expiresAt = explicitContextUint64(candidate.expires_at_ms, "expires_at_ms");
  if (expiresAt < sentAt) {
    return expiryRejection("INVALID_EXPIRY_WINDOW");
  }

  const evaluationTime = explicitContextUint64(
    evaluationTimeMs,
    "evaluation_time_ms",
  );
  if (evaluationTime >= expiresAt) {
    return expiryRejection("MESSAGE_EXPIRED");
  }
  return undefined;
}

export function durableAckMatchesAcceptedDelivery(ack, acceptedDelivery) {
  const delivery = acceptedDelivery?.delivery;
  return (
    delivery !== null &&
    typeof delivery === "object" &&
    ack.gateway_id === acceptedDelivery.gateway_id &&
    ack.session_id === acceptedDelivery.session_id &&
    ack.session_epoch === acceptedDelivery.session_epoch &&
    ack.credential_generation === acceptedDelivery.credential_generation &&
    ack.stream_id === delivery.stream_id &&
    ack.stream_epoch === delivery.stream_epoch &&
    ack.acknowledged_position === delivery.position &&
    ack.batch_id === delivery.batch_id &&
    ack.digest === delivery.digest
  );
}

export function dataLossRangeIsValid(payload) {
  try {
    const first = BigInt(payload.first_lost_position);
    const last = BigInt(payload.last_lost_position);
    const earliestRetained = BigInt(payload.earliest_retained_position);
    return first <= last && last < earliestRetained;
  } catch {
    return false;
  }
}

export function hasCursorConflict(cursors) {
  if (!Array.isArray(cursors)) {
    return true;
  }
  const identities = new Set();
  for (const cursor of cursors) {
    const identity = JSON.stringify([cursor.stream_id, cursor.stream_epoch]);
    if (identities.has(identity)) {
      return true;
    }
    identities.add(identity);
  }
  return false;
}

function evaluateDurableAckContext(ack, { currentSession, priorAcceptedDelivery }) {
  const stale = staleSessionResult(ack, currentSession);
  if (stale !== undefined) {
    return stale;
  }
  const priorDigestMismatch = digestMismatchResult(priorAcceptedDelivery);
  if (priorDigestMismatch !== undefined) {
    return priorDigestMismatch;
  }
  if (!durableAckMatchesAcceptedDelivery(ack, priorAcceptedDelivery)) {
    throw new Error(
      "durable ACK binding mismatch has no frozen failure code in the alpha profile",
    );
  }
  return {
    accepted: true,
    state_changed: true,
    successful_receipt_permitted: true,
  };
}

export function evaluateCloudLinkDeliveryContext(
  candidate,
  { currentSession, priorAcceptedDelivery, evaluationTimeMs } = {},
) {
  const candidateDigestMismatch = digestMismatchResult(candidate);
  if (candidateDigestMismatch !== undefined) {
    return candidateDigestMismatch;
  }
  if (priorAcceptedDelivery !== undefined) {
    const priorDigestMismatch = digestMismatchResult(priorAcceptedDelivery);
    if (priorDigestMismatch !== undefined) {
      return priorDigestMismatch;
    }
  }
  const expiry = evaluateExpiryContext(candidate, evaluationTimeMs);
  if (expiry !== undefined) {
    return expiry;
  }
  const stale = staleSessionResult(candidate, currentSession);
  if (stale !== undefined) {
    return stale;
  }

  if (priorAcceptedDelivery !== undefined) {
    const sameIdentity = hasSameReplayIdentity(candidate, priorAcceptedDelivery);
    if (
      sameIdentity &&
      (candidate.delivery.batch_id !== priorAcceptedDelivery.delivery.batch_id ||
        candidate.delivery.digest !== priorAcceptedDelivery.delivery.digest)
    ) {
      return {
        accepted: false,
        failure_code: "DIGEST_CONFLICT",
        state_changed: false,
        successful_receipt_permitted: false,
      };
    }
    return {
      accepted: true,
      state_changed: !sameIdentity,
      successful_receipt_permitted: true,
    };
  }

  return {
    accepted: true,
    state_changed: true,
    successful_receipt_permitted: true,
  };
}

function expectedMatches(actual, expected) {
  return isDeepStrictEqual(actual, expected);
}

async function executeCloudLinkScenario(scenario) {
  const candidateEvidence = await validateCloudLinkFixture(
    scenario.input.fixture,
    scenario.input.semantic_mutation,
  );
  const actual = { wire_accepted: candidateEvidence.accepted };
  if (!candidateEvidence.accepted) {
    return actual;
  }

  let priorAcceptedDelivery;
  if (typeof scenario.input.prior_fixture === "string") {
    const priorEvidence = await validateCloudLinkFixture(scenario.input.prior_fixture);
    actual.prior_wire_accepted = priorEvidence.accepted;
    if (!priorEvidence.accepted) {
      return actual;
    }
    priorAcceptedDelivery = priorEvidence.fixture;

    if (
      !isDeepStrictEqual(
        scenario.input.replay_identity_fields,
        CLOUDLINK_REPLAY_IDENTITY_FIELDS,
      )
    ) {
      throw new Error(
        `${scenario.id} must declare the exact CloudLink replay identity tuple`,
      );
    }
  } else if (typeof scenario.input.accepted_delivery_fixture === "string") {
    const acceptedDeliveryEvidence = await validateCloudLinkFixture(
      scenario.input.accepted_delivery_fixture,
    );
    actual.accepted_delivery_wire_accepted = acceptedDeliveryEvidence.accepted;
    if (!acceptedDeliveryEvidence.accepted) {
      return actual;
    }
    priorAcceptedDelivery = acceptedDeliveryEvidence.fixture;
  }

  let contextual;
  if (candidateEvidence.fixture.message_kind === "durable-ack") {
    contextual = evaluateDurableAckContext(candidateEvidence.fixture, {
      currentSession: scenario.input.current_session,
      priorAcceptedDelivery,
    });
  } else if (candidateEvidence.fixture.delivery !== undefined) {
    contextual = evaluateCloudLinkDeliveryContext(candidateEvidence.fixture, {
      currentSession: scenario.input.current_session,
      priorAcceptedDelivery,
      evaluationTimeMs: scenario.input.evaluation_time_ms,
    });
  } else if (candidateEvidence.fixture.message_kind === "session-accepted") {
    contextual = {
      accepted: true,
      state_changed: true,
      successful_receipt_permitted: true,
    };
  } else {
    contextual =
      staleSessionResult(
        candidateEvidence.fixture,
        scenario.input.current_session,
      ) ?? {
        accepted: true,
        state_changed: true,
        successful_receipt_permitted: true,
      };
  }
  if (
    contextual.accepted &&
    candidateEvidence.fixture.message_kind === "data-loss" &&
    !dataLossRangeIsValid(candidateEvidence.fixture.payload)
  ) {
    contextual = {
      accepted: false,
      failure_code: "DATA_LOSS_RANGE_INVALID",
      state_changed: false,
      successful_receipt_permitted: false,
    };
  }
  if (
    contextual.accepted &&
    [candidateEvidence.fixture.cursors, candidateEvidence.fixture.resume].some(
      (cursors) => Array.isArray(cursors) && hasCursorConflict(cursors),
    )
  ) {
    contextual = {
      accepted: false,
      failure_code: "CURSOR_CONFLICT",
      state_changed: false,
      successful_receipt_permitted: false,
    };
  }
  actual.context_accepted = contextual.accepted;
  actual.state_changed = contextual.state_changed;
  actual.successful_receipt_permitted =
    contextual.successful_receipt_permitted;
  if (contextual.failure_code !== undefined) {
    actual.failure_code = contextual.failure_code;
  }
  return actual;
}

async function executeRuntimeManifestChecksumScenario(scenario) {
  if (scenario.input.projection !== "runtime-manifest-without-checksum") {
    throw new Error(`${scenario.id} declares an unsupported digest projection`);
  }
  const evidence = await validateCloudLinkFixture(scenario.input.fixture);
  if (!evidence.accepted) {
    return { wire_accepted: false };
  }
  return {
    wire_accepted: true,
    digest: runtimeManifestChecksum(evidence.fixture.payload.manifest),
  };
}

async function executeThingModelPublicationDigestScenario(scenario) {
  if (scenario.input.projection !== "complete-thing-model") {
    throw new Error(`${scenario.id} declares an unsupported digest projection`);
  }
  const evidence = await validateThingModelFixture(scenario.input.fixture);
  if (!evidence.accepted) {
    return { wire_accepted: false };
  }
  return {
    wire_accepted: true,
    digest: thingModelPublicationDigest(evidence.fixture),
  };
}

async function executeScenario(scenario) {
  if (
    scenario.operation === "digest" &&
    scenario.input.projection === "runtime-manifest-without-checksum"
  ) {
    return executeRuntimeManifestChecksumScenario(scenario);
  }
  if (
    scenario.operation === "digest" &&
    scenario.input.projection === "complete-thing-model"
  ) {
    return executeThingModelPublicationDigestScenario(scenario);
  }
  if (scenario.operation !== "validate") {
    throw new Error(
      `implemented scenario ${scenario.id} has no ${scenario.operation} handler`,
    );
  }
  if (scenario.input.type === "raw-json") {
    return executeRawJsonScenario(scenario);
  }
  if (scenario.input.type === "uint64") {
    return validateUint64(scenario.input.value);
  }
  if (typeof scenario.input.fixture === "string") {
    if (scenario.input.fixture.startsWith("fixtures/thing-model/v1alpha1/")) {
      return executeThingModelScenario(scenario);
    }
    return executeCloudLinkScenario(scenario);
  }
  throw new Error(`implemented scenario ${scenario.id} has no input handler`);
}

export async function runScenarioSet(scenarioSet) {
  await validateScenarioSet(scenarioSet);
  const summary = {
    implemented: 0,
    executed: [],
    blocked: [],
    blocked_count: 0,
    planned: [],
    planned_count: 0,
  };

  for (const scenario of scenarioSet.scenarios) {
    if (scenario.status === "blocked") {
      summary.blocked.push(scenario.id);
      summary.blocked_count += 1;
      continue;
    }
    if (scenario.status === "planned") {
      summary.planned.push(scenario.id);
      summary.planned_count += 1;
      continue;
    }
    if (scenario.status !== "implemented") {
      throw new Error(`scenario ${scenario.id} must declare an execution status`);
    }

    summary.implemented += 1;
    try {
      const actual = await executeScenario(scenario);
      summary.executed.push({
        id: scenario.id,
        actual,
        passed: expectedMatches(actual, scenario.expected),
      });
    } catch (error) {
      summary.executed.push({
        id: scenario.id,
        actual: {
          execution_error: error instanceof Error ? error.message : String(error),
        },
        passed: false,
      });
    }
  }

  return summary;
}
