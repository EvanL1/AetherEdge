//! HTTP client for model management

use anyhow::Result;
use serde_json::Value;

crate::api_client::authenticated_api_client!(pub(crate) ModelClient);

impl ModelClient {
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
        let url = if let Some(p) = product {
            format!("{}/api/instances?product={}", self.base_url, p)
        } else {
            format!("{}/api/instances", self.base_url)
        };

        let response = self.apply_auth(self.client.get(url))?.send().await?;

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
        let request = self
            .client
            .get(format!("{}/api/instances/{}", self.base_url, name));
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
        self.require_governed_auth(
            confirmed,
            "device control requires explicit confirmation (--confirmed)",
            "device control requires AETHER_ACCESS_TOKEN from an authenticated Admin or Engineer session",
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::ModelClient;
    use wiremock::matchers::{body_json, header, method, path};
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

        let client = ModelClient::without_access_token(&server.uri());
        client.list_products().await.unwrap();

        let requests = server.received_requests().await.unwrap();
        assert!(
            requests
                .iter()
                .all(|request| !request.headers.contains_key("authorization")),
            "tokenless reads must not carry an authorization header"
        );
    }

    #[test]
    fn bearer_writes_reject_remote_plaintext_before_token_access() {
        let client = ModelClient::without_access_token("http://192.0.2.10:6002");

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
        let client = ModelClient::without_access_token("http://127.0.0.1:1");

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

        let client = ModelClient::without_access_token(&server.uri());
        let data = client
            .get_instance_data(3, Some("measurement"))
            .await
            .unwrap();
        assert_eq!(data["data"]["101"]["value"], 650.5);
    }
}
