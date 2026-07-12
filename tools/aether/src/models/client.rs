//! HTTP client for model management

use anyhow::Result;
use reqwest::Client;
use serde_json::Value;
use std::collections::HashMap;

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

    // Product operations
    pub async fn list_products(&self) -> Result<Value> {
        let response = self
            .client
            .get(format!("{}/api/products", self.base_url))
            .send()
            .await?;

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
        let response = self
            .client
            .get(format!("{}/api/products/{}", self.base_url, name))
            .send()
            .await?;

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
        let url = if let Some(p) = product {
            format!("{}/api/instances?product={}", self.base_url, p)
        } else {
            format!("{}/api/instances", self.base_url)
        };

        let response = self.client.get(url).send().await?;

        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            Err(anyhow::anyhow!(
                "Failed to get instances: {}",
                response.status()
            ))
        }
    }

    pub async fn get_instance(&self, name: &str) -> Result<Value> {
        let response = self
            .client
            .get(format!("{}/api/instances/{}", self.base_url, name))
            .send()
            .await?;

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
        let response = request.send().await?;
        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            Err(crate::output::parse_error_body("Failed to get instance data", response).await)
        }
    }

    #[allow(clippy::disallowed_methods)] // json! macro internally uses unwrap (safe for known valid JSON)
    pub async fn create_instance(
        &self,
        product: &str,
        name: &str,
        props: HashMap<String, String>,
    ) -> Result<()> {
        let response = self
            .client
            .post(format!("{}/api/instances", self.base_url))
            .json(&serde_json::json!({
                "product": product,
                "name": name,
                "properties": props
            }))
            .send()
            .await?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "Failed to create instance: {}",
                response.status()
            ))
        }
    }

    #[allow(clippy::disallowed_methods)] // json! macro internally uses unwrap (safe for known valid JSON)
    pub async fn update_instance(&self, name: &str, props: HashMap<String, String>) -> Result<()> {
        let response = self
            .client
            .put(format!("{}/api/instances/{}", self.base_url, name))
            .json(&serde_json::json!({
                "properties": props
            }))
            .send()
            .await?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "Failed to update instance: {}",
                response.status()
            ))
        }
    }

    pub async fn delete_instance(&self, name: &str) -> Result<()> {
        let response = self
            .client
            .delete(format!("{}/api/instances/{}", self.base_url, name))
            .send()
            .await?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "Failed to delete instance: {}",
                response.status()
            ))
        }
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
        let access_token = self.device_control_token(confirmed)?;
        let body = serde_json::json!({
            "point_id": point_id,
            "value": value,
            "confirmed": confirmed
        });
        let resp = self
            .client
            .post(format!(
                "{}/api/instances/{}/action",
                self.base_url, instance_id
            ))
            .bearer_auth(access_token)
            .header("x-request-id", uuid::Uuid::new_v4().to_string())
            .json(&body)
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(crate::output::parse_error_body("Failed to execute instance action", resp).await)
        }
    }

    fn device_control_token(&self, confirmed: bool) -> Result<&str> {
        if !confirmed {
            anyhow::bail!("device control requires explicit confirmation (--confirmed)");
        }
        crate::transport_security::require_secure_bearer_transport(&self.base_url)?;
        self.access_token.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "device control requires AETHER_ACCESS_TOKEN from an authenticated Admin or Engineer session"
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::ModelClient;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn bearer_writes_reject_remote_plaintext_before_token_access() {
        let client = ModelClient {
            client: reqwest::Client::new(),
            base_url: "http://192.0.2.10:6002".to_string(),
            access_token: None,
        };

        let error = client
            .device_control_token(true)
            .expect_err("remote plaintext must fail closed");
        assert!(error.to_string().contains("refusing to send"), "{error:#}");
    }

    #[tokio::test]
    async fn execute_action_posts_numeric_string_point_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/instances/3/action"))
            .and(header("authorization", "Bearer signed-access-token"))
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
                "data": {"101": {"value": 650.5, "timestamp_ms": 42}}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = ModelClient::new(&server.uri()).unwrap();
        let data = client
            .get_instance_data(3, Some("measurement"))
            .await
            .unwrap();
        assert_eq!(data["data"]["101"]["value"], 650.5);
    }
}
