use aether_domain::{
    AcquiredPointSample, AlarmComparator, AlarmRuleDefinition, AlarmRuleTarget, AlarmSeverity,
    AlertId, ChannelCommandAddress, ChannelId, ChannelPointAddress, CommandConstraints, CommandId,
    ControlCommand, DomainError, InstanceId, PhysicalDeviceCommand, PointAddress, PointId,
    PointKind, PointQuality, PointSample, TimestampMs,
};

#[test]
fn identifiers_remain_distinct_and_round_trip_their_raw_values() {
    let instance_id = InstanceId::new(42);
    let point_id = PointId::new(7);
    let command_id = CommandId::new(99);

    assert_eq!(instance_id.get(), 42);
    assert_eq!(point_id.get(), 7);
    assert_eq!(command_id.get(), 99);
    assert_eq!(AlertId::new(11).get(), 11);
}

#[test]
fn alarm_rule_definition_rejects_invalid_policy_and_supports_channel_health() {
    assert_eq!(
        AlarmSeverity::new(0),
        Err(DomainError::InvalidAlarmSeverity)
    );
    assert_eq!(
        AlarmSeverity::new(4),
        Err(DomainError::InvalidAlarmSeverity)
    );
    assert_eq!(
        AlarmComparator::try_from("~="),
        Err(DomainError::InvalidAlarmComparator)
    );
    assert!(matches!(
        AlarmRuleTarget::point("", ChannelId::new(1), "T", PointId::new(2)),
        Err(DomainError::InvalidAlarmTarget)
    ));

    let target = AlarmRuleTarget::channel_online(ChannelId::new(7));
    let definition = AlarmRuleDefinition::new(
        target.clone(),
        "gateway offline",
        AlarmSeverity::new(3).expect("severity"),
        AlarmComparator::Equal,
        0.0,
        false,
        Some("commission only after site acceptance".to_string()),
    )
    .expect("valid channel-health alarm");
    assert_eq!(definition.target(), &target);
    assert_eq!(definition.comparator().as_str(), "==");

    assert_eq!(
        AlarmRuleDefinition::new(
            target.clone(),
            " ",
            AlarmSeverity::new(2).expect("severity"),
            AlarmComparator::GreaterThan,
            1.0,
            false,
            None,
        ),
        Err(DomainError::InvalidAlarmRuleName)
    );
    assert_eq!(
        AlarmRuleDefinition::new(
            target,
            "offline",
            AlarmSeverity::new(2).expect("severity"),
            AlarmComparator::GreaterThan,
            f64::NAN,
            false,
            None,
        ),
        Err(DomainError::NonFiniteAlarmThreshold)
    );
}

#[test]
fn point_sample_preserves_address_value_time_and_quality() {
    let address = PointAddress::new(InstanceId::new(42), PointKind::Telemetry, PointId::new(7));
    let sample = PointSample::new(
        address,
        23.5,
        TimestampMs::new(1_720_000_000_000),
        PointQuality::Good,
    );

    assert_eq!(sample.address(), address);
    assert_eq!(sample.value(), 23.5);
    assert_eq!(sample.timestamp().get(), 1_720_000_000_000);
    assert_eq!(sample.quality(), PointQuality::Good);
}

#[test]
fn acquired_samples_use_physical_channel_identity() {
    let address =
        ChannelPointAddress::new(ChannelId::new(17), PointKind::Telemetry, PointId::new(3))
            .expect("telemetry is acquisition-owned");
    let sample = AcquiredPointSample::new(
        address,
        23.5,
        2_350.0,
        TimestampMs::new(1_720_000_000_000),
        PointQuality::Good,
    )
    .expect("finite acquisition sample is valid");

    assert_eq!(address.channel_id(), ChannelId::new(17));
    assert_eq!(address.kind(), PointKind::Telemetry);
    assert_eq!(address.point_id(), PointId::new(3));
    assert_eq!(sample.address(), address);
    assert_eq!(sample.value(), 23.5);
    assert_eq!(sample.raw(), 2_350.0);
    assert_eq!(sample.timestamp().get(), 1_720_000_000_000);
    assert_eq!(sample.quality(), PointQuality::Good);
}

#[test]
fn acquisition_addresses_reject_command_owned_point_kinds() {
    for kind in [PointKind::Command, PointKind::Action] {
        assert_eq!(
            ChannelPointAddress::new(ChannelId::new(17), kind, PointId::new(3)),
            Err(DomainError::PointNotAcquisitionOwned(kind))
        );
    }
}

#[test]
fn acquired_samples_reject_non_finite_values() {
    let address = ChannelPointAddress::new(ChannelId::new(17), PointKind::Status, PointId::new(3))
        .expect("status is acquisition-owned");

    assert_eq!(
        AcquiredPointSample::new(
            address,
            f64::NAN,
            1.0,
            TimestampMs::new(1),
            PointQuality::Bad,
        ),
        Err(DomainError::NonFiniteAcquiredValue)
    );
    assert_eq!(
        AcquiredPointSample::new(
            address,
            1.0,
            f64::INFINITY,
            TimestampMs::new(1),
            PointQuality::Bad,
        ),
        Err(DomainError::NonFiniteAcquiredRawValue)
    );
}

#[test]
fn physical_command_addresses_accept_only_command_owned_points() {
    for kind in [PointKind::Command, PointKind::Action] {
        let address = ChannelCommandAddress::new(ChannelId::new(17), kind, PointId::new(3))
            .expect("C/A is command-owned");
        assert_eq!(address.channel_id(), ChannelId::new(17));
        assert_eq!(address.kind(), kind);
        assert_eq!(address.point_id(), PointId::new(3));
    }

    for kind in [PointKind::Telemetry, PointKind::Status] {
        assert_eq!(
            ChannelCommandAddress::new(ChannelId::new(17), kind, PointId::new(3)),
            Err(DomainError::PointNotCommandOwned(kind))
        );
    }
}

#[test]
fn physical_device_commands_preserve_identity_value_and_ttl() {
    let target = ChannelCommandAddress::new(ChannelId::new(17), PointKind::Action, PointId::new(3))
        .expect("action target");
    let command = PhysicalDeviceCommand::new(
        CommandId::new(9),
        target,
        42.5,
        TimestampMs::new(1_000),
        TimestampMs::new(1_100),
    )
    .expect("valid physical command");

    assert_eq!(command.id(), CommandId::new(9));
    assert_eq!(command.target(), target);
    assert_eq!(command.value(), 42.5);
    assert_eq!(command.issued_at(), TimestampMs::new(1_000));
    assert_eq!(command.expires_at(), TimestampMs::new(1_100));
    assert_eq!(command.validate_at(TimestampMs::new(1_099)), Ok(()));
    assert_eq!(
        command.validate_at(TimestampMs::new(1_100)),
        Err(DomainError::CommandExpired)
    );
}

#[test]
fn physical_device_commands_reject_non_finite_values_and_invalid_windows() {
    let target =
        ChannelCommandAddress::new(ChannelId::new(17), PointKind::Command, PointId::new(3))
            .expect("command target");

    assert_eq!(
        PhysicalDeviceCommand::new(
            CommandId::new(9),
            target,
            f64::INFINITY,
            TimestampMs::new(1_000),
            TimestampMs::new(1_100),
        ),
        Err(DomainError::NonFiniteCommandValue)
    );
    assert_eq!(
        PhysicalDeviceCommand::new(
            CommandId::new(9),
            target,
            1.0,
            TimestampMs::new(1_000),
            TimestampMs::new(1_000),
        ),
        Err(DomainError::InvalidCommandWindow)
    );
}

#[test]
fn control_commands_accept_only_writable_point_kinds() {
    let action = PointAddress::new(InstanceId::new(9), PointKind::Action, PointId::new(3));
    let command = ControlCommand::new(
        CommandId::new(1),
        action,
        1.0,
        TimestampMs::new(100),
        TimestampMs::new(200),
    );
    assert!(command.is_ok());

    let telemetry = PointAddress::new(InstanceId::new(9), PointKind::Telemetry, PointId::new(3));
    let error = ControlCommand::new(
        CommandId::new(2),
        telemetry,
        1.0,
        TimestampMs::new(101),
        TimestampMs::new(201),
    )
    .expect_err("telemetry is read-only");

    assert_eq!(error, DomainError::PointNotWritable(PointKind::Telemetry));
}

#[test]
fn control_commands_reject_non_finite_values_and_invalid_deadlines() {
    let target = PointAddress::new(InstanceId::new(9), PointKind::Action, PointId::new(3));

    let non_finite = ControlCommand::new(
        CommandId::new(1),
        target,
        f64::NAN,
        TimestampMs::new(100),
        TimestampMs::new(200),
    )
    .expect_err("NaN must never reach a device");
    assert_eq!(non_finite, DomainError::NonFiniteCommandValue);

    let invalid_window = ControlCommand::new(
        CommandId::new(2),
        target,
        1.0,
        TimestampMs::new(200),
        TimestampMs::new(200),
    )
    .expect_err("a command must have a future deadline");
    assert_eq!(invalid_window, DomainError::InvalidCommandWindow);
}

#[test]
fn command_constraints_reject_invalid_configuration() {
    assert_eq!(
        CommandConstraints::new(Some(10.0), Some(5.0), None),
        Err(DomainError::InvalidCommandConstraints)
    );
    assert_eq!(
        CommandConstraints::new(None, None, Some(0.0)),
        Err(DomainError::InvalidCommandConstraints)
    );
    assert_eq!(
        CommandConstraints::new(Some(f64::NEG_INFINITY), None, None),
        Err(DomainError::InvalidCommandConstraints)
    );
}

#[test]
fn control_commands_are_checked_against_deadline_range_and_step() {
    let target = PointAddress::new(InstanceId::new(9), PointKind::Action, PointId::new(3));
    let command = ControlCommand::new(
        CommandId::new(1),
        target,
        10.5,
        TimestampMs::new(100),
        TimestampMs::new(200),
    )
    .expect("command envelope is structurally valid");
    let constraints =
        CommandConstraints::new(Some(10.0), Some(12.0), Some(0.5)).expect("constraints are valid");

    assert_eq!(
        command.validate_at(TimestampMs::new(199), constraints),
        Ok(())
    );
    assert_eq!(
        command.validate_at(TimestampMs::new(200), constraints),
        Err(DomainError::CommandExpired)
    );

    let too_high = ControlCommand::new(
        CommandId::new(2),
        target,
        12.5,
        TimestampMs::new(100),
        TimestampMs::new(200),
    )
    .expect("command envelope is structurally valid");
    assert_eq!(
        too_high.validate_at(TimestampMs::new(150), constraints),
        Err(DomainError::CommandValueOutOfRange)
    );

    let off_step = ControlCommand::new(
        CommandId::new(3),
        target,
        10.25,
        TimestampMs::new(100),
        TimestampMs::new(200),
    )
    .expect("command envelope is structurally valid");
    assert_eq!(
        off_step.validate_at(TimestampMs::new(150), constraints),
        Err(DomainError::CommandValueOffStep)
    );
}

#[test]
fn step_tolerance_does_not_expand_with_large_absolute_values() {
    let constraints =
        CommandConstraints::new(Some(0.0), Some(2.0e12), Some(0.5)).expect("constraints are valid");

    assert_eq!(
        constraints.validate_value(1.0e12 + 0.25),
        Err(DomainError::CommandValueOffStep)
    );
    assert_eq!(constraints.validate_value(1.0e12 + 0.5), Ok(()));
}
