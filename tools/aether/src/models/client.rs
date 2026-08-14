//! HTTP client for model management

use anyhow::Result;
use reqwest::Client;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;

#[derive(Serialize)]
struct CreateInstanceRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    instance_id: Option<u32>,
    instance_name: &'a str,
    product_name: &'a str,
    properties: &'a HashMap<String, Value>,
    expected_revision: u64,
    confirmed: bool,
}

#[derive(Serialize)]
struct UpdateInstanceRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    instance_name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    properties: Option<&'a HashMap<String, Value>>,
    expected_revision: u64,
    confirmed: bool,
}

pub struct ModelClient {
    client: Client,
    base_url: String,
    access_token: Option<String>,
}

impl ModelClient {
    pub fn new(base_url: &str) -> Result<Self> {
        Ok(Self {
            client: Client::new(),
            base_url: base_url.to_string(),
            access_token: std::env::var("AETHER_ACCESS_TOKEN")
                .ok()
                .filter(|value| !value.trim().is_empty() && value.trim() == value),
        })
    }

    #[cfg(test)]
    pub(crate) fn with_access_token(base_url: &str, access_token: &str) -> Result<Self> {
        Ok(Self {
            client: Client::new(),
            base_url: base_url.to_string(),
            access_token: Some(access_token.to_string()),
        })
    }

    fn apply_auth(&self, request: reqwest::RequestBuilder) -> Result<reqwest::RequestBuilder> {
        match &self.access_token {
            Some(token) => {
                crate::transport_security::require_secure_bearer_transport(&self.base_url)?;
                Ok(request.bearer_auth(token))
            },
            None => Ok(request),
        }
    }

    // Product operations
    pub async fn list_products(&self) -> Result<Value> {
        let request = self.client.get(format!("{}/api/products", self.base_url));
        let response = self.apply_auth(request)?.send().await?;

        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            Err(anyhow::anyhow!(
                "Failed to get products: {}",
                response.status()
            ))
        }
    }

    pub async fn get_product(&self, name: &str) -> Result<Value> {
        let request = self
            .client
            .get(format!("{}/api/products/{}", self.base_url, name));
        let response = self.apply_auth(request)?.send().await?;

        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            Err(anyhow::anyhow!(
                "Failed to get product: {}",
                response.status()
            ))
        }
    }

    // Instance operations
    pub async fn list_instances(&self, product: Option<&str>) -> Result<Value> {
        let mut request = self.client.get(format!("{}/api/instances", self.base_url));
        if let Some(product_name) = product {
            request = request.query(&[("product_name", product_name)]);
        }
        let response = self.apply_auth(request)?.send().await?;

        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            Err(anyhow::anyhow!(
                "Failed to get instances: {}",
                response.status()
            ))
        }
    }

    pub async fn get_instance(&self, instance_id: u32) -> Result<Value> {
        let request = self
            .client
            .get(format!("{}/api/instances/{instance_id}", self.base_url));
        let response = self.apply_auth(request)?.send().await?;

        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            Err(anyhow::anyhow!(
                "Failed to get instance: {}",
                response.status()
            ))
        }
    }

    /// Read current instance values from automation's authoritative SHM view.
    pub async fn get_instance_data(
        &self,
        instance_id: u32,
        data_type: Option<&str>,
    ) -> Result<Value> {
        let mut request = self.client.get(format!(
            "{}/api/instances/{instance_id}/data",
            self.base_url
        ));
        if let Some(data_type) = data_type {
            request = request.query(&[("type", data_type)]);
        }
        let response = self.apply_auth(request)?.send().await?;
        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            Err(crate::output::parse_error_body("Failed to get instance data", response).await)
        }
    }

    pub async fn create_instance(
        &self,
        instance_id: Option<u32>,
        instance_name: &str,
        product_name: &str,
        properties: HashMap<String, Value>,
        expected_revision: u64,
        confirmed: bool,
    ) -> Result<()> {
        Self::validate_instance_mutation(confirmed, expected_revision)?;
        let body = CreateInstanceRequest {
            instance_id,
            instance_name,
            product_name,
            properties: &properties,
            expected_revision,
            confirmed,
        };
        let request = self
            .client
            .post(format!("{}/api/instances", self.base_url))
            .header("x-request-id", uuid::Uuid::new_v4().to_string())
            .header("x-aether-confirmed", "true")
            .json(&body);
        let response = self.apply_auth(request)?.send().await?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(crate::output::parse_error_body("Failed to create instance", response).await)
        }
    }

    pub async fn update_instance(
        &self,
        instance_id: u32,
        instance_name: Option<&str>,
        properties: Option<HashMap<String, Value>>,
        expected_revision: u64,
        confirmed: bool,
    ) -> Result<()> {
        Self::validate_instance_mutation(confirmed, expected_revision)?;
        if instance_name.is_none() && properties.is_none() {
            anyhow::bail!("instance update requires --instance-name or at least one --props value");
        }
        let body = UpdateInstanceRequest {
            instance_name,
            properties: properties.as_ref(),
            expected_revision,
            confirmed,
        };
        let request = self
            .client
            .put(format!("{}/api/instances/{instance_id}", self.base_url))
            .header("x-request-id", uuid::Uuid::new_v4().to_string())
            .header("x-aether-confirmed", "true")
            .json(&body);
        let response = self.apply_auth(request)?.send().await?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(crate::output::parse_error_body("Failed to update instance", response).await)
        }
    }

    pub async fn delete_instance(
        &self,
        instance_id: u32,
        expected_revision: u64,
        confirmed: bool,
    ) -> Result<()> {
        Self::validate_instance_mutation(confirmed, expected_revision)?;
        let request = self
            .client
            .delete(format!("{}/api/instances/{instance_id}", self.base_url))
            .query(&[
                ("expected_revision", expected_revision.to_string()),
                ("confirmed", confirmed.to_string()),
            ])
            .header("x-request-id", uuid::Uuid::new_v4().to_string())
            .header("x-aether-confirmed", "true");
        let response = self.apply_auth(request)?.send().await?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(crate::output::parse_error_body("Failed to delete instance", response).await)
        }
    }

    fn validate_instance_mutation(confirmed: bool, expected_revision: u64) -> Result<()> {
        if !confirmed {
            anyhow::bail!("instance mutation requires explicit confirmation (--confirmed)");
        }
        if expected_revision == 0 {
            anyhow::bail!("--expected-revision must be at least 1");
        }
        Ok(())
    }

    /// automation's `ActionRequest` takes a numeric point ID encoded as a string.
    ///
    /// A successful response means the local command plane accepted the
    /// request. It does not prove that the physical device executed it or
    /// reached the requested state.
    pub async fn execute_action(
        &self,
        instance_id: u32,
        point_id: &str,
        value: f64,
        confirmed: bool,
    ) -> Result<Value> {
        self.require_device_control_auth(confirmed)?;
        let body = serde_json::json!({
            "point_id": point_id,
            "value": value,
            "confirmed": confirmed
        });
        let request = self
            .client
            .post(format!(
                "{}/api/instances/{}/action",
                self.base_url, instance_id
            ))
            .header("x-request-id", uuid::Uuid::new_v4().to_string())
            .header("x-aether-confirmed", "true")
            .json(&body);
        let resp = self.apply_auth(request)?.send().await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(crate::output::parse_error_body("Failed to execute instance action", resp).await)
        }
    }

    fn require_device_control_auth(&self, confirmed: bool) -> Result<()> {
        if !confirmed {
            anyhow::bail!("device control requires explicit confirmation (--confirmed)");
        }
        crate::transport_security::require_secure_bearer_transport(&self.base_url)?;
        if self.access_token.is_none() {
            anyhow::bail!(
                "device control requires AETHER_ACCESS_TOKEN from an authenticated Admin or Engineer session"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::ModelClient;
    use wiremock::matchers::{body_json, header, header_exists, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn list_products_attaches_bearer_when_access_token_is_present() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/products"))
            .and(header("authorization", "Bearer signed-access-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .expect(1)
            .mount(&server)
            .await;

        let client = ModelClient::with_access_token(&server.uri(), "signed-access-token").unwrap();
        client.list_products().await.unwrap();
    }

    #[tokio::test]
    async fn list_products_stays_unauthenticated_without_access_token() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/products"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .expect(1)
            .mount(&server)
            .await;

        let client = ModelClient {
            client: reqwest::Client::new(),
            base_url: server.uri(),
            access_token: None,
        };
        client.list_products().await.unwrap();

        let requests = server.received_requests().await.unwrap();
        assert!(
            requests
                .iter()
                .all(|request| !request.headers.contains_key("authorization")),
            "tokenless reads must not carry an authorization header"
        );
    }

    #[tokio::test]
    async fn instance_writes_use_canonical_ids_bodies_queries_and_governance_headers() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/instances"))
            .and(header("x-aether-confirmed", "true"))
            .and(header_exists("x-request-id"))
            .and(body_json(serde_json::json!({
                "instance_id": 7,
                "instance_name": "pump-1",
                "product_name": "pump",
                "properties": {"capacity": 100, "enabled": true},
                "expected_revision": 3,
                "confirmed": true
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/api/instances/7"))
            .and(header("x-aether-confirmed", "true"))
            .and(header_exists("x-request-id"))
            .and(body_json(serde_json::json!({
                "instance_name": "pump-renamed",
                "properties": {"capacity": 120},
                "expected_revision": 4,
                "confirmed": true
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/api/instances/7"))
            .and(query_param("expected_revision", "5"))
            .and(query_param("confirmed", "true"))
            .and(header("x-aether-confirmed", "true"))
            .and(header_exists("x-request-id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let client = ModelClient {
            client: reqwest::Client::new(),
            base_url: server.uri(),
            access_token: None,
        };
        client
            .create_instance(
                Some(7),
                "pump-1",
                "pump",
                std::collections::HashMap::from([
                    ("capacity".to_string(), serde_json::json!(100)),
                    ("enabled".to_string(), serde_json::json!(true)),
                ]),
                3,
                true,
            )
            .await
            .unwrap();
        client
            .update_instance(
                7,
                Some("pump-renamed"),
                Some(std::collections::HashMap::from([(
                    "capacity".to_string(),
                    serde_json::json!(120),
                )])),
                4,
                true,
            )
            .await
            .unwrap();
        client.delete_instance(7, 5, true).await.unwrap();

        for request in server.received_requests().await.unwrap() {
            let request_id = request
                .headers
                .get("x-request-id")
                .expect("mutation request ID")
                .to_str()
                .expect("request ID text");
            let parsed = uuid::Uuid::parse_str(request_id).expect("UUID request ID");
            assert_eq!(parsed.to_string(), request_id);
        }
    }

    #[tokio::test]
    async fn instance_mutations_fail_before_http_without_governance_inputs() {
        let server = MockServer::start().await;
        let client = ModelClient {
            client: reqwest::Client::new(),
            base_url: server.uri(),
            access_token: None,
        };

        assert!(
            client
                .create_instance(None, "pump-1", "pump", HashMap::new(), 1, false)
                .await
                .unwrap_err()
                .to_string()
                .contains("explicit confirmation")
        );
        assert!(
            client
                .update_instance(7, Some("pump-2"), None, 0, true)
                .await
                .unwrap_err()
                .to_string()
                .contains("at least 1")
        );
        assert!(
            client
                .delete_instance(7, 1, false)
                .await
                .unwrap_err()
                .to_string()
                .contains("explicit confirmation")
        );
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn get_instance_uses_the_numeric_identity_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/instances/7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let client = ModelClient {
            client: reqwest::Client::new(),
            base_url: server.uri(),
            access_token: None,
        };
        client.get_instance(7).await.unwrap();
    }

    #[test]
    fn bearer_writes_reject_remote_plaintext_before_token_access() {
        let client = ModelClient {
            client: reqwest::Client::new(),
            base_url: "http://192.0.2.10:6002".to_string(),
            access_token: None,
        };

        let error = client
            .require_device_control_auth(true)
            .expect_err("remote plaintext must fail closed");
        assert!(error.to_string().contains("refusing to send"), "{error:#}");
    }

    #[tokio::test]
    async fn execute_action_posts_numeric_string_point_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/instances/3/action"))
            .and(header("authorization", "Bearer signed-access-token"))
            .and(header("x-aether-confirmed", "true"))
            .and(body_json(serde_json::json!({
                "point_id": "1",
                "value": 4500.0,
                "confirmed": true
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let client = ModelClient::with_access_token(&server.uri(), "signed-access-token").unwrap();
        client.execute_action(3, "1", 4500.0, true).await.unwrap();
    }

    #[tokio::test]
    async fn execute_action_rejects_unconfirmed_before_http() {
        let server = MockServer::start().await;
        let client = ModelClient::with_access_token(&server.uri(), "signed-access-token").unwrap();

        let error = client
            .execute_action(3, "1", 4500.0, false)
            .await
            .expect_err("unconfirmed device control must fail closed");

        assert!(error.to_string().contains("explicit confirmation"));
        assert!(
            server.received_requests().await.unwrap().is_empty(),
            "unconfirmed command must not make an HTTP request"
        );
    }

    #[tokio::test]
    async fn execute_action_fails_before_http_without_access_token() {
        let client = ModelClient {
            client: reqwest::Client::new(),
            base_url: "http://127.0.0.1:1".to_string(),
            access_token: None,
        };

        let error = client
            .execute_action(3, "1", 4500.0, true)
            .await
            .expect_err("missing token must fail closed");

        assert!(error.to_string().contains("AETHER_ACCESS_TOKEN"));
    }

    #[tokio::test]
    async fn execute_action_surfaces_automation_typed_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/instances/3/action"))
            .respond_with(ResponseTemplate::new(503).set_body_json(serde_json::json!({
                "success": false,
                "error": { "code": "CHANNEL_OFFLINE", "message": "channel 1001 offline" }
            })))
            .mount(&server)
            .await;

        let client = ModelClient::with_access_token(&server.uri(), "signed-access-token").unwrap();
        let err = client
            .execute_action(3, "1", 1.0, true)
            .await
            .unwrap_err()
            .to_string();

        assert!(err.contains("channel 1001 offline"), "{err}");
    }

    #[tokio::test]
    async fn instance_data_reads_the_shm_backed_api() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/instances/3/data"))
            .and(wiremock::matchers::query_param("type", "measurement"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "data": {
                    "measurements": {"101": {"value": 650.5, "timestamp_ms": 42}},
                    "actions": {}
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = ModelClient {
            client: reqwest::Client::new(),
            base_url: server.uri(),
            access_token: None,
        };
        let data = client
            .get_instance_data(3, Some("measurement"))
            .await
            .unwrap();
        assert_eq!(data["data"]["measurements"]["101"]["value"], 650.5);
        assert_eq!(data["data"]["actions"], serde_json::json!({}));
    }
}
