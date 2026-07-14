use aether_domain::RuleId;
use aether_ports::{
    AutomationRulesRevision, PortError, PortErrorKind, RevisionedRuleMutation, RuleMutation,
    RuleMutationKind, RuleMutationReceipt,
};

#[test]
fn every_rule_mutation_carries_a_mandatory_aggregate_revision() {
    let expected = AutomationRulesRevision::new(7);
    let mutations = [
        RevisionedRuleMutation::create("rule", None, expected),
        RevisionedRuleMutation::set_enabled(RuleId::new(3), true, expected),
        RevisionedRuleMutation::delete(RuleId::new(3), expected),
        RevisionedRuleMutation::reload(expected),
    ];

    for mutation in mutations {
        assert_eq!(mutation.expected_revision(), expected);
    }
}

#[test]
fn legacy_rule_mutation_constructors_remain_revisionless() {
    let mutations = [
        RuleMutation::create("rule", None),
        RuleMutation::set_enabled(RuleId::new(3), true),
        RuleMutation::delete(RuleId::new(3)),
        RuleMutation::Reload,
    ];

    assert_eq!(mutations[0].kind(), RuleMutationKind::Create);
    assert_eq!(mutations[1].kind(), RuleMutationKind::Enable);
    assert_eq!(mutations[2].kind(), RuleMutationKind::Delete);
    assert_eq!(mutations[3].kind(), RuleMutationKind::Reload);
}

#[test]
fn rule_receipts_preserve_the_committed_revision_and_runtime_state() {
    let resulting = AutomationRulesRevision::new(8);
    let refreshed =
        RuleMutationReceipt::new_at_revision(RuleId::new(3), RuleMutationKind::Update, resulting);
    assert_eq!(refreshed.resulting_revision(), resulting);
    assert!(refreshed.scheduler_refresh().is_refreshed());

    let gated = RuleMutationReceipt::point_watch_gated(
        Some(RuleId::new(3)),
        RuleMutationKind::Update,
        resulting,
        PortError::new(PortErrorKind::Unavailable, "manifest unavailable"),
    );
    assert_eq!(gated.resulting_revision(), resulting);
    assert_eq!(gated.runtime_status().as_str(), "point_watch_gated");
    assert!(gated.runtime_status().reconciliation_required());
    assert!(gated.scheduler_refresh().is_refreshed());

    let stopped = RuleMutationReceipt::scheduler_stopped_at_revision(
        Some(RuleId::new(3)),
        RuleMutationKind::Update,
        resulting,
        PortError::new(PortErrorKind::Unavailable, "rule reload failed"),
    );
    assert_eq!(stopped.resulting_revision(), resulting);
    assert_eq!(stopped.scheduler_refresh().as_str(), "stopped");
}
