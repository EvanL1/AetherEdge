//! uplink management: MQTT connection/config and TLS certificates.

use std::path::Path;

use anyhow::Result;
use clap::Subcommand;
use reqwest::Client;
use serde_json::Value;

use crate::output::{parse_error_body, print_action, print_value};

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

    /// Replace the uplink configuration from a JSON file
    #[command(about = "Replace uplink configuration from a JSON file (full NetConfig object)")]
    ConfigSet {
        /// Path to a JSON file containing the complete NetConfig object
        #[arg(long)]
        file: String,
    },

    /// Reconnect the MQTT client
    #[command(about = "Reconnect the MQTT client")]
    Reconnect,

    /// Disconnect the MQTT client
    #[command(about = "Disconnect the MQTT client")]
    Disconnect,
}

#[derive(Subcommand)]
pub enum CertCommands {
    /// Show installed certificate info
    #[command(about = "Show installed TLS certificate info")]
    Info,

    /// Delete a certificate by type
    #[command(about = "Delete a TLS certificate by type")]
    Delete {
        /// Certificate role
        #[arg(value_parser = ["ca_cert", "client_cert", "client_key"])]
        cert_type: String,
    },

    /// Upload a certificate file
    #[command(about = "Upload a TLS certificate file (max 1 MB)")]
    Upload {
        /// Certificate role
        #[arg(long = "type", value_parser = ["ca_cert", "client_cert", "client_key"])]
        cert_type: String,

        /// Path to the certificate file (.pem .crt .key .cer .p12 .pfx)
        file: String,
    },
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
        // uplink always populates `message` on success for these three endpoints
        // (mirroring alarm's action endpoints — see alarms.rs), so
        // `action_message`'s fallback below is a defensive backstop that never
        // actually gets printed. `print_action`'s signature requires one anyway,
        // and it's the right default if that ever stops being true.
        MqttCommands::ConfigSet { file } => {
            let raw = std::fs::read_to_string(&file)
                .map_err(|e| anyhow::anyhow!("Failed to read config file {file}: {e}"))?;
            let cfg: Value = serde_json::from_str(&raw)
                .map_err(|e| anyhow::anyhow!("Invalid JSON in config file {file}: {e}"))?;
            let data = client.mqtt_config_set(&cfg).await?;
            print_action(&data, "uplink config updated", json);
        },
        MqttCommands::Reconnect => {
            let data = client.mqtt_reconnect().await?;
            print_action(&data, "MQTT reconnect requested", json);
        },
        MqttCommands::Disconnect => {
            let data = client.mqtt_disconnect().await?;
            print_action(&data, "MQTT disconnected", json);
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
        CertCommands::Delete { cert_type } => {
            // uplink returns HTTP 200 with genuinely different messages for the
            // same request: "Deleted successfully" for a real delete vs "File
            // does not exist, nothing to delete" for a no-op. Echoing the
            // server's message (rather than always printing the fallback) is
            // what lets the operator tell those two outcomes apart.
            let data = client.cert_delete(&cert_type).await?;
            print_action(&data, &format!("Certificate {cert_type} deleted"), json);
        },
        CertCommands::Upload { cert_type, file } => {
            let data = client.cert_upload(&cert_type, Path::new(&file)).await?;
            print_action(&data, &format!("Certificate {cert_type} uploaded"), json);
        },
    }
    Ok(())
}

pub(crate) struct NetClient {
    client: Client,
    base_url: String,
}

impl NetClient {
    pub(crate) fn new(base_url: &str) -> Result<Self> {
        Ok(Self {
            client: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
        })
    }

    pub(crate) async fn mqtt_status(&self) -> Result<Value> {
        let resp = self
            .client
            .get(format!("{}/netApi/mqtt/status", self.base_url))
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(parse_error_body("Failed to get MQTT status", resp).await)
        }
    }

    pub(crate) async fn mqtt_config(&self) -> Result<Value> {
        let resp = self
            .client
            .get(format!("{}/netApi/mqtt/config", self.base_url))
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(parse_error_body("Failed to get uplink config", resp).await)
        }
    }

    pub(crate) async fn mqtt_config_set(&self, cfg: &Value) -> Result<Value> {
        let resp = self
            .client
            .post(format!("{}/netApi/mqtt/config", self.base_url))
            .json(cfg)
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(parse_error_body("Failed to update uplink config", resp).await)
        }
    }

    pub(crate) async fn mqtt_reconnect(&self) -> Result<Value> {
        self.post_empty("/netApi/mqtt/reconnect", "Failed to reconnect MQTT")
            .await
    }

    pub(crate) async fn mqtt_disconnect(&self) -> Result<Value> {
        self.post_empty("/netApi/mqtt/disconnect", "Failed to disconnect MQTT")
            .await
    }

    async fn post_empty(&self, path: &str, context: &str) -> Result<Value> {
        let resp = self
            .client
            .post(format!("{}{}", self.base_url, path))
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(parse_error_body(context, resp).await)
        }
    }

    pub(crate) async fn cert_info(&self) -> Result<Value> {
        let resp = self
            .client
            .get(format!("{}/netApi/certificate/info", self.base_url))
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(parse_error_body("Failed to get certificate info", resp).await)
        }
    }

    pub(crate) async fn cert_delete(&self, cert_type: &str) -> Result<Value> {
        let resp = self
            .client
            .delete(format!(
                "{}/netApi/certificate/{}",
                self.base_url, cert_type
            ))
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(parse_error_body("Failed to delete certificate", resp).await)
        }
    }

    pub(crate) async fn cert_upload(&self, cert_type: &str, file: &Path) -> Result<Value> {
        // Read first: a missing file must fail before we open a connection.
        let bytes = std::fs::read(file)
            .map_err(|e| anyhow::anyhow!("Failed to read certificate {}: {e}", file.display()))?;

        // Reachable only for a non-UTF8 filename on Linux. uplink validates the
        // *extension* of this name against an allowlist (.pem/.crt/.key/…), so
        // this bare fallback has no extension and the server rejects it with
        // "Unsupported file format ''" — surfaced verbatim by parse_error_body.
        let filename = file
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("certificate")
            .to_string();

        let part = reqwest::multipart::Part::bytes(bytes).file_name(filename);
        let form = reqwest::multipart::Form::new()
            .text("cert_type", cert_type.to_string())
            .part("file", part);

        let resp = self
            .client
            .post(format!("{}/netApi/certificate/upload", self.base_url))
            .multipart(form)
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(resp.json().await?)
        } else {
            Err(parse_error_body("Failed to upload certificate", resp).await)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::NetClient;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn mqtt_status_gets_the_status_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/netApi/mqtt/status"))
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
            .and(path("/netApi/mqtt/status"))
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
            .and(path("/netApi/mqtt/config"))
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
            .and(path("/netApi/mqtt/config"))
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
    async fn mqtt_config_set_posts_the_body_verbatim() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/netApi/mqtt/config"))
            .and(wiremock::matchers::body_json(
                serde_json::json!({ "host": "new", "port": 1883 }),
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "success": true })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = NetClient::new(&server.uri()).unwrap();
        let cfg = serde_json::json!({ "host": "new", "port": 1883 });
        client.mqtt_config_set(&cfg).await.unwrap();
    }

    #[tokio::test]
    async fn mqtt_config_set_surfaces_server_message_on_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/netApi/mqtt/config"))
            .respond_with(ResponseTemplate::new(400).set_body_json(
                serde_json::json!({ "success": false, "message": "invalid broker_port" }),
            ))
            .mount(&server)
            .await;

        let client = NetClient::new(&server.uri()).unwrap();
        let err = client
            .mqtt_config_set(&serde_json::json!({ "host": "new" }))
            .await
            .unwrap_err()
            .to_string();

        assert!(err.contains("invalid broker_port"), "{err}");
    }

    #[tokio::test]
    async fn mqtt_reconnect_posts_its_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/netApi/mqtt/reconnect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let client = NetClient::new(&server.uri()).unwrap();
        client.mqtt_reconnect().await.unwrap();
    }

    #[tokio::test]
    async fn mqtt_reconnect_surfaces_server_message_on_error() {
        // Only /reconnect is mounted, and the assertion pins both the context
        // string ("Failed to reconnect MQTT") and the server's message — that's
        // what proves this hit its own path, not mqtt_disconnect's.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/netApi/mqtt/reconnect"))
            .respond_with(ResponseTemplate::new(500).set_body_json(
                serde_json::json!({ "success": false, "message": "broker unreachable" }),
            ))
            .mount(&server)
            .await;

        let client = NetClient::new(&server.uri()).unwrap();
        let err = client.mqtt_reconnect().await.unwrap_err().to_string();

        assert!(err.contains("Failed to reconnect MQTT"), "{err}");
        assert!(err.contains("broker unreachable"), "{err}");
    }

    #[tokio::test]
    async fn mqtt_disconnect_posts_its_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/netApi/mqtt/disconnect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let client = NetClient::new(&server.uri()).unwrap();
        client.mqtt_disconnect().await.unwrap();
    }

    #[tokio::test]
    async fn mqtt_disconnect_surfaces_server_message_on_error() {
        // Only /disconnect is mounted; the context string ("Failed to disconnect
        // MQTT") and message together prove this is disconnect's own path, not
        // mqtt_reconnect's.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/netApi/mqtt/disconnect"))
            .respond_with(ResponseTemplate::new(500).set_body_json(
                serde_json::json!({ "success": false, "message": "client not connected" }),
            ))
            .mount(&server)
            .await;

        let client = NetClient::new(&server.uri()).unwrap();
        let err = client.mqtt_disconnect().await.unwrap_err().to_string();

        assert!(err.contains("Failed to disconnect MQTT"), "{err}");
        assert!(err.contains("client not connected"), "{err}");
    }

    #[tokio::test]
    async fn cert_info_gets_info_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/netApi/certificate/info"))
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
            .and(path("/netApi/certificate/info"))
            .respond_with(ResponseTemplate::new(500).set_body_json(
                serde_json::json!({ "success": false, "message": "cert store unreadable" }),
            ))
            .mount(&server)
            .await;

        let client = NetClient::new(&server.uri()).unwrap();
        let err = client.cert_info().await.unwrap_err().to_string();

        assert!(err.contains("cert store unreadable"), "{err}");
    }

    #[tokio::test]
    async fn cert_delete_uses_delete_with_cert_type_in_path() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/netApi/certificate/client_key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let client = NetClient::new(&server.uri()).unwrap();
        client.cert_delete("client_key").await.unwrap();
    }

    #[tokio::test]
    async fn cert_delete_surfaces_server_message_on_error() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/netApi/certificate/client_key"))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "success": false,
                "message": "certificate client_key not found"
            })))
            .mount(&server)
            .await;

        let client = NetClient::new(&server.uri()).unwrap();
        let err = client
            .cert_delete("client_key")
            .await
            .unwrap_err()
            .to_string();

        assert!(err.contains("certificate client_key not found"), "{err}");
    }

    #[tokio::test]
    async fn cert_upload_posts_multipart_with_cert_type_and_file_fields() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/netApi/certificate/upload"))
            .and(wiremock::matchers::header_regex(
                "content-type",
                "^multipart/form-data; boundary=",
            ))
            .and(wiremock::matchers::body_string_contains(
                "name=\"cert_type\"",
            ))
            // Not just the field's *name* — its *value* must reach the wire.
            // "client_key" is distinct from the filename "ca.pem", so this
            // can't accidentally pass via a substring match on the filename.
            .and(wiremock::matchers::body_string_contains("client_key"))
            .and(wiremock::matchers::body_string_contains("name=\"file\""))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "success": true })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("ca.pem");
        std::fs::write(&cert, b"-----BEGIN CERTIFICATE-----\n").unwrap();

        let client = NetClient::new(&server.uri()).unwrap();
        client.cert_upload("client_key", &cert).await.unwrap();
    }

    #[tokio::test]
    async fn cert_upload_reports_missing_file_without_touching_the_network() {
        let client = NetClient::new("http://127.0.0.1:1").unwrap();
        let err = client
            .cert_upload("ca_cert", std::path::Path::new("/nonexistent/ca.pem"))
            .await
            .unwrap_err()
            .to_string();

        assert!(err.contains("/nonexistent/ca.pem"), "{err}");
    }

    #[tokio::test]
    async fn cert_upload_surfaces_server_message_on_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/netApi/certificate/upload"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "success": false,
                "message": "Unsupported file format 'txt'"
            })))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("ca.pem");
        std::fs::write(&cert, b"-----BEGIN CERTIFICATE-----\n").unwrap();

        let client = NetClient::new(&server.uri()).unwrap();
        let err = client
            .cert_upload("ca_cert", &cert)
            .await
            .unwrap_err()
            .to_string();

        assert!(err.contains("Unsupported file format"), "{err}");
    }
}
