use std::fmt::Write;
use std::sync::Arc;

use aether_ports::{PortResult, SecretMaterial, SecretRef, SecretResolver};
use async_trait::async_trait;

struct StaticSecretResolver;

#[async_trait]
impl SecretResolver for StaticSecretResolver {
    async fn resolve(&self, reference: &SecretRef) -> PortResult<SecretMaterial> {
        assert_eq!(reference.as_str(), "env:AETHER_HOME_ASSISTANT_TOKEN");
        SecretMaterial::new("never-print-this-token")
    }
}

#[tokio::test]
async fn secret_resolver_is_object_safe_and_material_is_redacted() {
    let resolver: Arc<dyn SecretResolver> = Arc::new(StaticSecretResolver);
    let reference =
        SecretRef::new("env:AETHER_HOME_ASSISTANT_TOKEN").expect("secret reference is valid");
    let material = resolver.resolve(&reference).await.expect("secret resolves");

    assert_eq!(material.expose(), "never-print-this-token");
    let mut rendered = String::new();
    write!(&mut rendered, "{material:?}").expect("debug rendering");
    assert_eq!(rendered, "SecretMaterial([REDACTED])");
    assert!(!rendered.contains(material.expose()));
}

#[test]
fn secret_reference_and_material_reject_unsafe_empty_or_unbounded_input() {
    assert!(SecretRef::new(" ").is_err());
    assert!(SecretRef::new("a".repeat(513)).is_err());
    assert!(SecretMaterial::new("").is_err());
    assert!(SecretMaterial::new("a".repeat(65_537)).is_err());
}
