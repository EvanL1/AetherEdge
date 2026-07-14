/// Device identity resolution and MQTT topic generation.
use tracing::{info, warn};

/// Resolved device identity (product SN + device SN).
#[derive(Debug, Clone)]
pub struct DeviceIdentity {
    pub product_sn: String,
    pub device_sn: String,
}

impl DeviceIdentity {
    /// Resolve the device identity from the config.
    /// If `device_sn` is `"auto"`, try:
    ///   1. `/proc/device-tree/serial-number` (embedded Linux)
    ///   2. `DEVICE_SN` env variable
    ///   3. `hostname` command (Docker container ID)
    ///   4. Fallback: `"dev_001"`
    pub fn resolve(product_sn: &str, raw_device_sn: &str) -> Self {
        let device_sn = if raw_device_sn == "auto" {
            resolve_device_sn()
        } else {
            raw_device_sn.to_string()
        };

        info!(
            "Device identity: product_sn={} device_sn={}",
            product_sn, device_sn
        );
        Self {
            product_sn: product_sn.to_string(),
            device_sn,
        }
    }

    /// Build all MQTT topic strings for this device.
    pub fn topics(&self) -> Topics {
        Topics::new(&self.product_sn, &self.device_sn)
    }
}

fn resolve_device_sn() -> String {
    // 1. Hardware serial (embedded Linux)
    if let Ok(bytes) = std::fs::read("/proc/device-tree/serial-number") {
        // The file may have a null terminator
        let s = String::from_utf8_lossy(&bytes)
            .trim_matches('\0')
            .trim()
            .to_string();
        if !s.is_empty() {
            return s;
        }
    }

    // 2. Environment variable
    if let Ok(sn) = std::env::var("DEVICE_SN")
        && !sn.is_empty()
    {
        return sn;
    }

    // 3. Hostname (Docker container ID)
    if let Ok(h) = std::env::var("HOSTNAME")
        && !h.is_empty()
    {
        return h;
    }

    // 4. Fallback
    warn!("Could not determine device SN, using default 'dev_001'");
    "dev_001".to_string()
}

// ── MQTT Topics ───────────────────────────────────────────────────────────────

/// All MQTT topic strings for a given device.
#[derive(Debug, Clone)]
pub struct Topics {
    pub status: String,
    pub property: String,
    pub read: String,
    pub read_reply: String,
    pub write: String,
    pub write_reply: String,
    pub call_data: String,
    pub call_data_reply: String,
    pub call_alarm: String,
    pub call_alarm_reply: String,
    pub alarm: String,
    /// Incoming device-list sync request from the cloud.
    pub inst_sync: String,
    /// Reply topic for the device-list sync response.
    pub inst_sync_reply: String,
    /// Ops telemetry about the acquisition path (ADR-0016). Never point values.
    pub telemetry: String,
}

impl Topics {
    pub fn new(product_sn: &str, device_sn: &str) -> Self {
        let f = |template: &str| {
            template
                .replace("{productSN}", product_sn)
                .replace("{deviceSN}", device_sn)
        };
        Self {
            status: f("status/{productSN}/{deviceSN}"),
            property: f("property/{productSN}/{deviceSN}"),
            read: f("read/{productSN}/{deviceSN}"),
            read_reply: f("read-reply/{productSN}/{deviceSN}"),
            write: f("write/{productSN}/{deviceSN}"),
            write_reply: f("write-reply/{productSN}/{deviceSN}"),
            call_data: f("call-data/{productSN}/{deviceSN}"),
            call_data_reply: f("call-data-reply/{productSN}/{deviceSN}"),
            call_alarm: f("call-alarm/{productSN}/{deviceSN}"),
            call_alarm_reply: f("call-alarm-reply/{productSN}/{deviceSN}"),
            alarm: f("alarm/{productSN}/{deviceSN}"),
            inst_sync: f("inst-sync/{productSN}/{deviceSN}"),
            inst_sync_reply: f("inst-sync-reply/{productSN}/{deviceSN}"),
            telemetry: f("telemetry/{productSN}/{deviceSN}"),
        }
    }

    /// Return the list of topics this service should subscribe to.
    pub fn subscriptions(&self) -> Vec<(&str, rumqttc::QoS)> {
        vec![
            (self.read.as_str(), rumqttc::QoS::AtLeastOnce),
            (self.write.as_str(), rumqttc::QoS::AtLeastOnce),
            (self.call_data.as_str(), rumqttc::QoS::AtLeastOnce),
            (self.call_alarm.as_str(), rumqttc::QoS::AtLeastOnce),
            (self.inst_sync.as_str(), rumqttc::QoS::AtLeastOnce),
        ]
    }
}
