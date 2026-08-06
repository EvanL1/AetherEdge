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
    pub fn is_ok(&self) -> bool {
        !matches!(self, Self::Failure(_))
    }
}

// ── Encoder ───────────────────────────────────────────────────────────────────

/// Size of the BER length field `push_ber_len` emits for `len` content bytes.
fn ber_len_size(len: usize) -> usize {
    if len < 0x80 {
        1
    } else if len < 0x100 {
        2
    } else {
        3
    }
}

/// Total encoded size of one TLV: tag byte + length field + content.
fn tlv_size(content: usize) -> usize {
    1 + ber_len_size(content) + content
}

/// Build an MMS Read-Request PDU for a single domain-specific variable.
///
/// `invoke_id`: 1–255, used to match request with response.
/// `domain`: MMS domain (IED logical device name), e.g. `"simpleIOGenericIO"`.
/// `item`: MMS item ID with functional constraint, e.g. `"GGIO1$MX$AnIn1$mag$f"`.
pub fn build_read_request(invoke_id: u8, domain: &str, item: &str) -> Vec<u8> {
    let d = domain.as_bytes();
    let i = item.as_bytes();

    // Sizes are computed bottom-up. Each variable = CONTENT length of that TLV
    // (i.e. what goes into push_ber_len for that tag); tlv_size accounts for
    // the exact BER length-field size push_ber_len will emit.
    let a1_domain_content = tlv_size(d.len()) + tlv_size(i.len()); // 1A [d] 1A [i]
    let a0_outer_content = tlv_size(a1_domain_content); // A1 TLV total
    let seq_content = tlv_size(a0_outer_content); // A0 TLV total
    let a0_varspec_content = tlv_size(seq_content); // 30 TLV total
    let a1_list_content = tlv_size(a0_varspec_content); // A0 TLV total
    let a4_read_content = tlv_size(a1_list_content); // A1 TLV total
    // outer A0 content = invokeID(3) + A4 TLV total
    let req_inner = 3 + tlv_size(a4_read_content);

    let mut buf = Vec::with_capacity(tlv_size(req_inner));

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
    push_ber_len(&mut buf, d.len());
    buf.extend_from_slice(d);

    // 1A [item_len] [item]
    buf.push(TAG_VISIBLE_STR);
    push_ber_len(&mut buf, i.len());
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
    let val = buf
        .get(1 + hdr..1 + hdr + len)
        .ok_or_else(|| GatewayError::Protocol("MMS: Data length exceeds buffer".into()))?;

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
    let a1_domain_content = tlv_size(d.len()) + tlv_size(i.len()); // A1 domainspecific [1] content
    let a0_name_content = tlv_size(a1_domain_content); // A0 varspec.name [0] EXPLICIT content
    let seq_content = tlv_size(a0_name_content); // 30 ListOfVariableSeq content
    let a0_list_content = tlv_size(seq_content); // A0 listOfVariable [0] content (one item)

    // listOfData [0] IMPLICIT SEQUENCE OF Data: A0
    let a0_data_content = data_bytes.len();

    // Write [5] content = A0 listOfVariable TLV + A0 listOfData TLV
    let write_inner = tlv_size(a0_list_content) + tlv_size(a0_data_content);
    let req_inner = 3 + tlv_size(write_inner); // 3 = invokeID TLV, then A5 TLV

    let mut buf = Vec::with_capacity(tlv_size(req_inner));

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
    push_ber_len(&mut buf, d.len());
    buf.extend_from_slice(d);

    // 1A [item_len] [item]
    buf.push(TAG_VISIBLE_STR);
    push_ber_len(&mut buf, i.len());
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
    /// Report timestamp as Unix milliseconds (from `OptFlds[2]` BinaryTime6),
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

    /// Encode one BER TLV (test helper mirroring the encoder conventions).
    fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        push_ber_len(&mut out, content.len());
        out.extend_from_slice(content);
        out
    }

    /// Build a Read-Response PDU carrying one raw AccessResult.
    fn read_response(access_result: &[u8]) -> Vec<u8> {
        let list = tlv(0xA1, access_result);
        let read = tlv(TAG_REJECT_PDU, &list);
        let mut content = vec![TAG_INTEGER, 0x01, 0x01];
        content.extend_from_slice(&read);
        tlv(TAG_CONFIRMED_RESP, &content)
    }

    /// Build a Write-Response PDU with the given response body.
    fn write_response(body: &[u8]) -> Vec<u8> {
        let mut content = vec![TAG_INTEGER, 0x01, 0x05];
        content.extend_from_slice(&tlv(0xA5, body));
        tlv(TAG_CONFIRMED_RESP, &content)
    }

    /// Parse a Read-Response built by `read_response` and return its value.
    fn read_value(access_result: &[u8]) -> MmsValue {
        let (_, value) =
            parse_read_response(&read_response(access_result)).expect("valid response");
        value
    }

    /// Walk a Read-Request PDU and return `(invoke_id, domain, item)`,
    /// asserting every nested BER length is consistent along the way.
    fn decode_read_request(pdu: &[u8]) -> (u8, String, String) {
        let (rest, content) = parse_tlv(pdu, TAG_CONFIRMED_REQ).expect("confirmedRequestPDU");
        assert!(rest.is_empty(), "trailing bytes after confirmedRequestPDU");
        let (rest, id) = parse_tlv(content, TAG_INTEGER).expect("invokeID");
        let (rest, read) = parse_tlv(rest, TAG_REJECT_PDU).expect("read request");
        assert!(rest.is_empty(), "trailing bytes after Read-Request");
        let (rest, list) = parse_tlv(read, 0xA1).expect("variableAccessSpecification");
        assert!(rest.is_empty(), "trailing bytes after access specification");
        let (_, varspec) = parse_tlv(list, 0xA0).expect("variable specification");
        let (_, seq) = parse_tlv(varspec, TAG_SEQUENCE).expect("object name sequence");
        let (_, outer) = parse_tlv(seq, 0xA0).expect("outer name");
        let (_, domspec) = parse_tlv(outer, 0xA1).expect("domain-specific name");
        let (rest, domain) = parse_tlv(domspec, TAG_VISIBLE_STR).expect("domain string");
        let (rest, item) = parse_tlv(rest, TAG_VISIBLE_STR).expect("item string");
        assert!(rest.is_empty(), "trailing bytes after item identifier");
        (
            id[0],
            String::from_utf8(domain.to_vec()).expect("utf8 domain"),
            String::from_utf8(item.to_vec()).expect("utf8 item"),
        )
    }

    /// Walk a Write-Request PDU and return `(invoke_id, domain, item, data)`,
    /// asserting every nested BER length is consistent along the way.
    fn decode_write_request(pdu: &[u8]) -> (u8, String, String, Vec<u8>) {
        let (rest, content) = parse_tlv(pdu, TAG_CONFIRMED_REQ).expect("confirmedRequestPDU");
        assert!(rest.is_empty(), "trailing bytes after confirmedRequestPDU");
        let (rest, id) = parse_tlv(content, TAG_INTEGER).expect("invokeID");
        let (rest, write) = parse_tlv(rest, 0xA5).expect("write request");
        assert!(rest.is_empty(), "trailing bytes after Write-Request");
        let (rest, listvar) = parse_tlv(write, 0xA0).expect("listOfVariable");
        let (_, seq) = parse_tlv(listvar, TAG_SEQUENCE).expect("variable sequence");
        let (_, name) = parse_tlv(seq, 0xA0).expect("variable name");
        let (_, domspec) = parse_tlv(name, 0xA1).expect("domain-specific name");
        let (name_rest, domain) = parse_tlv(domspec, TAG_VISIBLE_STR).expect("domain string");
        let (name_rest, item) = parse_tlv(name_rest, TAG_VISIBLE_STR).expect("item string");
        assert!(name_rest.is_empty(), "trailing bytes after item identifier");
        let (rest, data) = parse_tlv(rest, 0xA0).expect("listOfData");
        assert!(rest.is_empty(), "trailing bytes after listOfData");
        (
            id[0],
            String::from_utf8(domain.to_vec()).expect("utf8 domain"),
            String::from_utf8(item.to_vec()).expect("utf8 item"),
            data.to_vec(),
        )
    }

    /// Build an InformationReport PDU around the raw `listOfAccessResult` items.
    fn report_pdu(items: &[u8]) -> Vec<u8> {
        let mut info = tlv(0xA1, &[0x80, 0x03, b'R', b'P', b'T']);
        info.extend_from_slice(&tlv(0xA0, items));
        tlv(0xA3, &tlv(0xA0, &info))
    }

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

    #[test]
    fn read_request_roundtrips_through_a_full_tlv_walk() {
        for (invoke, domain, item) in [
            (1u8, "simpleIOGenericIO", "GGIO1$MX$AnIn1$mag$f"),
            (255, "LD", "LLN0$BR$EventsBRCB01$RptEna"),
        ] {
            let pdu = build_read_request(invoke, domain, item);
            let (got_invoke, got_domain, got_item) = decode_read_request(&pdu);
            assert_eq!(
                (got_invoke, got_domain.as_str(), got_item.as_str()),
                (invoke, domain, item)
            );
        }
    }

    #[test]
    fn read_request_stays_decodable_for_long_object_names() {
        let domain = "D".repeat(60);
        let item = "I".repeat(80);
        let pdu = build_read_request(1, &domain, &item);
        let (invoke, got_domain, got_item) = decode_read_request(&pdu);
        assert_eq!(
            (invoke, got_domain.as_str(), got_item.as_str()),
            (1, domain.as_str(), item.as_str())
        );
    }

    #[test]
    fn read_response_accepts_failure_wrapped_and_direct_data_forms() {
        assert!(matches!(
            read_value(&[0x80, 0x01, 0x0A]),
            MmsValue::Failure(10)
        ));
        assert!(matches!(
            read_value(&[DATA_INTEGER, 0x01, 0x2A]),
            MmsValue::Integer(42)
        ));
        // Success wrapper (A1) around the Data value.
        let wrapped = tlv(0xA1, &[DATA_BOOLEAN, 0x01, 0xFF]);
        assert!(matches!(read_value(&wrapped), MmsValue::Boolean(true)));
    }

    #[test]
    fn read_response_rejects_malformed_pdus() {
        let missing_list = {
            let mut content = vec![TAG_INTEGER, 0x01, 0x01];
            content.extend_from_slice(&tlv(TAG_REJECT_PDU, &[]));
            tlv(TAG_CONFIRMED_RESP, &content)
        };
        for pdu in [
            Vec::new(),                                   // empty buffer
            vec![0xA0, 0x03, 0x02, 0x01, 0x01],           // request tag, not response
            tlv(TAG_CONFIRMED_RESP, &[]),                 // missing invokeID
            tlv(TAG_CONFIRMED_RESP, &[0x02, 0x01, 0x01]), // missing Read-Response
            missing_list,                                 // missing listOfAccessResults
            read_response(&[]),                           // empty AccessResult
            read_response(&[0x55, 0x01, 0x00]),           // unknown AccessResult tag
            vec![0xA1, 0x7F, 0x02, 0x01, 0x01],           // declared length beyond buffer
        ] {
            assert!(parse_read_response(&pdu).is_err(), "{pdu:02X?}");
        }
    }

    #[test]
    fn read_response_rejects_truncated_data_without_panicking() {
        // An integer that declares 127 content bytes but provides none.
        let pdu = read_response(&[DATA_INTEGER, 0x7F]);
        assert!(parse_read_response(&pdu).is_err());
    }

    #[test]
    fn data_decoder_handles_integer_widths_and_signs() {
        for (bytes, expected) in [
            (vec![DATA_INTEGER, 0x01, 0x7F], 127i64),
            (vec![DATA_INTEGER, 0x01, 0x80], -128),
            (vec![DATA_INTEGER, 0x02, 0x01, 0x00], 256),
            (vec![DATA_INTEGER, 0x02, 0xFF, 0x7F], -129),
            (vec![DATA_INTEGER, 0x04, 0xFF, 0xFF, 0xFF, 0xF6], -10),
        ] {
            match read_value(&bytes) {
                MmsValue::Integer(n) => assert_eq!(n, expected, "{bytes:02X?}"),
                other => panic!("expected Integer for {bytes:02X?}, got {other:?}"),
            }
        }
        match read_value(&[DATA_UNSIGNED, 0x02, 0xFF, 0xFF]) {
            MmsValue::Unsigned(n) => assert_eq!(n, 65_535),
            other => panic!("expected Unsigned, got {other:?}"),
        }
        assert!(matches!(
            read_value(&[DATA_BOOLEAN, 0x01, 0x00]),
            MmsValue::Boolean(false)
        ));
    }

    #[test]
    fn data_decoder_handles_double_strings_bitstring_octets_and_utctime() {
        let mut f64_bytes = vec![DATA_FLOAT, 0x09, 0x11];
        f64_bytes.extend_from_slice(&12.5f64.to_be_bytes());
        assert!(matches!(
            read_value(&f64_bytes),
            MmsValue::Float64(f) if f == 12.5
        ));

        match read_value(&[DATA_BIT_STRING, 0x03, 0x06, 0xAA, 0x80]) {
            MmsValue::BitString { bytes, unused_bits } => {
                assert_eq!((bytes.as_slice(), unused_bits), (&[0xAA, 0x80][..], 6));
            },
            other => panic!("expected BitString, got {other:?}"),
        }

        assert!(matches!(
            read_value(&[DATA_OCTET_STRING, 0x03, 1, 2, 3]),
            MmsValue::OctetString(ref b) if b == &[1, 2, 3]
        ));
        assert!(matches!(
            read_value(&[DATA_VISIBLE_STRING, 0x03, b'a', b'b', b'c']),
            MmsValue::VisibleString(ref s) if s == "abc"
        ));
        assert!(matches!(
            read_value(&[DATA_MMS_STRING, 0x02, b'o', b'k']),
            MmsValue::VisibleString(ref s) if s == "ok"
        ));
        assert!(matches!(
            read_value(&[DATA_UTC_TIME, 0x08, 1, 2, 3, 4, 5, 6, 7, 8]),
            MmsValue::UtcTime([1, 2, 3, 4, 5, 6, 7, 8])
        ));
    }

    #[test]
    fn data_decoder_rejects_bad_lengths_and_unknown_tags() {
        for access_result in [
            vec![DATA_FLOAT, 0x03, 0x08, 0x41, 0x48], // float is 5 or 9 bytes
            vec![DATA_UTC_TIME, 0x07, 1, 2, 3, 4, 5, 6, 7], // UTC time is 8 bytes
            vec![DATA_INTEGER, 0x00],                 // zero-length integer
            tlv(0xA1, &[0x9E, 0x01, 0x00]),           // unknown Data tag
            tlv(0xA1, &[]),                           // empty Data
        ] {
            assert!(
                parse_read_response(&read_response(&access_result)).is_err(),
                "{access_result:02X?}"
            );
        }
    }

    #[test]
    fn simple_bool_write_targets_the_item_verbatim() {
        for (value, encoded) in [(true, 0x01u8), (false, 0x00)] {
            let pdu = build_write_simple_bool(3, "LD", "LLN0$BR$EventsBRCB01$GI", value);
            let (invoke, domain, item, data) = decode_write_request(&pdu);
            assert_eq!(invoke, 3);
            assert_eq!(domain, "LD");
            assert_eq!(item, "LLN0$BR$EventsBRCB01$GI");
            assert_eq!(data, vec![DATA_BOOLEAN, 0x01, encoded]);
        }
    }

    #[test]
    fn bool_control_write_targets_oper_with_the_full_oper_structure() {
        let pdu = build_write_bool_request(7, "LD", "GGIO1$CO$SPCSO1$Oper$ctlVal", true);
        let (invoke, domain, item, data) = decode_write_request(&pdu);
        assert_eq!((invoke, domain.as_str()), (7, "LD"));
        assert_eq!(item, "GGIO1$CO$SPCSO1$Oper", "write targets the Oper node");

        let (rest, oper) = parse_tlv(&data, DATA_STRUCTURE).expect("Oper structure");
        assert!(rest.is_empty());
        assert_eq!(oper.len(), 30);
        // ctlVal, origin{orCat=3, orIdent=""}, ctlNum=invokeID
        assert_eq!(
            &oper[..13],
            &[
                0x83, 0x01, 0x01, // ctlVal: TRUE
                0xA2, 0x05, 0x85, 0x01, 0x03, 0x89, 0x00, // origin
                0x86, 0x01, 0x07, // ctlNum = invoke id
            ]
        );
        // T (8-byte UtcTime, value is wall-clock), then Test, then Check.
        assert_eq!(&oper[13..15], &[0x91, 0x08]);
        assert_eq!(
            &oper[23..],
            &[0x83, 0x01, 0x00, 0x84, 0x02, 0x06, 0x00],
            "Test=FALSE and Check=no-checks close the Oper structure"
        );
    }

    #[test]
    fn f32_adjustment_write_strips_setmag_suffixes_and_encodes_ieee754() {
        let pdu = build_write_f32_request(9, "LD", "GGIO1$CO$AnOut1$Oper$setMag$f", 12.5);
        let (_, _, item, data) = decode_write_request(&pdu);
        assert_eq!(item, "GGIO1$CO$AnOut1$Oper");
        let (_, oper) = parse_tlv(&data, DATA_STRUCTURE).expect("Oper structure");
        assert_eq!(oper.len(), 36);
        assert_eq!(
            &oper[..9],
            &[0xA2, 0x07, 0x87, 0x05, 0x08, 0x41, 0x48, 0x00, 0x00],
            "setMag structure wraps the big-endian IEEE-754 value 12.5"
        );

        let (_, _, item, _) = decode_write_request(&build_write_f32_request(
            9,
            "LD",
            "GGIO1$CO$AnOut1$Oper$setMag",
            1.0,
        ));
        assert_eq!(item, "GGIO1$CO$AnOut1$Oper", "bare $setMag also strips");

        let (_, _, item, _) =
            decode_write_request(&build_write_f32_request(9, "LD", "GGIO1$SP$Volts", 1.0));
        assert_eq!(item, "GGIO1$SP$Volts", "unsuffixed items pass through");
    }

    #[test]
    fn sbo_select_reads_the_sbo_attribute() {
        assert_eq!(
            build_sbo_select_request(1, "LD", "GGIO1$CO$SPCSO1$Oper$ctlVal"),
            build_read_request(1, "LD", "GGIO1$CO$SPCSO1$SBO")
        );
        assert_eq!(
            build_sbo_select_request(1, "LD", "GGIO1$CO$SPCSO1$Oper"),
            build_read_request(1, "LD", "GGIO1$CO$SPCSO1$SBO")
        );
        assert_eq!(
            build_sbo_select_request(1, "LD", "SPCSO1"),
            build_read_request(1, "LD", "SPCSO1$SBO")
        );
    }

    #[test]
    fn sbo_select_response_maps_string_content_to_selection() {
        let selected = read_response(&[DATA_VISIBLE_STRING, 0x03, b'a', b'b', b'c']);
        assert!(parse_sbo_select_response(&selected).expect("selected"));

        let refused = read_response(&[DATA_VISIBLE_STRING, 0x00]);
        assert!(!parse_sbo_select_response(&refused).expect("refused"));

        let failure = read_response(&[0x80, 0x01, 0x0A]);
        assert!(parse_sbo_select_response(&failure).is_err());

        let wrong_type = read_response(&[DATA_INTEGER, 0x01, 0x01]);
        assert!(parse_sbo_select_response(&wrong_type).is_err());
    }

    #[test]
    fn sbow_select_writes_the_sbow_node() {
        let pdu = build_sbow_select_bool_request(4, "LD", "GGIO1$CO$SPCSO1$Oper$ctlVal", true);
        let (invoke, _, item, data) = decode_write_request(&pdu);
        assert_eq!(invoke, 4);
        assert_eq!(item, "GGIO1$CO$SPCSO1$SBOw");
        assert_eq!(data[0], DATA_STRUCTURE, "SBOw carries the Oper structure");
    }

    #[test]
    fn write_response_success_failure_and_malformed_paths() {
        assert_eq!(
            parse_write_response(&write_response(&[0x81, 0x00])).expect("success"),
            5
        );

        let error = parse_write_response(&write_response(&[0x80, 0x01, 0x07]))
            .expect_err("failure must not report success");
        assert!(
            error.to_string().contains("error code 7"),
            "unexpected error: {error}"
        );

        for body in [&[0x55, 0x00][..], &[]] {
            assert!(
                parse_write_response(&write_response(body)).is_err(),
                "{body:02X?}"
            );
        }
        // Missing Write-Response tag entirely.
        assert!(parse_write_response(&tlv(TAG_CONFIRMED_RESP, &[0x02, 0x01, 0x05])).is_err());
        // A request tag where the response should be.
        assert!(parse_write_response(&tlv(TAG_CONFIRMED_REQ, &[0x02, 0x01, 0x05])).is_err());
    }

    #[test]
    fn report_decoder_reads_the_inclusion_bitmap_and_values() {
        let mut items = tlv(DATA_VISIBLE_STRING, b"RptID");
        items.extend_from_slice(&[DATA_BIT_STRING, 0x03, 0x06, 0x00, 0x00]); // OptFlds: none
        items.extend_from_slice(&[DATA_BIT_STRING, 0x02, 0x04, 0xA0]); // include {0, 2} of 4
        items.extend_from_slice(&[DATA_BOOLEAN, 0x01, 0x01]);
        items.extend_from_slice(&[DATA_INTEGER, 0x01, 0x2A]);

        let report = parse_report(&report_pdu(&items)).expect("report");
        assert_eq!(report.rpt_id, "RptID");
        assert_eq!(report.timestamp_ms, None);
        assert_eq!(report.dataset_size, 4);
        assert_eq!(report.element_indices, vec![0, 2]);
        assert_eq!(report.values.len(), 2);
        assert!(matches!(report.values[0], MmsValue::Boolean(true)));
        assert!(matches!(report.values[1], MmsValue::Integer(42)));
    }

    #[test]
    fn report_decoder_skips_optional_fields_and_decodes_the_timestamp() {
        let mut items = tlv(DATA_VISIBLE_STRING, b"Events");
        // OptFlds bits {1: seqNum, 2: timestamp, 4: dataSetName, 5: data-refs}.
        items.extend_from_slice(&[DATA_BIT_STRING, 0x03, 0x06, 0x6C, 0x00]);
        items.extend_from_slice(&[DATA_UNSIGNED, 0x01, 0x07]); // seqNum
        // BinaryTime6: 3 600 000 ms of day 20 000 → 2024-10-04T01:00:00Z.
        items.extend_from_slice(&[DATA_BINARY_TIME, 0x06, 0x00, 0x36, 0xEE, 0x80, 0x4E, 0x20]);
        items.extend_from_slice(&tlv(DATA_VISIBLE_STRING, b"DS01")); // dataSetName
        items.extend_from_slice(&[DATA_BIT_STRING, 0x02, 0x06, 0x40]); // include {1} of 2
        items.extend_from_slice(&tlv(DATA_VISIBLE_STRING, b"ref")); // data-reference
        items.extend_from_slice(&[DATA_FLOAT, 0x05, 0x08, 0x41, 0x48, 0x00, 0x00]);

        let report = parse_report(&report_pdu(&items)).expect("report");
        assert_eq!(report.rpt_id, "Events");
        assert_eq!(report.timestamp_ms, Some(1_728_003_600_000));
        assert_eq!(report.dataset_size, 2);
        assert_eq!(report.element_indices, vec![1]);
        assert!(matches!(report.values[0], MmsValue::Float32(f) if f == 12.5));
    }

    #[test]
    fn report_decoder_rejects_malformed_pdus() {
        let mut valid_items = tlv(DATA_VISIBLE_STRING, b"RptID");
        valid_items.extend_from_slice(&[DATA_BIT_STRING, 0x03, 0x06, 0x00, 0x00]);
        valid_items.extend_from_slice(&[DATA_BIT_STRING, 0x02, 0x06, 0xC0]); // include {0, 1}
        valid_items.extend_from_slice(&[DATA_BOOLEAN, 0x01, 0x01]);
        valid_items.extend_from_slice(&[DATA_BOOLEAN, 0x01, 0x00]);
        let valid = report_pdu(&valid_items);
        assert!(parse_report(&valid).is_some(), "baseline report must parse");

        let mut wrong_outer = valid.clone();
        wrong_outer[0] = 0xA4;
        assert!(parse_report(&wrong_outer).is_none(), "wrong outer tag");
        assert!(
            parse_report(&valid[..valid.len() - 1]).is_none(),
            "truncated PDU"
        );

        let rpt_id_not_string = report_pdu(&[DATA_INTEGER, 0x01, 0x01]);
        assert!(parse_report(&rpt_id_not_string).is_none());

        let mut optflds_not_bitstring = tlv(DATA_VISIBLE_STRING, b"RptID");
        optflds_not_bitstring.extend_from_slice(&[DATA_INTEGER, 0x01, 0x00]);
        assert!(parse_report(&report_pdu(&optflds_not_bitstring)).is_none());

        let mut missing_inclusion = tlv(DATA_VISIBLE_STRING, b"RptID");
        missing_inclusion.extend_from_slice(&[DATA_BIT_STRING, 0x03, 0x06, 0x00, 0x00]);
        assert!(parse_report(&report_pdu(&missing_inclusion)).is_none());

        let mut missing_value = tlv(DATA_VISIBLE_STRING, b"RptID");
        missing_value.extend_from_slice(&[DATA_BIT_STRING, 0x03, 0x06, 0x00, 0x00]);
        missing_value.extend_from_slice(&[DATA_BIT_STRING, 0x02, 0x06, 0xC0]); // 2 included
        missing_value.extend_from_slice(&[DATA_BOOLEAN, 0x01, 0x01]); // only 1 value
        assert!(parse_report(&report_pdu(&missing_value)).is_none());
    }

    #[test]
    fn report_item_walker_consumes_structures_and_unknown_tags_without_values() {
        assert!(parse_data_item(&[]).is_none(), "empty buffer");
        assert!(
            parse_data_item(&[DATA_BOOLEAN, 0x05, 0x01]).is_none(),
            "declared length beyond buffer must not be consumed"
        );

        let (consumed, value) =
            parse_data_item(&[DATA_STRUCTURE, 0x03, 0x85, 0x01, 0x01, 0x83]).expect("structure");
        assert_eq!((consumed, value.is_none()), (5, true));

        let (consumed, value) = parse_data_item(&[0x9E, 0x01, 0x00]).expect("unknown tag");
        assert_eq!((consumed, value.is_none()), (3, true));

        let (consumed, value) =
            parse_data_item(&[DATA_BINARY_TIME, 0x06, 0, 0, 0, 0, 0, 1]).expect("binary time");
        assert_eq!(consumed, 8);
        assert!(matches!(value, Some(MmsValue::OctetString(ref b)) if b.len() == 6));
    }
}
