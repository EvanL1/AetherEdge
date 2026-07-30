//! Typed loopback client for the Automation service.

use anyhow::{Context, anyhow};
use serde::{Deserialize, Serialize};

use crate::trace_context::TraceParent;

const MAX_INSTANCE_RESPONSE_BYTES: usize = 1024 * 1024;

#[derive(Clone)]
pub struct AutomationClient {
    http: reqwest::Client,
}

impl AutomationClient {
    pub fn new(http: reqwest::Client) -> Self {
        Self { http }
    }

    pub async fn list_instances(
        &self,
        automation_url: &str,
    ) -> anyhow::Result<Vec<AutomationInstance>> {
        let url = format!(
            "{}/api/instances?page_size=100",
            automation_url.trim_end_matches('/')
        );
        let response = self
            .http
            .get(url)
            .send()
            .await
            .context("request Automation instance list")?;
        if !response.status().is_success() {
            return Err(anyhow!(
                "Automation returned status {} for the instance list",
                response.status()
            ));
        }
        decode_instance_list(&read_bounded_response(response).await?)
    }

    pub async fn dispatch_action(&self, request: AutomationAction<'_>) -> anyhow::Result<()> {
        let url = format!(
            "{}/api/instances/{}/action",
            request.automation_url.trim_end_matches('/'),
            request.instance_id
        );
        let body = AutomationActionBody {
            point_id: request.point_id,
            value: request.value,
            confirmed: true,
        };
        let mut http_request = self
            .http
            .post(url)
            .header(
                "authorization",
                format!("AetherService {}", request.control_token),
            )
            .json(&body);
        if let Some(message_id) = request.message_id {
            http_request = http_request.header("Idempotency-Key", message_id);
        }
        if let Some(traceparent) = request.traceparent {
            http_request = http_request.header("traceparent", traceparent.as_str());
        }
        let response = http_request
            .send()
            .await
            .context("request Automation instance action")?;
        if !response.status().is_success() {
            return Err(anyhow!(
                "Automation action returned HTTP {}",
                response.status()
            ));
        }
        Ok(())
    }
}

pub struct AutomationAction<'a> {
    pub automation_url: &'a str,
    pub control_token: &'a str,
    pub instance_id: u32,
    pub point_id: &'a str,
    pub value: f64,
    pub message_id: Option<&'a str>,
    pub traceparent: Option<&'a TraceParent>,
}

pub struct AutomationInstance {
    pub instance_id: u32,
    pub instance_name: String,
    pub product_name: String,
}

#[derive(Serialize)]
struct AutomationActionBody<'a> {
    point_id: &'a str,
    value: f64,
    confirmed: bool,
}

#[derive(Deserialize)]
struct AutomationInstanceListResponse {
    success: bool,
    data: AutomationInstancePage,
}

#[derive(Deserialize)]
struct AutomationInstancePage {
    total: u32,
    page: u32,
    page_size: u32,
    list: Vec<AutomationInstanceItem>,
}

#[derive(Deserialize)]
struct AutomationInstanceItem {
    instance_id: u32,
    instance_name: String,
    product_name: String,
}

fn decode_instance_list(body: &[u8]) -> anyhow::Result<Vec<AutomationInstance>> {
    let response: AutomationInstanceListResponse =
        serde_json::from_slice(body).context("decode typed Automation instance response")?;
    if !response.success {
        return Err(anyhow!("Automation instance response reported failure"));
    }
    let page = response.data;
    if page.page != 1 || page.page_size != 100 {
        return Err(anyhow!(
            "Automation instance response does not match the requested page"
        ));
    }
    if page.list.len() > page.page_size as usize || page.total < page.list.len() as u32 {
        return Err(anyhow!(
            "Automation instance response contains inconsistent pagination"
        ));
    }

    page.list
        .into_iter()
        .enumerate()
        .map(|(index, item)| {
            if item.instance_name.is_empty() || item.product_name.is_empty() {
                return Err(anyhow!(
                    "Automation instance response item {index} has an empty identity"
                ));
            }
            Ok(AutomationInstance {
                instance_id: item.instance_id,
                instance_name: item.instance_name,
                product_name: item.product_name,
            })
        })
        .collect()
}

async fn read_bounded_response(mut response: reqwest::Response) -> anyhow::Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_INSTANCE_RESPONSE_BYTES as u64)
    {
        return Err(anyhow!(
            "Automation instance response exceeds the size limit"
        ));
    }

    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("read Automation instance response")?
    {
        if body.len().saturating_add(chunk.len()) > MAX_INSTANCE_RESPONSE_BYTES {
            return Err(anyhow!(
                "Automation instance response exceeds the size limit"
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_list_decode_is_all_or_nothing() {
        let list = decode_instance_list(
            br#"{
                "success": true,
                "data": {
                    "total": 1,
                    "page": 1,
                    "page_size": 100,
                    "list": [{
                        "instance_id": 7,
                        "instance_name": "pump-7",
                        "product_name": "pump",
                        "parent_id": null,
                        "properties": {}
                    }]
                }
            }"#,
        )
        .expect("valid typed automation response");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].instance_id, 7);
        assert_eq!(list[0].instance_name, "pump-7");

        for malformed in [
            br#"{"success":true,"data":{"list":[{"instance_id":"7","instance_name":"pump-7","product_name":"pump"}]}}"#.as_slice(),
            br#"{"success":true,"data":{"list":[{"instance_id":7,"instance_name":"pump-7"}]}}"#.as_slice(),
            br#"{"success":true,"data":{}}"#.as_slice(),
            br#"{"data":{"list":[]}}"#.as_slice(),
            br#"{"success":false,"data":{"list":[]}}"#.as_slice(),
        ] {
            assert!(
                decode_instance_list(malformed).is_err(),
                "malformed Automation response must fail closed"
            );
        }
    }

    #[test]
    fn action_request_targets_the_automation_action_api() {
        let request = AutomationAction {
            automation_url: "http://localhost:6002/",
            control_token: "secret",
            instance_id: 12,
            point_id: "5",
            value: 42.5,
            message_id: None,
            traceparent: None,
        };
        let url = format!(
            "{}/api/instances/{}/action",
            request.automation_url.trim_end_matches('/'),
            request.instance_id
        );
        let body = serde_json::to_value(AutomationActionBody {
            point_id: request.point_id,
            value: request.value,
            confirmed: true,
        })
        .expect("serialize action request");

        assert_eq!(url, "http://localhost:6002/api/instances/12/action");
        assert_eq!(
            body,
            serde_json::json!({"point_id": "5", "value": 42.5, "confirmed": true})
        );
    }
}
