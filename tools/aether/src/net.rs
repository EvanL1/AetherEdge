//! uplink management: MQTT connection/config and TLS certificates.

use anyhow::Result;
use clap::Subcommand;
use serde_json::Value;

use crate::output::{parse_error_body, print_value};

#[derive(Subcommand)]
pub enum NetCommands {
    /// MQTT connection and configuration
    #[command(subcommand)]
    Mqtt(MqttCommands),

    /// TLS certificate management
    #[command(subcommand)]
    Cert(CertCommands),
}

#[derive(Subcommand)]
pub enum MqttCommands {
    /// Show MQTT connection status
    #[command(about = "Show MQTT connection status")]
    Status,

    /// Show the current uplink configuration
    #[command(about = "Show the current uplink configuration")]
    Config,
}

#[derive(Subcommand)]
pub enum CertCommands {
    /// Show installed certificate info
    #[command(about = "Show installed TLS certificate info")]
    Info,
}

pub async fn handle_command(cmd: NetCommands, base_url: &str, json: bool) -> Result<()> {
    match cmd {
        NetCommands::Mqtt(command) => handle_mqtt_command(command, base_url, json).await,
        NetCommands::Cert(command) => handle_cert_command(command, base_url, json).await,
    }
}

async fn handle_mqtt_command(cmd: MqttCommands, base_url: &str, json: bool) -> Result<()> {
    let client = NetClient::new(base_url)?;
    match cmd {
        MqttCommands::Status => {
            let data = client.mqtt_status().await?;
            print_value(&data, json);
        },
        MqttCommands::Config => {
            let data = client.mqtt_config().await?;
            print_value(&data, json);
        },
    }
    Ok(())
}

async fn handle_cert_command(cmd: CertCommands, base_url: &str, json: bool) -> Result<()> {
    let client = NetClient::new(base_url)?;
    match cmd {
        CertCommands::Info => {
            let data = client.cert_info().await?;
            print_value(&data, json);
        },
    }
    Ok(())
}

crate::api_client::authenticated_api_client!(pub(crate) NetClient);

impl NetClient {
    pub(crate) async fn mqtt_status(&self) -> Result<Value> {
        let request = self.client.get(format!("{}/mqtt/status", self.base_url));
        let resp = self.apply_auth(request)?.send().await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(parse_error_body("Failed to get MQTT status", resp).await)
        }
    }

    pub(crate) async fn mqtt_config(&self) -> Result<Value> {
        let request = self.client.get(format!("{}/mqtt/config", self.base_url));
        let resp = self.apply_auth(request)?.send().await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(parse_error_body("Failed to get uplink config", resp).await)
        }
    }

    pub(crate) async fn cert_info(&self) -> Result<Value> {
        let request = self
            .client
            .get(format!("{}/certificate/info", self.base_url));
        let resp = self.apply_auth(request)?.send().await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(parse_error_body("Failed to get certificate info", resp).await)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::NetClient;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn reads_attach_bearer_token_when_present() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/mqtt/status"))
            .and(header("authorization", "Bearer signed-access-token"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "connected": true })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = NetClient::with_access_token(&server.uri(), "signed-access-token").unwrap();
        client.mqtt_status().await.unwrap();
    }

    #[tokio::test]
    async fn reads_without_token_carry_no_authorization_header() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/mqtt/status"))
            .and(|request: &wiremock::Request| !request.headers.contains_key("authorization"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "connected": true })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = NetClient::without_access_token(&server.uri());
        client.mqtt_status().await.unwrap();
    }

    #[tokio::test]
    async fn mqtt_status_gets_the_status_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/mqtt/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "connected": true, "broker": "tcp://1.2.3.4:1883" }),
            ))
            .expect(1)
            .mount(&server)
            .await;

        let client = NetClient::new(&server.uri()).unwrap();
        let v = client.mqtt_status().await.unwrap();

        assert_eq!(v["connected"], true);
    }

    #[tokio::test]
    async fn mqtt_status_surfaces_server_message_on_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/mqtt/status"))
            .respond_with(ResponseTemplate::new(500).set_body_json(
                serde_json::json!({ "success": false, "message": "broker unreachable" }),
            ))
            .mount(&server)
            .await;

        let client = NetClient::new(&server.uri()).unwrap();
        let err = client.mqtt_status().await.unwrap_err().to_string();

        assert!(err.contains("broker unreachable"), "{err}");
    }

    #[tokio::test]
    async fn mqtt_config_get_hits_config_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/mqtt/config"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "host": "h" })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = NetClient::new(&server.uri()).unwrap();
        let v = client.mqtt_config().await.unwrap();

        assert_eq!(v["host"], "h");
    }

    #[tokio::test]
    async fn mqtt_config_surfaces_server_message_on_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/mqtt/config"))
            .respond_with(ResponseTemplate::new(500).set_body_json(
                serde_json::json!({ "success": false, "message": "config store unreadable" }),
            ))
            .mount(&server)
            .await;

        let client = NetClient::new(&server.uri()).unwrap();
        let err = client.mqtt_config().await.unwrap_err().to_string();

        assert!(err.contains("config store unreadable"), "{err}");
    }

    #[tokio::test]
    async fn cert_info_gets_info_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/certificate/info"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "ca_cert": "present" })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = NetClient::new(&server.uri()).unwrap();
        let v = client.cert_info().await.unwrap();

        assert_eq!(v["ca_cert"], "present");
    }

    #[tokio::test]
    async fn cert_info_surfaces_server_message_on_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/certificate/info"))
            .respond_with(ResponseTemplate::new(500).set_body_json(
                serde_json::json!({ "success": false, "message": "cert store unreadable" }),
            ))
            .mount(&server)
            .await;

        let client = NetClient::new(&server.uri()).unwrap();
        let err = client.cert_info().await.unwrap_err().to_string();

        assert!(err.contains("cert store unreadable"), "{err}");
    }
}
