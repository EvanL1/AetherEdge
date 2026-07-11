//! MMS (Manufacturing Message Specification) PDU encoder and decoder.
//!
//! Implements a minimal subset of MMS (ISO 9506) required for IEC 61850 data collection:
//!
//! - **Read-Request**: read a single domain-specific variable
//! - **Read-Response**: parse the returned `AccessResult`
//! - **Write-Request**: write a single variable (for control)
//! - **Initiate-Request/Response**: already handled as static bytes in `transport.rs`
//!
//! # Wire format (MMS Read)
//!
//! ```text
//! A0 [len]          confirmedRequestPDU
//!   02 01 [id]      invokeID
//!   A4 [len]        Read-Request [4]
//!     A1 [len]      variableAccessSpec (list scope)
//!       A0 [len]    VariableSpecification: name [0]
//!         30 [len]  SEQUENCE (ObjectName wrapper)
//!           A0 [len]  outer name [0]
//!             A1 [len]  domain-specific [1]
//!               1A [dlen] [domain]   VisibleString: domain id
//!               1A [ilen] [item]     VisibleString: item id
//! ```
//!
//! The nesting matches captured libiec61850 traffic; no C code has been copied.

use crate::protocols::core::error::{GatewayError, Result};

use super::transport::{parse_ber_len, parse_tlv, push_ber_len};

// ── MMS tag constants ─────────────────────────────────────────────────────────

const TAG_CONFIRMED_REQ: u8 = 0xA0;
const TAG_CONFIRMED_RESP: u8 = 0xA1;
const TAG_REJECT_PDU: u8 = 0xA4; // [4] = both Read tag and Reject (context-dependent)
const TAG_INTEGER: u8 = 0x02;
const TAG_VISIBLE_STR: u8 = 0x1A;
const TAG_SEQUENCE: u8 = 0x30;

// AccessResult Data type tags (context-specific primitive)
const DATA_BOOLEAN: u8 = 0x83;
const DATA_BIT_STRING: u8 = 0x84;
const DATA_INTEGER: u8 = 0x85;
const DATA_UNSIGNED: u8 = 0x86;
const DATA_FLOAT: u8 = 0x87;
const DATA_OCTET_STRING: u8 = 0x89;
const DATA_VISIBLE_STRING: u8 = 0x8A;
const DATA_UTC_TIME: u8 = 0x91;
const DATA_MMS_STRING: u8 = 0x90;
// libiec61850 uses mms-extended.asn where Data CHOICE is offset by 1:
//   array [1], structure [2], boolean [3], ... (NOT the standard [0],[1],[2])
// So MMS structures on the wire must be tagged 0xA2, not 0xA1.
const DATA_STRUCTURE: u8 = 0xA2;

// ── Public API ────────────────────────────────────────────────────────────────

/// A value decoded from an MMS AccessResult.
#[derive(Debug, Clone)]
pub enum MmsValue {
    /// IEEE 754 single precision (5-byte MMS float: 1 exponent + 4 data)
    Float32(f32),
    /// IEEE 754 double precision (9-byte MMS float: 1 exponent + 8 data)
    Float64(f64),
    Boolean(bool),
    Integer(i64),
    Unsigned(u64),
    VisibleString(String),
    BitString {
        bytes: Vec<u8>,
        unused_bits: u8,
    },
    UtcTime([u8; 8]),
    OctetString(Vec<u8>),
    /// MMS data-access-error code
    Failure(u8),
}

impl MmsValue {
    /// Convert to `f64` for use in DataPoint.
    pub fn to_f64(&self) -> Option<f64> {
        match self {
            Self::Float32(v) => Some(*v as f64),
            Self::Float64(v) => Some(*v),
            Self::Integer(v) => Some(*v as f64),
            Self::Unsigned(v) => Some(*v as f64),
            Self::Boolean(v) => Some(if *v { 1.0 } else { 0.0 }),
            _ => None,
        }
    }

    pub fn to_bool(&self) -> Option<bool> {
        match self {
            Self::Boolean(v) => Some(*v),
            Self::Integer(v) => Some(*v != 0),
            Self::Unsigned(v) => Some(*v != 0),
            _ => None,
        }
    }

    pub fn to_string_val(&self) -> Option<String> {
        match self {
            Self::VisibleString(s) => Some(s.clone()),
            _ => None,
        }
    }

    pub fn is_ok(&self) -> bool {
        !matches!(self, Self::Failure(_))
    }
}

// ── Encoder ───────────────────────────────────────────────────────────────────

/// Build an MMS Read-Request PDU for a single domain-specific variable.
///
/// `invoke_id`: 1–255, used to match request with response.
/// `domain`: MMS domain (IED logical device name), e.g. `"simpleIOGenericIO"`.
/// `item`: MMS item ID with functional constraint, e.g. `"GGIO1$MX$AnIn1$mag$f"`.
pub fn build_read_request(invoke_id: u8, domain: &str, item: &str) -> Vec<u8> {
    let d = domain.as_bytes();
    let i = item.as_bytes();

    // Sizes are computed bottom-up. Each variable = CONTENT length of that TLV
    // (i.e. what goes into push_ber_len for that tag). Total TLV size = 2 + content.
    let a1_domain_content = 2 + d.len() + 2 + i.len(); // 1A dl [d] 1A il [i]
    let a0_outer_content = 2 + a1_domain_content; // A1 TLV total
    let seq_content = 2 + a0_outer_content; // A0 TLV total
    let a0_varspec_content = 2 + seq_content; // 30 TLV total
    let a1_list_content = 2 + a0_varspec_content; // A0 TLV total
    let a4_read_content = 2 + a1_list_content; // A1 TLV total
    // outer A0 content = invokeID(3) + A4 TLV total(2 + a4_read_content)
    let req_inner = 3 + 2 + a4_read_content;

    let mut buf = Vec::with_capacity(2 + req_inner);

    // confirmedRequestPDU A0 [req_inner]
    buf.push(TAG_CONFIRMED_REQ);
    push_ber_len(&mut buf, req_inner);

    // invokeID: 02 01 [id]
    buf.extend_from_slice(&[TAG_INTEGER, 0x01, invoke_id]);

    // Read [4]: A4, content = a4_read_content
    buf.push(TAG_REJECT_PDU); // 0xA4 = [4] IMPLICIT = Read
    push_ber_len(&mut buf, a4_read_content);

    // A1, content = a1_list_content
    buf.push(0xA1);
    push_ber_len(&mut buf, a1_list_content);

    // A0, content = a0_varspec_content
    buf.push(0xA0);
    push_ber_len(&mut buf, a0_varspec_content);

    // 30 (ObjectName SEQUENCE), content = seq_content
    buf.push(TAG_SEQUENCE);
    push_ber_len(&mut buf, seq_content);

    // A0, content = a0_outer_content
    buf.push(0xA0);
    push_ber_len(&mut buf, a0_outer_content);

    // A1 (domain-specific), content = a1_domain_content
    buf.push(0xA1);
    push_ber_len(&mut buf, a1_domain_content);

    // 1A [domain_len] [domain]
    buf.push(TAG_VISIBLE_STR);
    buf.push(d.len() as u8);
    buf.extend_from_slice(d);

    // 1A [item_len] [item]
    buf.push(TAG_VISIBLE_STR);
    buf.push(i.len() as u8);
    buf.extend_from_slice(i);

    buf
}

// ── Decoder ───────────────────────────────────────────────────────────────────

/// Decode an MMS Read-Response and return the first `AccessResult` value.
///
/// Returns `Err` if the PDU is malformed.  Returns `Ok(MmsValue::Failure(n))` if
/// the server returned a data-access error.
pub fn parse_read_response(pdu: &[u8]) -> Result<(u8, MmsValue)> {
    // A1 [len]  confirmedResponsePDU — parse into its CONTENT, not the bytes after it
    let (_, pdu_content) = parse_tlv(pdu, TAG_CONFIRMED_RESP)
        .ok_or_else(|| GatewayError::Protocol("MMS: expected confirmedResponsePDU (A1)".into()))?;

    // 02 01 [id]  invokeID
    let (rest, id_bytes) = parse_tlv(pdu_content, TAG_INTEGER)
        .ok_or_else(|| GatewayError::Protocol("MMS: missing invokeID".into()))?;
    let invoke_id = id_bytes.first().copied().unwrap_or(0);

    // A4 [len]  Read-Response [4]
    let (_, read_resp) = parse_tlv(rest, TAG_REJECT_PDU)
        .ok_or_else(|| GatewayError::Protocol("MMS: expected Read-Response (A4)".into()))?;

    // A1 [len]  listOfAccessResults
    let (_, access_results) = parse_tlv(read_resp, 0xA1)
        .ok_or_else(|| GatewayError::Protocol("MMS: expected listOfAccessResults (A1)".into()))?;

    // First AccessResult: either A1 (success wrapper) or direct primitive tag
    let value = parse_access_result(access_results)?;

    Ok((invoke_id, value))
}

/// Parse a single AccessResult from the start of `buf`.
fn parse_access_result(buf: &[u8]) -> Result<MmsValue> {
    if buf.is_empty() {
        return Err(GatewayError::Protocol("MMS: empty AccessResult".into()));
    }

    let tag = buf[0];

    // Failure: [0] = A0 with inner = [0] failure code
    // In MMS, AccessResult CHOICE:
    //   failure [0] IMPLICIT DataAccessError   → tag = 0x80 (primitive)
    //   success [1] IMPLICIT Data              → tag = 0xA1 (constructed)
    // But many servers return data directly (primitive tags), so handle both.

    match tag {
        0x80 => {
            // failure (DataAccessError)
            let code = buf.get(2).copied().unwrap_or(0);
            Ok(MmsValue::Failure(code))
        },
        0xA1 => {
            // success: A1 contains a Data CHOICE value
            let (_, data_buf) = parse_tlv(buf, 0xA1).ok_or_else(|| {
                GatewayError::Protocol("MMS: malformed AccessResult success".into())
            })?;
            parse_data(data_buf)
        },
        // Direct data tags (some servers embed data without the A1 wrapper)
        DATA_BOOLEAN | DATA_BIT_STRING | DATA_INTEGER | DATA_UNSIGNED | DATA_FLOAT
        | DATA_OCTET_STRING | DATA_VISIBLE_STRING | DATA_UTC_TIME | DATA_MMS_STRING => {
            parse_data(buf)
        },
        other => Err(GatewayError::Protocol(format!(
            "MMS: unknown AccessResult tag 0x{:02X}",
            other
        ))),
    }
}

/// Parse a single MMS `Data` value (the inner content of AccessResult success).
fn parse_data(buf: &[u8]) -> Result<MmsValue> {
    if buf.is_empty() {
        return Err(GatewayError::Protocol("MMS: empty Data".into()));
    }

    let tag = buf[0];
    let (len, hdr) = parse_ber_len(&buf[1..])
        .ok_or_else(|| GatewayError::Protocol("MMS: BER length error".into()))?;
    let val = &buf[1 + hdr..1 + hdr + len];

    match tag {
        DATA_BOOLEAN => Ok(MmsValue::Boolean(val.first().copied().unwrap_or(0) != 0)),

        DATA_INTEGER => {
            if val.is_empty() {
                return Err(GatewayError::Protocol("MMS: zero-length integer".into()));
            }
            let mut n: i64 = if val[0] & 0x80 != 0 { -1i64 } else { 0 };
            for &b in val {
                n = (n << 8) | (b as i64);
            }
            Ok(MmsValue::Integer(n))
        },

        DATA_UNSIGNED => {
            let mut n: u64 = 0;
            for &b in val {
                n = (n << 8) | (b as u64);
            }
            Ok(MmsValue::Unsigned(n))
        },

        DATA_FLOAT => {
            if val.len() == 5 {
                // float32: [exponent_bits(1)] [ieee754_be(4)]
                let bytes = [val[1], val[2], val[3], val[4]];
                let f = f32::from_be_bytes(bytes);
                Ok(MmsValue::Float32(f))
            } else if val.len() == 9 {
                // float64: [exponent_bits(1)] [ieee754_be(8)]
                let bytes = [
                    val[1], val[2], val[3], val[4], val[5], val[6], val[7], val[8],
                ];
                let f = f64::from_be_bytes(bytes);
                Ok(MmsValue::Float64(f))
            } else {
                Err(GatewayError::Protocol(format!(
                    "MMS: unexpected float length {}",
                    val.len()
                )))
            }
        },

        DATA_BIT_STRING => {
            let unused = val.first().copied().unwrap_or(0);
            Ok(MmsValue::BitString {
                bytes: val[1..].to_vec(),
                unused_bits: unused,
            })
        },

        DATA_OCTET_STRING => Ok(MmsValue::OctetString(val.to_vec())),

        DATA_VISIBLE_STRING | DATA_MMS_STRING => {
            let s = String::from_utf8_lossy(val).into_owned();
            Ok(MmsValue::VisibleString(s))
        },

        DATA_UTC_TIME => {
            if val.len() == 8 {
                let mut arr = [0u8; 8];
                arr.copy_from_slice(val);
                Ok(MmsValue::UtcTime(arr))
            } else {
                Err(GatewayError::Protocol(format!(
                    "MMS: UTC time length {} (expected 8)",
                    val.len()
                )))
            }
        },

        other => Err(GatewayError::Protocol(format!(
            "MMS: unknown Data tag 0x{:02X}",
            other
        ))),
    }
}

// ── SBO / SBOw Select ─────────────────────────────────────────────────────────

/// Build an MMS Read-Request for the `$SBO` attribute (SBO-Normal, ctlModel=2).
///
/// IEC 61850 SBO-Normal select is a **read** of the `$SBO` attribute.
/// The server returns a non-empty VisibleString on success, empty on failure.
///
/// `item` should end with `$Oper$ctlVal` (as stored in the DB).
/// The function derives the SBO path: strips `$ctlVal` and `$Oper`, appends `$SBO`.
pub fn build_sbo_select_request(invoke_id: u8, domain: &str, item: &str) -> Vec<u8> {
    let base = item
        .strip_suffix("$ctlVal")
        .unwrap_or(item)
        .strip_suffix("$Oper")
        .unwrap_or(item);
    let sbo_item = format!("{}$SBO", base);
    build_read_request(invoke_id, domain, &sbo_item)
}

/// Parse the SBO-Normal select response (Read-Response for `$SBO`).
///
/// Returns `Ok(true)` if selected (non-empty VisibleString),
/// `Ok(false)` if select refused (empty VisibleString).
pub fn parse_sbo_select_response(pdu: &[u8]) -> Result<bool> {
    let (_, value) = parse_read_response(pdu)?;
    match value {
        MmsValue::VisibleString(ref s) if !s.is_empty() => Ok(true),
        MmsValue::VisibleString(_) => Ok(false),
        MmsValue::Failure(code) => Err(GatewayError::Protocol(format!(
            "MMS: SBO select data-access error {}",
            code
        ))),
        other => Err(GatewayError::Protocol(format!(
            "MMS: SBO select unexpected value: {:?}",
            other
        ))),
    }
}

/// Build a Write-Request for `$SBOw` (SBO-Enhanced / ctlModel=4 select-with-value).
///
/// The SBOw structure is identical to `Oper`; only the target node name differs.
/// `item` should end with `$Oper$ctlVal` (as stored in the DB).
pub fn build_sbow_select_bool_request(
    invoke_id: u8,
    domain: &str,
    item: &str,
    value: bool,
) -> Vec<u8> {
    let base = item
        .strip_suffix("$ctlVal")
        .unwrap_or(item)
        .strip_suffix("$Oper")
        .unwrap_or(item);
    let sbow_item = format!("{}$SBOw", base);
    let oper_data = encode_oper(&[DATA_BOOLEAN, 0x01, value as u8], invoke_id);
    build_write_request(invoke_id, domain, &sbow_item, &oper_data)
}

// ── Write-Request ─────────────────────────────────────────────────────────────

/// Build an MMS Write-Request to set a `boolean` value.
/// Build an IEC 61850 Operate request for a boolean (SPC/DPC) control object.
///
/// `item` should end with `$Oper$ctlVal` as stored in the database.  The
/// `$ctlVal` suffix is stripped automatically so the write targets the parent
/// `$Oper` node with the complete Oper structure, matching libiec61850 behaviour.
pub fn build_write_bool_request(invoke_id: u8, domain: &str, item: &str, value: bool) -> Vec<u8> {
    let oper_item = item.strip_suffix("$ctlVal").unwrap_or(item);
    let oper_data = encode_oper(&[DATA_BOOLEAN, 0x01, value as u8], invoke_id);
    build_write_request(invoke_id, domain, oper_item, &oper_data)
}

/// Build an IEC 61850 Operate request for a float (APC) analog control object.
///
/// `item` should end with `$Oper$setMag$f` or `$Oper$setMag`.  The suffix is
/// stripped to target `$Oper` with the complete Oper structure.
pub fn build_write_f32_request(invoke_id: u8, domain: &str, item: &str, value: f32) -> Vec<u8> {
    let oper_item = item
        .strip_suffix("$setMag$f")
        .or_else(|| item.strip_suffix("$setMag"))
        .unwrap_or(item);
    // setMag = structure [1] { floating-point [7] }
    let f_bytes = value.to_be_bytes();
    let setmag_inner = [
        DATA_FLOAT, 0x05, 0x08, f_bytes[0], f_bytes[1], f_bytes[2], f_bytes[3],
    ];
    let mut setmag = Vec::with_capacity(2 + setmag_inner.len());
    setmag.push(DATA_STRUCTURE); // structure [2] per mms-extended.asn
    push_ber_len(&mut setmag, setmag_inner.len());
    setmag.extend_from_slice(&setmag_inner);

    let oper_data = encode_oper(&setmag, invoke_id);
    build_write_request(invoke_id, domain, oper_item, &oper_data)
}

/// Encode the IEC 61850 Oper structure as an MMS Data TLV.
///
/// Structure: `structure [2] { ctlVal|setMag, origin, ctlNum, T, Test, Check }` (mms-extended.asn).
///
/// - `ctrl_bytes` — the already-encoded `ctlVal` or `setMag` Data TLV.
/// - `ctl_num` — control sequence number (use invoke_id for simplicity).
fn encode_oper(ctrl_bytes: &[u8], ctl_num: u8) -> Vec<u8> {
    // origin: structure [1] { orCat=integer(3=remote), orIdent=octet-string(empty) }
    let origin_inner = [DATA_INTEGER, 0x01, 0x03, DATA_OCTET_STRING, 0x00];
    let origin_len = origin_inner.len(); // 5 bytes

    // ctlNum: unsigned [6] = 86 01 [num]
    let ctlnum_bytes = [DATA_UNSIGNED, 0x01, ctl_num];

    // T: utc-time [17] = 91 08 [8 bytes]
    let t_bytes = utc_time_now();

    // Test: boolean [3] = 83 01 00 (false)
    let test_bytes = [DATA_BOOLEAN, 0x01, 0x00u8];

    // Check: bit-string [4] = 84 02 06 00 (2 bits, all zero = no interlock/synchro check)
    let check_bytes = [DATA_BIT_STRING, 0x02, 0x06, 0x00u8];

    let oper_content_len = ctrl_bytes.len()
        + (2 + origin_len) // A1 [5 bytes]
        + ctlnum_bytes.len()
        + (2 + 8) // 91 08 [t_bytes]
        + test_bytes.len()
        + check_bytes.len();

    let mut buf = Vec::with_capacity(2 + oper_content_len);
    buf.push(DATA_STRUCTURE); // structure [2] per mms-extended.asn = Oper SEQUENCE
    push_ber_len(&mut buf, oper_content_len);

    buf.extend_from_slice(ctrl_bytes); // ctlVal or setMag

    buf.push(DATA_STRUCTURE); // origin structure [2] per mms-extended.asn
    push_ber_len(&mut buf, origin_len);
    buf.extend_from_slice(&origin_inner);

    buf.extend_from_slice(&ctlnum_bytes); // ctlNum

    buf.push(DATA_UTC_TIME); // T: utc-time [17]
    buf.push(0x08);
    buf.extend_from_slice(&t_bytes);

    buf.extend_from_slice(&test_bytes); // Test
    buf.extend_from_slice(&check_bytes); // Check

    buf
}

/// Current time as 8-byte MMS UtcTime:
/// `[secs(4)][fraction(3, 1/2^24 s units)][quality(1)]`
fn utc_time_now() -> [u8; 8] {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as u32;
    let frac = (now.subsec_nanos() as u64 * (1u64 << 24) / 1_000_000_000) as u32;
    [
        (secs >> 24) as u8,
        (secs >> 16) as u8,
        (secs >> 8) as u8,
        secs as u8,
        (frac >> 16) as u8,
        (frac >> 8) as u8,
        frac as u8,
        0x00, // quality: no drift, no failure
    ]
}

/// Generic Write-Request builder. `data_bytes` = the encoded Data TLV to write.
///
/// **Structural difference from Read:** `ReadRequest` wraps `variableAccessSpecification`
/// in `[1] EXPLICIT`, giving the Read a visible A1 outer tag.  `WriteRequest` has **no**
/// such wrapper — `listOfVariable [0] IMPLICIT` = A0 appears directly in the body.
///
/// ```text
/// A0 [len]  confirmedRequestPDU
///   02 01 [id]  invokeID
///   A5 [len]  Write-Request [5]
///     A0 [len]  listOfVariable [0] IMPLICIT SEQUENCE OF  ← A0, no A1 wrapper!
///       30 [len]  ListOfVariableSeq SEQUENCE
///         A0 [len]  VariableSpec: name [0] EXPLICIT
///           A1 [len]  ObjectName: domain-specific [1] IMPLICIT SEQUENCE
///             1A [dlen] [domain]
///             1A [ilen] [item]
///     A0 [len]  listOfData [0] IMPLICIT SEQUENCE OF Data
///       [data_bytes]
/// ```
fn build_write_request(invoke_id: u8, domain: &str, item: &str, data_bytes: &[u8]) -> Vec<u8> {
    let d = domain.as_bytes();
    let i = item.as_bytes();

    // Innermost 4 levels (same as Read's inner structure, but WITHOUT the outer A1 wrapper)
    let a1_domain_content = 2 + d.len() + 2 + i.len(); // A1 domainspecific [1] content
    let a0_name_content = 2 + a1_domain_content; // A0 varspec.name [0] EXPLICIT content
    let seq_content = 2 + a0_name_content; // 30 ListOfVariableSeq content
    let a0_list_content = 2 + seq_content; // A0 listOfVariable [0] content (one item)

    // listOfData [0] IMPLICIT SEQUENCE OF Data: A0
    let a0_data_content = data_bytes.len();

    // Write [5] content = A0 listOfVariable TLV + A0 listOfData TLV
    let write_inner = (2 + a0_list_content) + (2 + a0_data_content);
    let req_inner = 3 + 2 + write_inner; // 3 = invokeID TLV, 2 = A5 header

    let mut buf = Vec::with_capacity(2 + req_inner);

    // confirmedRequestPDU A0
    buf.push(TAG_CONFIRMED_REQ);
    push_ber_len(&mut buf, req_inner);

    // invokeID: 02 01 [id]
    buf.extend_from_slice(&[TAG_INTEGER, 0x01, invoke_id]);

    // Write [5]: A5
    buf.push(0xA5);
    push_ber_len(&mut buf, write_inner);

    // listOfVariable [0] IMPLICIT: A0  (WriteRequest has NO EXPLICIT [1] wrapper)
    buf.push(0xA0);
    push_ber_len(&mut buf, a0_list_content);

    // 30 ListOfVariableSeq SEQUENCE (directly inside A0)
    buf.push(TAG_SEQUENCE);
    push_ber_len(&mut buf, seq_content);

    // A0 VariableSpec.name [0] EXPLICIT
    buf.push(0xA0);
    push_ber_len(&mut buf, a0_name_content);

    // A1 ObjectName.domainspecific [1] IMPLICIT SEQUENCE
    buf.push(0xA1);
    push_ber_len(&mut buf, a1_domain_content);

    // 1A [domain_len] [domain]
    buf.push(TAG_VISIBLE_STR);
    buf.push(d.len() as u8);
    buf.extend_from_slice(d);

    // 1A [item_len] [item]
    buf.push(TAG_VISIBLE_STR);
    buf.push(i.len() as u8);
    buf.extend_from_slice(i);

    // listOfData [0] IMPLICIT: A0
    buf.push(0xA0);
    push_ber_len(&mut buf, a0_data_content);
    buf.extend_from_slice(data_bytes);

    buf
}

/// Parse an MMS Write-Response. Returns `Ok(invoke_id)` on success,
/// `Err` if the server returned a data-access error or the PDU is malformed.
///
/// Expected wire format:
/// ```text
/// A1 [len]  confirmedResponsePDU
///   02 01 [id]  invokeID
///   A5 [len]  Write-Response [5]
///     81 00     success NULL          (one per written variable)
///   -- or --
///     A0 [len]  failure
///       0A 01 [code]  DataAccessError (ENUMERATED)
/// ```
pub fn parse_write_response(pdu: &[u8]) -> Result<u8> {
    tracing::debug!(bytes = ?&pdu[..pdu.len().min(24)], "write response raw");
    // A1 [len]  confirmedResponsePDU
    let (_, pdu_content) = parse_tlv(pdu, TAG_CONFIRMED_RESP)
        .ok_or_else(|| GatewayError::Protocol("MMS: expected confirmedResponsePDU (A1)".into()))?;

    // 02 01 [id]  invokeID
    let (rest, id_bytes) = parse_tlv(pdu_content, TAG_INTEGER)
        .ok_or_else(|| GatewayError::Protocol("MMS: missing invokeID in write response".into()))?;
    let invoke_id = id_bytes.first().copied().unwrap_or(0);

    // A5 [len]  Write-Response [5]
    let (_, write_resp) = parse_tlv(rest, 0xA5)
        .ok_or_else(|| GatewayError::Protocol("MMS: expected Write-Response tag (A5)".into()))?;

    // WriteResponse CHOICE: 81 00 = success NULL, 80 [len] [code] = failure DataAccessError
    match write_resp.first().copied() {
        Some(0x81) => Ok(invoke_id),
        Some(0x80) => {
            // failure [0] primitive: 80 01 [code]
            let code = write_resp.get(2).copied().unwrap_or(0);
            Err(GatewayError::Protocol(format!(
                "MMS Write data-access error code {}",
                code
            )))
        },
        Some(other) => Err(GatewayError::Protocol(format!(
            "MMS: unexpected WriteResponse tag 0x{:02X}",
            other
        ))),
        None => Err(GatewayError::Protocol(
            "MMS: empty Write-Response body".into(),
        )),
    }
}

// ── Simple Write helpers (for RCB attributes, NOT control Oper) ──────────────

/// Build an MMS Write-Request for a **single boolean** attribute.
///
/// Unlike [`build_write_bool_request`] (which wraps the value in a full IEC 61850
/// `Oper` structure), this writes a bare `boolean [3]` Data value.  Use this for
/// writing RCB attributes (`RptEna`, `GI`, `PurgeBuf`, etc.).
pub fn build_write_simple_bool(invoke_id: u8, domain: &str, item: &str, value: bool) -> Vec<u8> {
    let data = [DATA_BOOLEAN, 0x01, value as u8];
    build_write_request(invoke_id, domain, item, &data)
}

// ── Report parsing ────────────────────────────────────────────────────────────

/// IEC 61850 BinaryTime6 tag in MMS Data CHOICE.
const DATA_BINARY_TIME: u8 = 0x8C;

/// A parsed IEC 61850 Report received via an unconfirmed MMS PDU.
#[derive(Debug)]
pub struct ParsedReport {
    /// RptID of the sending RCB.
    pub rpt_id: String,
    /// Report timestamp as Unix milliseconds (from OptFlds[2] BinaryTime6),
    /// or `None` when the timestamp field is absent.
    pub timestamp_ms: Option<u64>,
    /// Total number of elements in the dataset (= inclusion-bitmap bit count).
    pub dataset_size: usize,
    /// Dataset element indices (0-based) that are **included** in this report.
    pub element_indices: Vec<usize>,
    /// Decoded data values, one per entry in `element_indices` (same order).
    pub values: Vec<MmsValue>,
}

/// Parse an unconfirmed MMS PDU (`0xA3`) as an IEC 61850 InformationReport.
///
/// Returns `None` if the PDU is malformed or not a valid report.
///
/// The expected wire format is:
/// ```text
/// A3 [len]     unconfirmedPDU
///   A0 [len]   unconfirmedService: informationReport [0]
///     A1 [len] variableAccessSpecification: variableListName [1]
///       80 [n] [name]   vmdspecific Identifier ("RPT")
///     A0 [len] listOfAccessResult [0]
///       items… RptID, OptFlds, (seqNum?), (ts?), (datSet?), (bufOvfl?),
///              (entryId?), (confRev?), (segmentation?),
///              inclusion-BitString, (data-refs?), data-values…, (reasons?)
/// ```
pub fn parse_report(pdu: &[u8]) -> Option<ParsedReport> {
    use super::transport::parse_tlv;

    // Strip the outer unconfirmedPDU (A3) and informationReport (A0) wrappers.
    let (_, a0_content) = parse_tlv(pdu, 0xA3)?;
    let (_, info) = parse_tlv(a0_content, 0xA0)?;

    // Skip variableAccessSpecification (A1 … variableListName).
    let (rest, _) = parse_tlv(info, 0xA1)?;

    // Enter listOfAccessResult (A0).
    let (_, items) = parse_tlv(rest, 0xA0)?;

    // ── Sequential parsing of report items ────────────────────────────────────
    let mut pos = 0usize;

    // Item 0: RptID (VisibleString 0x8A)
    let (consumed, rpt_id_val) = parse_data_item(&items[pos..])?;
    pos += consumed;
    let rpt_id = match rpt_id_val {
        Some(MmsValue::VisibleString(s)) => s,
        _ => return None,
    };

    // Item 1: OptFlds (BitString 0x84)
    let (consumed, opt_val) = parse_data_item(&items[pos..])?;
    pos += consumed;
    let (opt_bytes, _opt_unused) = match opt_val {
        Some(MmsValue::BitString { bytes, unused_bits }) => (bytes, unused_bits),
        _ => return None,
    };

    // Helper: true if OptFlds bit `n` is set (bit 0 = MSB of first byte).
    let opt_bit = |n: usize| -> bool {
        opt_bytes
            .get(n / 8)
            .map(|b| (b >> (7 - n % 8)) & 1 == 1)
            .unwrap_or(false)
    };

    let mut timestamp_ms: Option<u64> = None;

    // bit 1: seqNum (Unsigned) → skip
    if opt_bit(1) {
        let (n, _) = parse_data_item(&items[pos..])?;
        pos += n;
    }

    // bit 2: reportTimestamp (BinaryTime6, tag 0x8C) → decode as Unix ms
    if opt_bit(2) {
        let (n, ts_val) = parse_data_item(&items[pos..])?;
        pos += n;
        if let Some(MmsValue::OctetString(ref ts_bytes)) = ts_val
            && ts_bytes.len() == 6
        {
            // libiec61850 stores days since Unix epoch (1970-01-01), NOT 1984.
            let ms_today: u64 = ((ts_bytes[0] as u64) << 24)
                | ((ts_bytes[1] as u64) << 16)
                | ((ts_bytes[2] as u64) << 8)
                | (ts_bytes[3] as u64);
            let days: u64 = ((ts_bytes[4] as u64) << 8) | (ts_bytes[5] as u64);
            timestamp_ms = Some(days * 86_400_000 + ms_today);
        }
    }

    // bit 4: dataSetName (VisibleString) → skip
    if opt_bit(4) {
        let (n, _) = parse_data_item(&items[pos..])?;
        pos += n;
    }

    // bit 6: bufOvfl (Boolean) → skip
    if opt_bit(6) {
        let (n, _) = parse_data_item(&items[pos..])?;
        pos += n;
    }

    // bit 7: entryId (OctetString) → skip
    if opt_bit(7) {
        let (n, _) = parse_data_item(&items[pos..])?;
        pos += n;
    }

    // bit 8: confRev (Unsigned) → skip
    if opt_bit(8) {
        let (n, _) = parse_data_item(&items[pos..])?;
        pos += n;
    }

    // bit 9: segmentation → subSeqNum + moreSegmentsFollow (skip both)
    if opt_bit(9) {
        for _ in 0..2 {
            let (n, _) = parse_data_item(&items[pos..])?;
            pos += n;
        }
    }

    // inclusion BitString — determines which dataset elements are in this report.
    let (consumed, incl_val) = parse_data_item(&items[pos..])?;
    pos += consumed;
    let (incl_bytes, incl_unused) = match incl_val {
        Some(MmsValue::BitString { bytes, unused_bits }) => (bytes, unused_bits),
        _ => return None,
    };

    let dataset_size = incl_bytes.len() * 8 - incl_unused as usize;
    let incl_bit = |n: usize| -> bool {
        incl_bytes
            .get(n / 8)
            .map(|b| (b >> (7 - n % 8)) & 1 == 1)
            .unwrap_or(false)
    };
    let element_indices: Vec<usize> = (0..dataset_size).filter(|&n| incl_bit(n)).collect();

    // bit 5: data-reference (VisibleString per included element) → skip all
    if opt_bit(5) {
        for _ in 0..element_indices.len() {
            let (n, _) = parse_data_item(&items[pos..])?;
            pos += n;
        }
    }

    // Data values: one per included element (in dataset order).
    let mut values = Vec::with_capacity(element_indices.len());
    for _ in 0..element_indices.len() {
        let (n, val) = parse_data_item(&items[pos..])?;
        pos += n;
        values.push(val.unwrap_or(MmsValue::Failure(0)));
    }

    // bit 3: reasonForInclusion (BitString per included element) → ignore

    Some(ParsedReport {
        rpt_id,
        timestamp_ms,
        dataset_size,
        element_indices,
        values,
    })
}

/// Parse exactly one MMS Data TLV from the start of `buf`.
///
/// Returns `Some((bytes_consumed, value_option))` on success.
/// - `value_option` is `None` when the item is a structure / array (constructed
///   type that we don't recurse into), but the bytes are still consumed so the
///   caller can advance the cursor.
///
/// Returns `None` if the buffer is too short or the TLV is malformed.
fn parse_data_item(buf: &[u8]) -> Option<(usize, Option<MmsValue>)> {
    if buf.is_empty() {
        return None;
    }

    let tag = buf[0];
    let (len, hdr) = parse_ber_len(&buf[1..])?;
    let total = 1 + hdr + len;
    if buf.len() < total {
        return None;
    }
    let val = &buf[1 + hdr..total];

    let mms = match tag {
        DATA_BOOLEAN => Some(MmsValue::Boolean(val.first().copied().unwrap_or(0) != 0)),

        DATA_BIT_STRING => {
            let unused = val.first().copied().unwrap_or(0);
            Some(MmsValue::BitString {
                bytes: val.get(1..).unwrap_or(&[]).to_vec(),
                unused_bits: unused,
            })
        },

        DATA_INTEGER => {
            if val.is_empty() {
                return None;
            }
            let mut n: i64 = if val[0] & 0x80 != 0 { -1i64 } else { 0 };
            for &b in val {
                n = (n << 8) | (b as i64);
            }
            Some(MmsValue::Integer(n))
        },

        DATA_UNSIGNED => {
            let mut n: u64 = 0;
            for &b in val {
                n = (n << 8) | (b as u64);
            }
            Some(MmsValue::Unsigned(n))
        },

        DATA_FLOAT => match val.len() {
            5 => {
                let bytes = [val[1], val[2], val[3], val[4]];
                Some(MmsValue::Float32(f32::from_be_bytes(bytes)))
            },
            9 => {
                let bytes = [
                    val[1], val[2], val[3], val[4], val[5], val[6], val[7], val[8],
                ];
                Some(MmsValue::Float64(f64::from_be_bytes(bytes)))
            },
            _ => None,
        },

        DATA_OCTET_STRING | DATA_BINARY_TIME => Some(MmsValue::OctetString(val.to_vec())),

        DATA_VISIBLE_STRING | DATA_MMS_STRING => Some(MmsValue::VisibleString(
            String::from_utf8_lossy(val).into_owned(),
        )),

        DATA_UTC_TIME => {
            if val.len() == 8 {
                let mut arr = [0u8; 8];
                arr.copy_from_slice(val);
                Some(MmsValue::UtcTime(arr))
            } else {
                None
            }
        },

        // Constructed types (array 0xA1, structure 0xA2) → consume bytes, no value
        0xA1 | 0xA2 => None,

        // Unknown tag → consume the bytes anyway to keep cursor advancing
        _ => None,
    };

    Some((total, mms))
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_request_matches_capture() {
        // Captured from libiec61850 client reading simpleIOGenericIO/GGIO1.AnIn1.mag.f
        let expected = hex::decode(
            "a038020101a433a131a02f302da02ba1291a1173696d706c65494f47656e65\
             726963494f1a144747494f31244d5824416e496e31246d61672466",
        )
        .unwrap();

        let got = build_read_request(1, "simpleIOGenericIO", "GGIO1$MX$AnIn1$mag$f");
        assert_eq!(got, expected, "Read request bytes mismatch");
    }

    #[test]
    fn parse_float_response() {
        // MMS Read-Response for a float32 value ≈ -0.0977 (0xBDC6AF00):
        //   A1 10  confirmedResponsePDU (content=16)
        //     02 01 01  invokeID=1
        //     A4 0B  Read-Response (content=11)
        //       A1 09  listOfAccessResults (content=9)
        //         A1 07  AccessResult success (content=7)
        //           87 05  Data floating-point (content=5)
        //             08 BD C6 AF 00  (exponent=8, IEEE-754 = -0.0977)
        let resp = hex::decode("a110020101a40ba109a107870508bdc6af00").unwrap();
        let (id, val) = parse_read_response(&resp).unwrap();
        assert_eq!(id, 1);
        match val {
            MmsValue::Float32(f) => {
                // 0xBDC6AF00 ≈ -0.0977
                assert!(
                    (f - (-0.0977f32)).abs() < 0.001,
                    "float value mismatch: {}",
                    f
                );
            },
            other => panic!("expected Float32, got {:?}", other),
        }
    }
}
