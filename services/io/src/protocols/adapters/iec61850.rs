//! IEC 61850 MMS protocol adapter.
//!
//! Provides polling-mode data collection from IEC 61850 IED servers via the
//! MMS (Manufacturing Message Specification) application protocol.
//!
//! # Protocol Stack
//!
//! ```text
//! TCP (port 102) → TPKT → COTP → ISO Session → ISO Presentation → ACSE → MMS
//! ```
//!
//! # YAML Configuration Example
//!
//! ```yaml
//! id: 10
//! name: IED1
//! protocol: iec61850
//! parameters:
//!   address: "192.168.1.10:102"
//!   connect_timeout_ms: 10000
//!   request_timeout_ms: 5000
//! points:
//!   - id: 1001
//!     point_type: Telemetry
//!     name: "AnIn1 magnitude"
//!     address: "simpleIOGenericIO/GGIO1$MX$AnIn1$mag$f"
//! ```

pub mod mms;
pub mod transport;

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use aether_config::io::MAX_CHANNEL_TIMING_MS;
use aether_core::PointType;
use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tracing::{debug, error, info, warn};

use crate::protocols::core::data::{DataBatch, DataPoint, Value};
use crate::protocols::core::error::{GatewayError, Result};
use crate::protocols::core::metadata::{
    DriverMetadata, HasMetadata, ParameterMetadata, ParameterType,
};
use crate::protocols::core::point::{PointConfig, TransformConfig};
use crate::protocols::core::traits::{ConnectionState, Diagnostics, PointFailure, PollResult};
use crate::protocols::runtime::ChannelRuntime;
use aether_domain::PointQuality;

use self::mms::{
    MmsValue, build_read_request, build_sbo_select_request, build_sbow_select_bool_request,
    build_write_bool_request, build_write_f32_request, build_write_simple_bool,
    parse_read_response, parse_report, parse_sbo_select_response, parse_write_response,
};
use self::transport::Framer;

// ── Timeout defaults ──────────────────────────────────────────────────────────

fn default_connect_timeout_ms() -> u64 {
    10_000
}
fn default_request_timeout_ms() -> u64 {
    5_000
}

// ── Parameters config (parsed from YAML/JSON `parameters` block) ──────────────

/// One Report Control Block (RCB) subscription to set up on connect.
///
/// # Configuration example (in the channel `parameters` JSON)
///
/// ```json
/// "reports": [
///   {
///     "rcb_ref": "simpleIOGenericIO/LLN0$BR$EventsBRCB",
///     "dataset_members": [
///       "simpleIOGenericIO/GGIO1$ST$SPCSO1$stVal",
///       "simpleIOGenericIO/GGIO1$ST$SPCSO2$stVal",
///       "simpleIOGenericIO/GGIO1$ST$SPCSO3$stVal",
///       "simpleIOGenericIO/GGIO1$ST$SPCSO4$stVal"
///     ]
///   }
/// ]
/// ```
///
/// `rcb_ref` format: `"LDInst/LNRef$FC$RCBName"`
/// where FC is `BR` (buffered) or `UR` (unbuffered).
///
/// `dataset_members`: ordered list of MMS paths (`"LD/LN$FC$DO$DA"`) matching
/// the server's dataset definition.  Points whose address matches a member are
/// **excluded from polling** and supplied exclusively via the report.
/// Leave empty to enable reports without excluding any poll points.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReportConfig {
    /// Full RCB object reference, e.g. `"simpleIOGenericIO/LLN0$BR$EventsBRCB"`.
    pub rcb_ref: String,

    /// Ordered MMS paths of the dataset elements, matching the server CID/SCL.
    #[serde(default)]
    pub dataset_members: Vec<String>,
}

/// IEC 61850 channel parameters (parsed from the `parameters:` block).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Iec61850ParamsConfig {
    /// Server address, e.g. `"192.168.1.10:102"`. Default port is 102.
    pub address: String,

    /// TCP connect timeout in milliseconds.
    #[serde(default = "default_connect_timeout_ms")]
    pub connect_timeout_ms: u64,

    /// Per-request timeout in milliseconds.
    #[serde(default = "default_request_timeout_ms")]
    pub request_timeout_ms: u64,

    /// Report Control Block subscriptions.  When configured, the channel
    /// subscribes to these RCBs on connect (writes `RptEna=TRUE`, `GI=TRUE`)
    /// and processes incoming unconfirmed report PDUs during each poll cycle.
    /// Points covered by `dataset_members` are excluded from polling.
    #[serde(default)]
    pub reports: Vec<ReportConfig>,
}

impl Iec61850ParamsConfig {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.address.trim().is_empty() {
            return Err(GatewayError::Config(
                "IEC 61850 address must be non-empty".to_owned(),
            ));
        }
        for (name, value) in [
            ("connect_timeout_ms", self.connect_timeout_ms),
            ("request_timeout_ms", self.request_timeout_ms),
        ] {
            if !(1..=MAX_CHANNEL_TIMING_MS).contains(&value) {
                return Err(GatewayError::Config(format!(
                    "IEC 61850 {name} must be between 1 and {MAX_CHANNEL_TIMING_MS}"
                )));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Iec61850PointMapping<'a> {
    #[serde(borrow)]
    address: &'a str,
    #[serde(default = "default_mapping_ctrl_model")]
    ctrl_model: u8,
}

fn default_mapping_ctrl_model() -> u8 {
    1
}

/// IEC 61850 MMS variable address owned by this adapter.
///
/// `domain` is the IED logical-device name and `item` is the MMS item ID
/// with its functional constraint embedded.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Iec61850Address {
    pub domain: String,
    pub item: String,
    #[serde(default = "default_mapping_ctrl_model")]
    pub ctrl_model: u8,
}

impl Iec61850Address {
    pub fn new(domain: impl Into<String>, item: impl Into<String>) -> Self {
        Self {
            domain: domain.into(),
            item: item.into(),
            ctrl_model: default_mapping_ctrl_model(),
        }
    }

    /// Parse `domain:item` MMS form or `domain/object.path` reference form.
    pub fn parse(address: &str) -> Result<Self> {
        if let Some((domain, item)) = address.split_once(':') {
            return Ok(Self::new(domain.trim(), item.trim()));
        }
        if let Some((domain, item)) = address.split_once('/') {
            return Ok(Self::new(domain.trim(), item.replace('.', "$")));
        }
        Err(GatewayError::Config(format!(
            "Invalid IEC 61850 address: '{address}'. Expected 'domain/item' or 'domain:item$...'"
        )))
    }
}

/// Decode and validate one persisted IEC 61850 point mapping without cloning
/// its JSON value.
///
/// IEC 61850 supports all four runtime point types; the point type determines
/// whether the channel polls the address or exposes it through a write path.
pub(crate) fn parse_point_mapping(
    mapping: &serde_json::Value,
    _point_type: PointType,
) -> Result<Iec61850Address> {
    let mapping = Iec61850PointMapping::deserialize(mapping).map_err(|error| {
        GatewayError::Config(format!("invalid IEC 61850 point mapping: {error}"))
    })?;
    if mapping.address.trim().is_empty() {
        return Err(GatewayError::Config(
            "IEC 61850 mapping address must be nonblank".to_owned(),
        ));
    }
    if !(1..=4).contains(&mapping.ctrl_model) {
        return Err(GatewayError::Config(
            "IEC 61850 ctrl_model must be in 1..=4".to_owned(),
        ));
    }

    let mut address = Iec61850Address::parse(mapping.address)?;
    if address.domain.is_empty() || address.item.is_empty() {
        return Err(GatewayError::Config(
            "IEC 61850 mapping address must contain nonblank domain and item components".to_owned(),
        ));
    }
    address.ctrl_model = mapping.ctrl_model;
    Ok(address)
}

// ── Channel ───────────────────────────────────────────────────────────────────

/// Telemetry / Signal point entry — polled on every cycle.
struct PointEntry {
    domain: String,
    item: String,
    id: u32,
    point_type: PointType,
    transform: TransformConfig,
}

/// Result of a best-effort RCB attribute write (used in `subscribe_reports`).
enum RcbWriteResult {
    Ok,
    /// MMS data-access error 10 = OBJECT_NONE_EXISTENT.
    NotFound,
    Err(GatewayError),
}

/// Control / Adjustment point entry — written on demand, never polled.
struct WriteEntry {
    domain: String,
    item: String,
    /// IEC 61850 control model: 1=direct, 2=SBO-normal, 3=direct-enhanced, 4=SBOw-enhanced
    ctrl_model: u8,
}

/// IEC 61850 MMS polling channel.
pub struct Iec61850Channel {
    name: String,

    address: String,
    connect_timeout: Duration,
    request_timeout: Duration,

    /// Active TCP + framing layer. `None` when disconnected.
    framer: Option<Framer>,

    state: ConnectionState,

    /// Monotonic invoke-ID counter (1–255).
    invoke_id: u8,

    /// Telemetry and Signal points — polled in order.
    points: Vec<PointEntry>,

    /// Control points indexed by point_id — written via `write_control`.
    ctrl_points: HashMap<u32, WriteEntry>,

    /// Adjustment points indexed by point_id — written via `write_adjustment`.
    adj_points: HashMap<u32, WriteEntry>,

    /// RCB subscriptions configured in channel parameters.
    report_configs: Vec<ReportConfig>,

    /// Reverse map: full MMS path (`"domain/item"`) → (point_id, type, transform).
    /// Used to decode report data values to DataPoints.
    path_to_point: HashMap<String, (u32, PointType, TransformConfig)>,

    /// Point IDs that are covered by an active report subscription.
    /// These are **skipped** during the polling phase of `poll_once`.
    report_skip_set: HashSet<u32>,
}

impl Iec61850Channel {
    pub fn new(
        name: impl Into<String>,
        params: &Iec61850ParamsConfig,
        points: Vec<PointConfig<Iec61850Address>>,
    ) -> Self {
        let mut poll_points: Vec<PointEntry> = Vec::new();
        let mut ctrl_points: HashMap<u32, WriteEntry> = HashMap::new();
        let mut adj_points: HashMap<u32, WriteEntry> = HashMap::new();

        for point in points {
            let PointConfig {
                id,
                point_type,
                address,
                transform,
            } = point;
            match point_type {
                PointType::Control => {
                    ctrl_points.insert(
                        id,
                        WriteEntry {
                            domain: address.domain,
                            item: address.item,
                            ctrl_model: address.ctrl_model,
                        },
                    );
                },
                PointType::Adjustment => {
                    adj_points.insert(
                        id,
                        WriteEntry {
                            domain: address.domain,
                            item: address.item,
                            ctrl_model: address.ctrl_model,
                        },
                    );
                },
                _ => {
                    poll_points.push(PointEntry {
                        domain: address.domain,
                        item: address.item,
                        id,
                        point_type,
                        transform,
                    });
                },
            }
        }

        // Build reverse map: full path → (point_id, type, transform)
        let mut path_to_point: HashMap<String, (u32, PointType, TransformConfig)> = HashMap::new();
        for pe in &poll_points {
            let path = format!("{}/{}", pe.domain, pe.item);
            path_to_point.insert(path, (pe.id, pe.point_type, pe.transform));
        }

        // Build the skip-set: poll points whose path appears in any report dataset.
        let mut report_skip_set: HashSet<u32> = HashSet::new();
        for rc in &params.reports {
            for member_path in &rc.dataset_members {
                if let Some((pt_id, _, _)) = path_to_point.get(member_path) {
                    report_skip_set.insert(*pt_id);
                }
            }
        }

        Self {
            name: name.into(),
            address: params.address.clone(),
            connect_timeout: Duration::from_millis(params.connect_timeout_ms),
            request_timeout: Duration::from_millis(params.request_timeout_ms),
            framer: None,
            state: ConnectionState::Disconnected,
            invoke_id: 1,
            points: poll_points,
            ctrl_points,
            adj_points,
            report_configs: params.reports.clone(),
            path_to_point,
            report_skip_set,
        }
    }

    fn next_invoke_id(&mut self) -> u8 {
        let id = self.invoke_id;
        self.invoke_id = if self.invoke_id == 255 {
            1
        } else {
            self.invoke_id + 1
        };
        id
    }

    /// Derive the MMS path for the `ctlModel` CF attribute from a control item path.
    ///
    /// Example: `"GGIO1$CO$SPCSO2$Oper$ctlVal"` → `"GGIO1$CF$SPCSO2$ctlModel"`
    fn derive_ctlmodel_item(item: &str) -> Option<String> {
        let (ln, rest) = item.split_once("$CO$")?;
        let do_name = rest.split('$').next()?;
        Some(format!("{}$CF${}$ctlModel", ln, do_name))
    }

    /// After a successful MMS handshake, read `ctlModel` (FC=CF) for every
    /// control and adjustment point and cache it in `WriteEntry.ctrl_model`.
    ///
    /// Failures are non-fatal:
    /// - MMS data-access error → keep configured value (default 1 = direct)
    /// - IO / timeout error    → stop detection early, keep remaining defaults
    async fn detect_ctrl_models(&mut self) {
        // Phase 1: collect work list (avoid borrow conflict during async reads)
        let mut work: Vec<(bool, u32, String, String)> = Vec::new();
        for (&id, e) in &self.ctrl_points {
            if let Some(ci) = Self::derive_ctlmodel_item(&e.item) {
                work.push((true, id, e.domain.clone(), ci));
            }
        }
        for (&id, e) in &self.adj_points {
            if let Some(ci) = Self::derive_ctlmodel_item(&e.item) {
                work.push((false, id, e.domain.clone(), ci));
            }
        }

        if work.is_empty() {
            return;
        }

        // Phase 2: read ctlModel for each point
        for (is_ctrl, id, domain, ctlmodel_item) in work {
            let invoke_id = self.next_invoke_id();
            match self.read_variable(invoke_id, &domain, &ctlmodel_item).await {
                Ok(MmsValue::Integer(n)) => {
                    let cm = n as u8;
                    info!(
                        "IEC 61850 [{}] pt{} ctlModel={} (auto-detected)",
                        self.name, id, cm
                    );
                    if is_ctrl {
                        if let Some(e) = self.ctrl_points.get_mut(&id) {
                            e.ctrl_model = cm;
                        }
                    } else if let Some(e) = self.adj_points.get_mut(&id) {
                        e.ctrl_model = cm;
                    }
                },
                Ok(MmsValue::Unsigned(n)) => {
                    let cm = n as u8;
                    info!(
                        "IEC 61850 [{}] pt{} ctlModel={} (auto-detected)",
                        self.name, id, cm
                    );
                    if is_ctrl {
                        if let Some(e) = self.ctrl_points.get_mut(&id) {
                            e.ctrl_model = cm;
                        }
                    } else if let Some(e) = self.adj_points.get_mut(&id) {
                        e.ctrl_model = cm;
                    }
                },
                Ok(MmsValue::Failure(code)) => {
                    // Variable not accessible (access denied, not found, etc.)
                    // Keep configured value (default 1 = direct).
                    debug!(
                        "IEC 61850 [{}] pt{} ctlModel not readable (err {}), using default",
                        self.name, id, code
                    );
                },
                Err(e) => {
                    // IO / timeout — framer may be in unknown state; stop early.
                    warn!(
                        "IEC 61850 [{}] ctlModel detection stopped at pt{}: {}",
                        self.name, id, e
                    );
                    break;
                },
                _ => {},
            }
        }
    }

    async fn try_connect(&mut self) -> Result<()> {
        self.state = ConnectionState::Connecting;

        let connect_timeout_ms = self.connect_timeout.as_millis() as u64;
        let stream = timeout(self.connect_timeout, TcpStream::connect(&self.address))
            .await
            .map_err(|_| GatewayError::ConnectionTimeout(connect_timeout_ms))?
            .map_err(GatewayError::Io)?;

        stream.set_nodelay(true).ok();
        let mut framer = Framer::new(stream);

        timeout(self.connect_timeout, framer.handshake_cotp())
            .await
            .map_err(|_| GatewayError::ConnectionTimeout(connect_timeout_ms))?
            .map_err(|e| GatewayError::Protocol(format!("IEC 61850: COTP handshake: {}", e)))?;

        timeout(self.connect_timeout, framer.handshake_mms())
            .await
            .map_err(|_| GatewayError::ConnectionTimeout(connect_timeout_ms))?
            .map_err(|e| GatewayError::Protocol(format!("IEC 61850: MMS initiate: {}", e)))?;

        self.framer = Some(framer);
        self.state = ConnectionState::Connected;
        info!("IEC 61850 [{}] connected to {}", self.name, self.address);
        Ok(())
    }

    async fn read_variable(&mut self, invoke_id: u8, domain: &str, item: &str) -> Result<MmsValue> {
        let framer = self
            .framer
            .as_mut()
            .ok_or_else(|| GatewayError::Protocol("IEC 61850: not connected".into()))?;

        let req = build_read_request(invoke_id, domain, item);

        timeout(self.request_timeout, framer.send_mms(&req))
            .await
            .map_err(|_| GatewayError::WriteTimeout)??;

        let resp = timeout(self.request_timeout, framer.recv_mms())
            .await
            .map_err(|_| GatewayError::ReadTimeout)??;

        let (_, value) = parse_read_response(&resp)
            .map_err(|e| GatewayError::Protocol(format!("IEC 61850: parse response: {}", e)))?;

        Ok(value)
    }

    /// Send any pre-built MMS request PDU and return the raw response bytes.
    async fn do_request_raw(&mut self, req: Vec<u8>) -> Result<Vec<u8>> {
        let framer = self
            .framer
            .as_mut()
            .ok_or_else(|| GatewayError::Protocol("IEC 61850: not connected".into()))?;

        timeout(self.request_timeout, framer.send_mms(&req))
            .await
            .map_err(|_| GatewayError::WriteTimeout)??;

        timeout(self.request_timeout, framer.recv_mms())
            .await
            .map_err(|_| GatewayError::ReadTimeout)?
    }

    /// Send a pre-built Write-Request PDU and wait for a Write-Response.
    async fn do_write(&mut self, req: Vec<u8>) -> Result<()> {
        tracing::debug!(bytes = ?&req[..req.len().min(40)], "write request raw");
        let resp = self.do_request_raw(req).await?;
        parse_write_response(&resp)
            .map(|_| ())
            .map_err(|e| GatewayError::Protocol(format!("IEC 61850: write response: {}", e)))
    }

    fn go_disconnected(&mut self) {
        self.framer = None;
        self.state = ConnectionState::Disconnected;
    }

    // ── Report subscription ───────────────────────────────────────────────────

    /// Parse `"LD/LN$FC$RCB"` → `(domain="LD", base_item="LN$FC$RCB")`.
    fn split_rcb_ref(rcb_ref: &str) -> Option<(String, String)> {
        let slash = rcb_ref.find('/')?;
        Some((rcb_ref[..slash].to_owned(), rcb_ref[slash + 1..].to_owned()))
    }

    /// After a successful MMS handshake, subscribe to all configured RCBs:
    /// writes `RptEna = TRUE` (enables reporting) and `GI = TRUE` (triggers a
    /// general-interrogation snapshot).
    ///
    /// **Auto-index probing**: IEC 61850 IEDs built with libiec61850 append a
    /// numeric suffix to each RCB name (`EventsBRCB` → `EventsBRCB01`).
    /// If the configured name is not found (MMS data-access error 10 =
    /// `OBJECT_NONE_EXISTENT`), the code automatically retries with `"01"`
    /// appended, so users can keep the plain CID name in their config.
    ///
    /// Failures are non-fatal: a warning is logged and the remaining RCBs are
    /// still attempted.
    async fn subscribe_reports(&mut self) {
        if self.report_configs.is_empty() {
            return;
        }

        // Collect work: (domain, base_item) for each RCB.
        let work: Vec<(String, String)> = self
            .report_configs
            .iter()
            .filter_map(|rc| Self::split_rcb_ref(&rc.rcb_ref))
            .collect();

        for (domain, base_item) in work {
            // Resolve the actual MMS item name: try configured name first,
            // then fall back to "name01" (libiec61850 indexed-RCB convention).
            let resolved_base = match self
                .try_rcb_write_bool(&domain, &base_item, "RptEna", true)
                .await
            {
                RcbWriteResult::Ok => {
                    info!(
                        "IEC 61850 [{}] RCB {}/{} RptEna=TRUE ok",
                        self.name, domain, base_item
                    );
                    base_item.clone()
                },
                RcbWriteResult::NotFound => {
                    // Try with "01" suffix (libiec61850 default index).
                    let indexed = format!("{}01", base_item);
                    match self
                        .try_rcb_write_bool(&domain, &indexed, "RptEna", true)
                        .await
                    {
                        RcbWriteResult::Ok => {
                            info!(
                                "IEC 61850 [{}] RCB {}/{} (→{}) RptEna=TRUE ok",
                                self.name, domain, base_item, indexed
                            );
                            indexed
                        },
                        RcbWriteResult::NotFound => {
                            warn!(
                                "IEC 61850 [{}] RCB {}/{} not found (tried {} and {}), skipping",
                                self.name, domain, base_item, base_item, indexed
                            );
                            continue;
                        },
                        RcbWriteResult::Err(e) => {
                            warn!(
                                "IEC 61850 [{}] RCB {}/{} RptEna failed: {}",
                                self.name, domain, indexed, e
                            );
                            continue;
                        },
                    }
                },
                RcbWriteResult::Err(e) => {
                    warn!(
                        "IEC 61850 [{}] RCB {}/{} RptEna failed: {}",
                        self.name, domain, base_item, e
                    );
                    continue;
                },
            };

            // Write GI = TRUE (trigger an immediate full-dataset snapshot report)
            let invoke_id = self.next_invoke_id();
            let gi_item = format!("{}$GI", resolved_base);
            let req = build_write_simple_bool(invoke_id, &domain, &gi_item, true);
            match self.do_write(req).await {
                Ok(()) => {
                    info!(
                        "IEC 61850 [{}] RCB {}/{} GI=TRUE ok (snapshot requested)",
                        self.name, domain, resolved_base
                    );
                },
                Err(e) => {
                    warn!(
                        "IEC 61850 [{}] RCB {}/{} GI failed: {}",
                        self.name, domain, resolved_base, e
                    );
                },
            }
        }
    }

    /// Attempt to write a boolean to `domain / base_item $ attr`.
    /// Returns [`RcbWriteResult::NotFound`] specifically when the server
    /// responds with MMS data-access error 10 (OBJECT_NONE_EXISTENT) so the
    /// caller can probe an alternative name.
    async fn try_rcb_write_bool(
        &mut self,
        domain: &str,
        base_item: &str,
        attr: &str,
        value: bool,
    ) -> RcbWriteResult {
        let invoke_id = self.next_invoke_id();
        let item = format!("{}${}", base_item, attr);
        let req = build_write_simple_bool(invoke_id, domain, &item, value);
        match self.do_write(req).await {
            Ok(()) => RcbWriteResult::Ok,
            Err(e) => {
                // Check for "data-access error code 10" (OBJECT_NONE_EXISTENT).
                if e.to_string().contains("error code 10") {
                    RcbWriteResult::NotFound
                } else {
                    RcbWriteResult::Err(e)
                }
            },
        }
    }

    // ── Report processing ─────────────────────────────────────────────────────

    /// Convert a list of raw unconfirmed-PDU bytes into `DataPoint`s.
    ///
    /// Each PDU is parsed as an IEC 61850 InformationReport.  Dataset element
    /// values are matched to point IDs via `self.path_to_point` using the
    /// configured `dataset_members` ordering.
    fn process_report_pdus(&self, pdus: Vec<Vec<u8>>) -> PollResult {
        let mut batch = DataBatch::new();
        let mut failures = Vec::new();
        for pdu in &pdus {
            let Some(report) = parse_report(pdu) else {
                debug!(
                    "IEC 61850 [{}] failed to parse unconfirmed PDU ({} bytes)",
                    self.name,
                    pdu.len()
                );
                continue;
            };

            // Convert BinaryTime6 timestamp to chrono::DateTime<Utc>.
            let source_ts: Option<DateTime<Utc>> = report
                .timestamp_ms
                .and_then(|ms| Utc.timestamp_millis_opt(ms as i64).single());

            // Match each included element to a configured dataset_members entry.
            for rc in &self.report_configs {
                if rc.dataset_members.is_empty() {
                    continue;
                }
                for (i, &elem_idx) in report.element_indices.iter().enumerate() {
                    let Some(member_path) = rc.dataset_members.get(elem_idx) else {
                        continue;
                    };
                    let Some((pt_id, pt_type, transform)) = self.path_to_point.get(member_path)
                    else {
                        continue;
                    };
                    let Some(mms_val) = report.values.get(i) else {
                        continue;
                    };
                    collect_mms_point(
                        &mut batch,
                        &mut failures,
                        *pt_id,
                        *pt_type,
                        mms_val,
                        transform,
                        source_ts,
                    );
                }
            }
        }
        if failures.is_empty() {
            PollResult::success(batch)
        } else {
            PollResult::partial(batch, failures)
        }
    }
}

// ── Metadata ──────────────────────────────────────────────────────────────────

impl HasMetadata for Iec61850Channel {
    fn metadata() -> DriverMetadata {
        DriverMetadata {
            name: "iec61850",
            display_name: "IEC 61850 MMS",
            description: "IEC 61850 MMS client over TCP/ISO transport",
            is_recommended: true,
            example_config: serde_json::json!({
                "address": "192.168.1.10:102",
                "connect_timeout_ms": 10_000,
                "request_timeout_ms": 5_000,
                "reports": []
            }),
            parameters: vec![
                ParameterMetadata::required(
                    "address",
                    "Server Address",
                    "IEC 61850 server host and TCP port",
                    ParameterType::String,
                ),
                ParameterMetadata::optional(
                    "connect_timeout_ms",
                    "Connect Timeout (ms)",
                    "TCP connection timeout",
                    ParameterType::Integer,
                    serde_json::json!(10_000),
                ),
                ParameterMetadata::optional(
                    "request_timeout_ms",
                    "Request Timeout (ms)",
                    "Per-request timeout",
                    ParameterType::Integer,
                    serde_json::json!(5_000),
                ),
                ParameterMetadata::optional(
                    "reports",
                    "Report Subscriptions",
                    "Report control block subscriptions",
                    ParameterType::Array,
                    serde_json::json!([]),
                ),
            ],
        }
    }
}

// ── ChannelRuntime ────────────────────────────────────────────────────────────

#[async_trait]
impl ChannelRuntime for Iec61850Channel {
    async fn connect(&mut self) -> Result<()> {
        self.try_connect().await.map_err(|e| {
            self.state = ConnectionState::Error;
            error!("IEC 61850 [{}] connect failed: {}", self.name, e);
            e
        })?;
        self.detect_ctrl_models().await;
        self.subscribe_reports().await;
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        self.go_disconnected();
        info!("IEC 61850 [{}] disconnected", self.name);
        Ok(())
    }

    async fn poll_once(&mut self) -> PollResult {
        if self.state != ConnectionState::Connected {
            return PollResult::default();
        }

        // ── Phase 1: collect pending reports (arrived since last cycle) ────────
        // drain_socket() reads buffered + incoming 0xA3 PDUs with a short
        // timeout so we capture reports that arrived while the channel was idle.
        let report_pdus = if !self.report_configs.is_empty() {
            if let Some(framer) = self.framer.as_mut() {
                framer.drain_socket().await
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        let mut batch = DataBatch::with_capacity(self.points.len());
        let mut failures: Vec<PointFailure> = Vec::new();
        let point_count = self.points.len();

        // ── Phase 2: poll data for points NOT covered by an active report ─────
        for i in 0..point_count {
            let (domain, item, point_id, point_type, transform) = {
                let p = &self.points[i];
                (
                    p.domain.clone(),
                    p.item.clone(),
                    p.id,
                    p.point_type,
                    p.transform,
                )
            };

            // Skip points covered by a subscribed report dataset.
            if self.report_skip_set.contains(&point_id) {
                continue;
            }

            let invoke_id = self.next_invoke_id();

            match self.read_variable(invoke_id, &domain, &item).await {
                Ok(mms_val) => collect_mms_point(
                    &mut batch,
                    &mut failures,
                    point_id,
                    point_type,
                    &mms_val,
                    &transform,
                    None,
                ),
                Err(GatewayError::ReadTimeout) | Err(GatewayError::WriteTimeout) => {
                    warn!(
                        "IEC 61850 [{}] read {}/{} timeout (skipping)",
                        self.name, domain, item
                    );
                    failures.push(PointFailure::with_error(
                        point_id,
                        "read timeout".to_string(),
                    ));
                    self.go_disconnected();
                    break;
                },
                Err(e) => {
                    warn!(
                        "IEC 61850 [{}] read {}/{} IO error: {}",
                        self.name, domain, item, e
                    );
                    self.go_disconnected();
                    failures.push(PointFailure::with_error(point_id, e.to_string()));
                    break;
                },
            }
        }

        // ── Phase 3: also collect any reports buffered during the poll phase ──
        // recv_mms() silently buffers 0xA3 PDUs encountered while waiting for
        // confirmed responses; drain them now.
        let mid_pdus = if !self.report_configs.is_empty() {
            if let Some(framer) = self.framer.as_mut() {
                framer.take_pending_reports()
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        // ── Phase 4: convert all report PDUs to DataPoints ────────────────────
        let all_report_pdus: Vec<Vec<u8>> = report_pdus.into_iter().chain(mid_pdus).collect();

        if !all_report_pdus.is_empty() {
            let report_result = self.process_report_pdus(all_report_pdus);
            batch.merge(report_result.data);
            failures.extend(report_result.failures);
        }

        if failures.is_empty() {
            PollResult::success(batch)
        } else {
            PollResult::partial(batch, failures)
        }
    }

    async fn write_control(&mut self, commands: &[(u32, f64)]) -> Result<usize> {
        if self.state != ConnectionState::Connected {
            return Ok(0);
        }

        let mut ok = 0;
        for &(point_id, value) in commands {
            let entry = match self.ctrl_points.get(&point_id) {
                Some(e) => (e.domain.clone(), e.item.clone(), e.ctrl_model),
                None => {
                    warn!(
                        "IEC 61850 [{}] control point {} not configured",
                        self.name, point_id
                    );
                    continue;
                },
            };
            let (domain, item, ctrl_model) = entry;
            let bool_val = value != 0.0;

            // ── Select step (SBO models only) ──────────────────────────────
            let selected = match ctrl_model {
                2 => {
                    // SBO-Normal: READ $SBO, server returns non-empty VisibleString on success
                    let invoke_id = self.next_invoke_id();
                    let req = build_sbo_select_request(invoke_id, &domain, &item);
                    match self.do_request_raw(req).await {
                        Ok(resp) => match parse_sbo_select_response(&resp) {
                            Ok(true) => {
                                info!(
                                    "IEC 61850 [{}] SBO select+ pt{} ({}/{})",
                                    self.name, point_id, domain, item
                                );
                                true
                            },
                            Ok(false) => {
                                warn!(
                                    "IEC 61850 [{}] SBO select- pt{} ({}/{}) (refused by server)",
                                    self.name, point_id, domain, item
                                );
                                false
                            },
                            Err(e) => {
                                warn!(
                                    "IEC 61850 [{}] SBO select pt{} err: {}",
                                    self.name, point_id, e
                                );
                                false
                            },
                        },
                        Err(e) => {
                            warn!(
                                "IEC 61850 [{}] SBO select pt{} IO error: {}",
                                self.name, point_id, e
                            );
                            self.go_disconnected();
                            break;
                        },
                    }
                },
                4 => {
                    // SBOw-Enhanced: WRITE $SBOw with the same Oper structure
                    let invoke_id = self.next_invoke_id();
                    let req = build_sbow_select_bool_request(invoke_id, &domain, &item, bool_val);
                    match self.do_write(req).await {
                        Ok(()) => {
                            info!(
                                "IEC 61850 [{}] SBOw select+ pt{} ({}/{})",
                                self.name, point_id, domain, item
                            );
                            true
                        },
                        Err(e) => {
                            warn!(
                                "IEC 61850 [{}] SBOw select pt{} err: {}",
                                self.name, point_id, e
                            );
                            self.go_disconnected();
                            break;
                        },
                    }
                },
                _ => true, // ctlModel=1,3: direct control, no select needed
            };

            if !selected {
                continue;
            }

            // ── Operate step ───────────────────────────────────────────────
            let invoke_id = self.next_invoke_id();
            let req = build_write_bool_request(invoke_id, &domain, &item, bool_val);

            match self.do_write(req).await {
                Ok(()) => {
                    info!(
                        "IEC 61850 [{}] control pt{} ({}/{}) = {} ok",
                        self.name, point_id, domain, item, bool_val
                    );
                    ok += 1;
                },
                Err(e) => {
                    warn!(
                        "IEC 61850 [{}] control pt{} ({}/{}) err: {}",
                        self.name, point_id, domain, item, e
                    );
                    self.go_disconnected();
                    break;
                },
            }
        }
        Ok(ok)
    }

    async fn write_adjustment(&mut self, adjustments: &[(u32, f64)]) -> Result<usize> {
        if self.state != ConnectionState::Connected {
            return Ok(0);
        }

        let mut ok = 0;
        for &(point_id, value) in adjustments {
            let entry = match self.adj_points.get(&point_id) {
                Some(e) => (e.domain.clone(), e.item.clone()),
                None => {
                    warn!(
                        "IEC 61850 [{}] adjustment point {} not configured",
                        self.name, point_id
                    );
                    continue;
                },
            };
            let (domain, item) = entry;
            let invoke_id = self.next_invoke_id();
            let req = build_write_f32_request(invoke_id, &domain, &item, value as f32);

            match self.do_write(req).await {
                Ok(()) => {
                    info!(
                        "IEC 61850 [{}] adjustment pt{} ({}/{}) = {} ok",
                        self.name, point_id, domain, item, value
                    );
                    ok += 1;
                },
                Err(e) => {
                    warn!(
                        "IEC 61850 [{}] adjustment pt{} ({}/{}) err: {}",
                        self.name, point_id, domain, item, e
                    );
                    self.go_disconnected();
                    break;
                },
            }
        }
        Ok(ok)
    }

    async fn diagnostics(&self) -> Result<Diagnostics> {
        let mut d = Diagnostics::new("iec61850");
        d.connection_state = self.state;
        Ok(d)
    }

    fn connection_state(&self) -> ConnectionState {
        self.state
    }
}

// ── Value helpers ─────────────────────────────────────────────────────────────

fn collect_mms_point(
    batch: &mut DataBatch,
    failures: &mut Vec<PointFailure>,
    point_id: u32,
    point_type: PointType,
    mms: &MmsValue,
    transform: &TransformConfig,
    source_timestamp: Option<DateTime<Utc>>,
) {
    match convert_mms_point(point_id, point_type, mms, transform, source_timestamp) {
        Ok(point) => batch.add(point),
        Err(failure) => failures.push(failure),
    }
}

fn convert_mms_point(
    point_id: u32,
    point_type: PointType,
    mms: &MmsValue,
    transform: &TransformConfig,
    source_timestamp: Option<DateTime<Utc>>,
) -> std::result::Result<DataPoint, PointFailure> {
    if let MmsValue::Failure(code) = mms {
        return Err(PointFailure::with_error(
            point_id,
            format!("MMS data-access error {code}"),
        ));
    }

    let raw_value = mms_to_value(mms).map_err(|error| PointFailure::new(point_id, error))?;
    let value = apply_transform(raw_value, transform)
        .map_err(|error| PointFailure::new(point_id, error))?;

    Ok(DataPoint {
        id: point_id,
        point_type,
        value,
        quality: PointQuality::Good,
        timestamp: Utc::now(),
        source_timestamp,
    })
}

fn mms_to_value(mms: &MmsValue) -> std::result::Result<Value, &'static str> {
    match mms {
        MmsValue::Float32(value) if value.is_finite() => Ok(Value::Float(*value as f64)),
        MmsValue::Float64(value) if value.is_finite() => Ok(Value::Float(*value)),
        MmsValue::Float32(_) | MmsValue::Float64(_) => {
            Err("IEC 61850 floating-point value is not finite")
        },
        MmsValue::Integer(value) => Ok(Value::Integer(*value)),
        MmsValue::Unsigned(value) => i64::try_from(*value)
            .map(Value::Integer)
            .map_err(|_| "IEC 61850 unsigned value exceeds the live-state integer range"),
        MmsValue::Boolean(value) => Ok(Value::Bool(*value)),
        MmsValue::VisibleString(_) => {
            Err("IEC 61850 string value is not supported for live acquisition")
        },
        MmsValue::BitString { .. } | MmsValue::UtcTime(_) | MmsValue::OctetString(_) => {
            Err("IEC 61850 value type is not supported for live acquisition")
        },
        MmsValue::Failure(_) => Err("IEC 61850 MMS data-access failure"),
    }
}

fn apply_transform(
    value: Value,
    transform: &TransformConfig,
) -> std::result::Result<Value, &'static str> {
    match value {
        Value::Float(value) => finite_transformed_value(transform.apply(value)),
        Value::Integer(value) => finite_transformed_value(transform.apply(value as f64)),
        Value::Bool(value) => Ok(Value::Bool(transform.apply_bool(value))),
    }
}

fn finite_transformed_value(value: f64) -> std::result::Result<Value, &'static str> {
    if value.is_finite() {
        Ok(Value::Float(value))
    } else {
        Err("IEC 61850 transformed value is not finite")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_mapping_codec_is_strict_and_accepts_all_runtime_point_types() {
        let mapping = serde_json::json!({
            "address": "simpleIOGenericIO/GGIO1$MX$AnIn1$mag$f",
            "ctrl_model": 4
        });

        for point_type in [
            PointType::Telemetry,
            PointType::Signal,
            PointType::Control,
            PointType::Adjustment,
        ] {
            let address = parse_point_mapping(&mapping, point_type).unwrap();
            assert_eq!(address.domain, "simpleIOGenericIO");
            assert_eq!(address.item, "GGIO1$MX$AnIn1$mag$f");
            assert_eq!(address.ctrl_model, 4);
        }

        assert!(
            parse_point_mapping(
                &serde_json::json!({
                    "address": "simpleIOGenericIO/GGIO1$MX$AnIn1$mag$f",
                    "ignored": true
                }),
                PointType::Telemetry,
            )
            .is_err()
        );
        assert!(
            parse_point_mapping(
                &serde_json::json!({
                    "address": "simpleIOGenericIO/GGIO1$MX$AnIn1$mag$f",
                    "ctrl_model": 5
                }),
                PointType::Control,
            )
            .is_err()
        );
    }

    #[test]
    fn mixed_mms_values_keep_numeric_points_and_report_non_numeric_values() {
        let mut batch = DataBatch::new();
        let mut failures = Vec::new();
        let transform = TransformConfig::default();

        for (point_id, value) in [
            (1, MmsValue::Float32(12.5)),
            (2, MmsValue::Boolean(true)),
            (3, MmsValue::VisibleString("not numeric".to_owned())),
            (4, MmsValue::OctetString(vec![1, 2, 3])),
            (5, MmsValue::Float64(f64::NAN)),
        ] {
            collect_mms_point(
                &mut batch,
                &mut failures,
                point_id,
                PointType::Telemetry,
                &value,
                &transform,
                None,
            );
        }

        assert_eq!(batch.len(), 2);
        assert_eq!(
            batch.iter().map(|point| point.id).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(
            failures
                .iter()
                .map(|failure| failure.point_id)
                .collect::<Vec<_>>(),
            vec![3, 4, 5]
        );
    }
}
