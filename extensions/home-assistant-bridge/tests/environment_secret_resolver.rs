use aether_home_assistant_bridge::EnvironmentSecretResolver;
use aether_ports::{PortErrorKind, SecretRef, SecretResolver};

#[tokio::test]
async fn environment_resolver_accepts_only_bounded_environment_references() {
    let expected = std::env::var("PATH").expect("test process PATH");
    let resolver = EnvironmentSecretResolver;
    let reference = SecretRef::new("env:PATH").expect("secret reference");

    let material = resolver.resolve(&reference).await.expect("PATH secret");

    assert_eq!(material.expose(), expected);
    assert!(!format!("{resolver:?}").contains(&expected));
    assert!(!format!("{material:?}").contains(&expected));
}

#[tokio::test]
async fn environment_resolver_rejects_other_schemes_without_echoing_the_reference() {
    let resolver = EnvironmentSecretResolver;
    let reference =
        SecretRef::new("inline:secret-marker-that-must-not-leak").expect("secret reference");

    let error = resolver
        .resolve(&reference)
        .await
        .expect_err("inline material must be rejected");

    assert_eq!(error.kind(), PortErrorKind::Permanent);
    assert!(
        !error
            .to_string()
            .contains("secret-marker-that-must-not-leak")
    );
}

#[tokio::test]
async fn environment_resolver_rejects_invalid_or_missing_variables_without_echoing_values() {
    let resolver = EnvironmentSecretResolver;

    for reference in [
        "env:",
        "env:lowercase",
        "env:1STARTS_WITH_DIGIT",
        "env:HAS-DASH",
        "env:AETHER_HOME_ASSISTANT_TEST_VARIABLE_THAT_MUST_NOT_EXIST_7A4C",
    ] {
        let reference = SecretRef::new(reference).expect("opaque secret reference");
        let error = resolver
            .resolve(&reference)
            .await
            .expect_err("invalid or missing environment variable must fail");
        assert_eq!(error.kind(), PortErrorKind::Permanent);
        assert!(!error.to_string().contains(reference.as_str()));
    }
}
