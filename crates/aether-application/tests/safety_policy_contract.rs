use std::collections::{BTreeMap, BTreeSet};

use aether_application::{
    AuditPolicy, ConfirmationPolicy, OperationKind, RiskLevel, capability_catalog,
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct SafetyPolicyDocument {
    defaults: SafetyDefaults,
    capabilities: BTreeMap<String, CapabilityPolicy>,
}

#[derive(Debug, Deserialize)]
struct SafetyDefaults {
    audit: String,
}

#[derive(Debug, Deserialize)]
struct CapabilityPolicy {
    kind: String,
    risk: String,
    permission: String,
    idempotent: bool,
    confirmation: String,
    audit: Option<String>,
}

#[test]
fn channel_management_capabilities_remain_high_risk_and_audited() {
    let policy: SafetyPolicyDocument =
        serde_yml::from_str(include_str!("../../../ai/safety-policy.yaml"))
            .expect("safety policy is valid YAML");

    for capability in ["io.channel.manage", "io.channel.reconcile"] {
        let declared = policy
            .capabilities
            .get(capability)
            .unwrap_or_else(|| panic!("{capability} is missing from safety-policy.yaml"));
        assert_eq!(declared.kind, "command", "{capability} kind");
        assert_eq!(declared.risk, "high", "{capability} risk");
        assert_eq!(
            declared.permission, "io.channel.manage",
            "{capability} permission"
        );
        assert!(!declared.idempotent, "{capability} idempotency");
        assert_eq!(declared.confirmation, "always", "{capability} confirmation");
        assert_eq!(
            declared.audit.as_deref().unwrap_or(&policy.defaults.audit),
            "required",
            "{capability} audit policy"
        );
    }
}

#[test]
fn machine_readable_safety_policy_matches_the_rust_capability_catalog() {
    let policy: SafetyPolicyDocument =
        serde_yml::from_str(include_str!("../../../ai/safety-policy.yaml"))
            .expect("safety policy is valid YAML");
    let rust_names: BTreeSet<_> = capability_catalog()
        .iter()
        .map(|descriptor| descriptor.name().to_string())
        .collect();
    let policy_names: BTreeSet<_> = policy.capabilities.keys().cloned().collect();
    assert_eq!(
        policy_names, rust_names,
        "safety-policy.yaml must describe exactly the exposed Rust capabilities"
    );

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
        let expected_confirmation = match descriptor.confirmation() {
            ConfirmationPolicy::Never => "never",
            ConfirmationPolicy::Policy => "policy",
            ConfirmationPolicy::Always => "always",
        };
        assert_eq!(
            declared.confirmation,
            expected_confirmation,
            "{} confirmation",
            descriptor.name()
        );
        let expected_audit = match descriptor.audit_policy() {
            AuditPolicy::NotRequired => "not_required",
            AuditPolicy::Required => "required",
        };
        assert_eq!(
            declared.audit.as_deref().unwrap_or(&policy.defaults.audit),
            expected_audit,
            "{} audit policy",
            descriptor.name()
        );
    }
}
