use std::collections::BTreeMap;

use aether_application::{OperationKind, RiskLevel, capability_catalog};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct SafetyPolicyDocument {
    capabilities: BTreeMap<String, CapabilityPolicy>,
}

#[derive(Debug, Deserialize)]
struct CapabilityPolicy {
    kind: String,
    risk: String,
    permission: String,
    idempotent: bool,
    confirmation: Option<String>,
}

#[test]
fn machine_readable_safety_policy_matches_the_rust_capability_catalog() {
    let policy: SafetyPolicyDocument =
        serde_yml::from_str(include_str!("../../../ai/safety-policy.yaml"))
            .expect("safety policy is valid YAML");

    for descriptor in capability_catalog() {
        let declared = policy
            .capabilities
            .get(descriptor.name())
            .unwrap_or_else(|| panic!("{} is missing from safety-policy.yaml", descriptor.name()));
        let expected_kind = match descriptor.kind() {
            OperationKind::Query => "query",
            OperationKind::Command => "command",
        };
        let expected_risk = match descriptor.risk() {
            RiskLevel::Low => "low",
            RiskLevel::Medium => "medium",
            RiskLevel::High => "high",
        };

        assert_eq!(declared.kind, expected_kind, "{} kind", descriptor.name());
        assert_eq!(declared.risk, expected_risk, "{} risk", descriptor.name());
        assert_eq!(
            declared.permission,
            descriptor.required_permission(),
            "{} permission",
            descriptor.name()
        );
        assert_eq!(
            declared.idempotent,
            descriptor.is_idempotent(),
            "{} idempotency",
            descriptor.name()
        );
        assert_eq!(
            declared.confirmation.as_deref() == Some("always"),
            descriptor.requires_confirmation(),
            "{} confirmation",
            descriptor.name()
        );
    }
}
