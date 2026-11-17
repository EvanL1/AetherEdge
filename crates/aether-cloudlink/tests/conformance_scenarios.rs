use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use aether_cloudlink::{CandidateMessage, CloudLinkCodec, CloudLinkCodecError, SessionBinding};
use aether_integration_contract::{IntegrationContractCodec, TOPOLOGY_SNAPSHOT_SCHEMA};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const SCENARIO_SET: &str = "tck/scenarios/core.json";
const UINT64_CARRIER_FIXTURE: &str = "fixtures/cloudlink/v1alpha1/heartbeat.valid.json";
const UINT64_CARRIER_MEMBER: &str = "observed_at_ms";
const UNEVALUATED_CONTEXT: [&str; 2] = ["evaluation_time_ms", "accepted_delivery_fixture"];

const SKIPPED: &[(&str, &[&str])] = &[
    (
        "replay state: needs a previously accepted delivery",
        &[
            "cloudlink-idempotent-replay",
            "cloudlink-conflicting-digest",
            "cloudlink-conflicting-batch-binding",
        ],
    ),
    (
        "receiver clock: Edge sends deliveries and never evaluates their deadline",
        &["cloudlink-expired-delivery"],
    ),
    (
        "durable ACK prefix evidence: held by the Cloud store",
        &[
            "cloudlink-ack-gap-blocks-prefix",
            "cloudlink-ack-gap-declared-lost",
            "cloudlink-ack-gap-other-epoch-does-not-cover",
            "cloudlink-ack-gap-filled-out-of-order",
        ],
    ),
    (
        "Thing Model: outside the adopted modules, fixtures not imported",
        &[
            "thing-model-cross-namespace-key-conflict",
            "thing-model-duplicate-capability-parameter",
            "thing-model-publication-digest",
        ],
    ),
    (
        "signature verification: blocked by the kit on an unresolved profile",
        &["cloudlink-shared-broker-attestation"],
    ),
];

const KNOWN_DIVERGENCES: &[(&str, Outcome<'static>)] = &[
    (
        "uint64-long-mixed-out-of-range",
        Outcome::Rejected("INTEGER_NON_CANONICAL"),
    ),
    (
        "uint64-arabic-indic-byte-overflow",
        Outcome::Rejected("INTEGER_NON_CANONICAL"),
    ),
    (
        "cloudlink-data-loss-invalid-range",
        Outcome::Rejected("FIELD_BOUND"),
    ),
    ("cloudlink-duplicate-cursor", Outcome::Accepted),
    ("raw-json-duplicate-key", Outcome::Accepted),
    ("raw-json-escaped-duplicate-key", Outcome::Accepted),
    (
        "raw-json-lone-surrogate",
        Outcome::Rejected("JSON_SYNTAX_ERROR"),
    ),
    ("raw-json-unsafe-integer", Outcome::Accepted),
    ("raw-json-unsafe-integer-exponent", Outcome::Accepted),
    ("raw-json-unsafe-integer-fraction", Outcome::Accepted),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome<'a> {
    Accepted,
    Rejected(&'a str),
}

struct PinnedContract {
    repository_root: PathBuf,
    imports: BTreeMap<String, String>,
}

impl PinnedContract {
    fn load() -> Self {
        let repository_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let lock: Value = serde_json::from_slice(
            &fs::read(repository_root.join("aether-contracts.lock.json")).expect("consumer lock"),
        )
        .expect("consumer lock JSON");
        let imports = lock["imports"]
            .as_array()
            .expect("lock imports")
            .iter()
            .map(|import| {
                (
                    text(import, "source").to_owned(),
                    text(import, "destination").to_owned(),
                )
            })
            .collect();
        Self {
            repository_root,
            imports,
        }
    }

    fn read(&self, source: &str) -> Vec<u8> {
        let destination = self
            .imports
            .get(source)
            .unwrap_or_else(|| panic!("`{source}` is not imported by the consumer lock"));
        fs::read(self.repository_root.join(destination))
            .unwrap_or_else(|error| panic!("{destination}: {error}"))
    }

    fn json(&self, source: &str) -> Value {
        serde_json::from_slice(&self.read(source))
            .unwrap_or_else(|error| panic!("{source}: {error}"))
    }

    fn scenarios(&self) -> Vec<Value> {
        self.json(SCENARIO_SET)["scenarios"]
            .as_array()
            .expect("scenario list")
            .clone()
    }
}

fn text<'a>(value: &'a Value, member: &str) -> &'a str {
    value[member]
        .as_str()
        .unwrap_or_else(|| panic!("`{member}` must be a string in {value}"))
}

fn require_known_members(id: &str, object: &Value, known: &[&str]) {
    for member in object.as_object().expect("JSON object").keys() {
        assert!(
            known.contains(&member.as_str()),
            "{id}: `{member}` has no executor; execute it or add the scenario to SKIPPED"
        );
    }
}

fn skipped_ids() -> Vec<&'static str> {
    SKIPPED
        .iter()
        .flat_map(|(_, ids)| ids.iter().copied())
        .collect()
}

fn expected_outcome(scenario: &Value) -> Outcome<'_> {
    let expected = &scenario["expected"];
    if let Some(code) = expected["failure_code"].as_str() {
        return Outcome::Rejected(code);
    }
    let accepted = expected["accepted"] == true
        || (expected["wire_accepted"] == true
            && (expected["context_accepted"] == true || expected.get("digest").is_some()));
    assert!(accepted, "{}: unreadable expectation", scenario["id"]);
    Outcome::Accepted
}

fn rejected(error: &CloudLinkCodecError) -> Outcome<'static> {
    Outcome::Rejected(error.failure_code())
}

fn passed_strict_json(result: &Result<CandidateMessage, CloudLinkCodecError>) -> bool {
    matches!(
        result,
        Ok(_)
            | Err(CloudLinkCodecError::InvalidField {
                field: "schema",
                ..
            })
    )
}

fn execute(contract: &PinnedContract, scenario: &Value) -> Outcome<'static> {
    let id = text(scenario, "id");
    let input = &scenario["input"];
    match (
        text(scenario, "operation"),
        input["type"].as_str(),
        input["fixture"].as_str(),
    ) {
        ("validate", Some("uint64"), None) => execute_uint64(contract, id, input),
        ("validate", Some("raw-json"), None) => execute_raw_json(id, input),
        ("validate", None, Some(fixture)) if fixture.starts_with("fixtures/integration/") => {
            execute_integration(contract, id, input)
        },
        ("validate" | "digest", None, Some(fixture))
            if fixture.starts_with("fixtures/cloudlink/") =>
        {
            execute_cloudlink(contract, scenario)
        },
        _ => panic!("{id}: no executor; execute it or add the scenario to SKIPPED"),
    }
}

fn execute_uint64(contract: &PinnedContract, id: &str, input: &Value) -> Outcome<'static> {
    require_known_members(id, input, &["type", "value"]);
    let mut carrier = contract.json(UINT64_CARRIER_FIXTURE);
    carrier[UINT64_CARRIER_MEMBER] = input["value"].clone();
    match CloudLinkCodec::decode(&serde_json::to_vec(&carrier).expect("carrier JSON")) {
        Ok(_) => Outcome::Accepted,
        Err(error) => rejected(&error),
    }
}

fn execute_raw_json(id: &str, input: &Value) -> Outcome<'static> {
    require_known_members(id, input, &["type", "encoding", "raw"]);
    let raw = text(input, "raw");
    let bytes = match text(input, "encoding") {
        "utf8" => raw.as_bytes().to_vec(),
        "hex" => (0..raw.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&raw[index..index + 2], 16).expect("hex byte"))
            .collect(),
        other => panic!("{id}: unknown raw encoding `{other}`"),
    };
    let result = CloudLinkCodec::decode(&bytes);
    if passed_strict_json(&result) {
        return Outcome::Accepted;
    }
    result.map_or_else(|error| rejected(&error), |_| Outcome::Accepted)
}

fn execute_integration(contract: &PinnedContract, id: &str, input: &Value) -> Outcome<'static> {
    require_known_members(id, input, &["fixture", "topology_fixture"]);
    let bytes = contract.read(text(input, "fixture"));
    let document: Value = serde_json::from_slice(&bytes).expect("integration fixture JSON");
    let result = if document["schema"] == TOPOLOGY_SNAPSHOT_SCHEMA {
        IntegrationContractCodec::decode_topology(&bytes).map(drop)
    } else {
        let topology = IntegrationContractCodec::decode_topology(
            &contract.read(text(input, "topology_fixture")),
        )
        .expect("topology context fixture");
        IntegrationContractCodec::decode_observation_batch(&bytes, &topology).map(drop)
    };
    match result {
        Ok(()) => Outcome::Accepted,
        Err(error) => Outcome::Rejected(error.code().as_str()),
    }
}

fn execute_cloudlink(contract: &PinnedContract, scenario: &Value) -> Outcome<'static> {
    let id = text(scenario, "id");
    let input = &scenario["input"];
    let evaluated = [
        "fixture",
        "semantic_mutation",
        "current_session",
        "projection",
    ];
    require_known_members(
        id,
        input,
        &[&evaluated[..], &UNEVALUATED_CONTEXT[..]].concat(),
    );

    let fixture = text(input, "fixture");
    let bytes = match input.get("semantic_mutation") {
        None => contract.read(fixture),
        Some(mutation) => serde_json::to_vec(&mutated(id, contract.json(fixture), mutation))
            .expect("mutated fixture JSON"),
    };
    let message = match CloudLinkCodec::decode(&bytes) {
        Ok(message) => message,
        Err(error) => return rejected(&error),
    };
    if let Some(context) = input.get("current_session")
        && let Err(error) = message.validate_session(&session(context))
    {
        return rejected(&error);
    }
    if scenario["operation"] == "digest" {
        assert_eq!(
            input["projection"], "runtime-manifest-without-checksum",
            "{id}"
        );
        let CandidateMessage::Delivery(envelope) = &message else {
            panic!("{id}: a Runtime Manifest report is a delivery envelope");
        };
        assert_eq!(
            envelope.payload()["manifest"]["checksum"]["digest"],
            scenario["expected"]["digest"],
            "{id}: the checksum the codec verified is not the one the kit computes"
        );
    }
    Outcome::Accepted
}

fn mutated(id: &str, mut document: Value, mutation: &Value) -> Value {
    require_known_members(
        id,
        mutation,
        &[
            "top_level_overrides",
            "payload_overrides",
            "duplicate_cursor_index",
            "recompute_business_digest",
        ],
    );
    if let Some(overrides) = mutation.get("top_level_overrides") {
        merge(&mut document, overrides);
    }
    if let Some(overrides) = mutation.get("payload_overrides") {
        merge(&mut document["payload"], overrides);
    }
    if let Some(index) = mutation.get("duplicate_cursor_index") {
        let index = usize::try_from(index.as_u64().expect("cursor index")).expect("cursor index");
        let cursors = document["cursors"].as_array_mut().expect("cursor array");
        let duplicate = cursors[index].clone();
        cursors.push(duplicate);
    }
    if mutation.get("recompute_business_digest") == Some(&Value::Bool(true)) {
        document["delivery"]["digest"] = Value::String(business_digest(&document));
    }
    document
}

fn merge(target: &mut Value, overrides: &Value) {
    for (member, value) in overrides.as_object().expect("override object") {
        target[member] = value.clone();
    }
}

fn business_digest(envelope: &Value) -> String {
    let projection = json!({
        "protocol_version": envelope["protocol_version"],
        "message_kind": envelope["message_kind"],
        "payload": envelope["payload"],
    });
    let canonical = serde_json_canonicalizer::to_vec(&projection).expect("canonical projection");
    format!("sha256:{:x}", Sha256::digest(canonical))
}

fn session(context: &Value) -> SessionBinding {
    let uint64 = |member| {
        text(context, member)
            .parse::<u64>()
            .expect("canonical uint64 session context")
    };
    SessionBinding::new(
        text(context, "gateway_id"),
        text(context, "session_id"),
        uint64("session_epoch"),
        uint64("credential_generation"),
    )
    .expect("current session context")
}

#[test]
fn every_pinned_scenario_is_executed_or_skipped_by_id() {
    let scenarios = PinnedContract::load().scenarios();
    let pinned: Vec<&str> = scenarios
        .iter()
        .map(|scenario| text(scenario, "id"))
        .collect();
    let skipped = skipped_ids();

    for id in &skipped {
        assert!(
            pinned.contains(id),
            "SKIPPED names `{id}`, which the kit does not define"
        );
    }
    for (id, _) in KNOWN_DIVERGENCES {
        assert!(
            pinned.contains(id) && !skipped.contains(id),
            "KNOWN_DIVERGENCES names `{id}`, which is not an executed scenario"
        );
    }
    let executed = pinned.iter().filter(|id| !skipped.contains(id)).count();
    assert_eq!((pinned.len(), executed, skipped.len()), (46, 34, 12));
}

#[test]
fn the_codec_reaches_the_outcome_every_executed_scenario_expects() {
    let contract = PinnedContract::load();
    let skipped = skipped_ids();
    let mut disagreements = Vec::new();

    for scenario in contract.scenarios() {
        let id = text(&scenario, "id");
        if skipped.contains(&id) {
            continue;
        }
        let expected = expected_outcome(&scenario);
        let actual = execute(&contract, &scenario);
        let recorded = KNOWN_DIVERGENCES
            .iter()
            .find(|(known, _)| *known == id)
            .map(|(_, divergence)| *divergence);
        match recorded {
            None if actual == expected => {},
            Some(divergence) if actual == divergence && actual != expected => {},
            None => disagreements.push(format!(
                "{id}: the kit expects {expected:?}, the codec reaches {actual:?}"
            )),
            Some(divergence) => disagreements.push(format!(
                "{id}: recorded as diverging with {divergence:?}, but the codec reaches \
                 {actual:?} and the kit expects {expected:?}"
            )),
        }
    }

    assert!(disagreements.is_empty(), "\n{}", disagreements.join("\n"));
}
