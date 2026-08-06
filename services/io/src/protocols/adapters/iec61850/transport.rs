//! TPKT / COTP / ISO Session / ISO Presentation framing for IEC 61850 MMS.
//!
//! This module implements the five-layer framing stack required by IEC 61850:
//!
//! ```text
//! TCP (port 102)
//! └── TPKT (RFC 1006)          – 4-byte header: 03 00 [len16]
//!     └── COTP (ISO 8073)      – Connection Request / Data TPDU
//!         └── ISO Session      – CONNECT / Data SPDU
//!             └── ISO Presentation – CP / Data PDU
//!                 └── ACSE / MMS PDU
//! ```
//!
//! All byte sequences are derived from empirical analysis of libiec61850 traffic;
//! no C source code has been copied.

use std::io;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::protocols::core::error::{GatewayError, Result};

// ── COTP constants ────────────────────────────────────────────────────────────

/// COTP Connection Request PDU type.
const COTP_CR: u8 = 0xE0;
/// COTP Connection Confirm PDU type.
const COTP_CC: u8 = 0xD0;
/// TPKT version byte.
const TPKT_VER: u8 = 0x03;

/// Maximum receive buffer size (65 KB, slightly above max TPKT length).
const MAX_PDU_SIZE: usize = 65_536;

// ── MMS Initiate PDU ─────────────────────────────────────────────────────────
//
// The connection-setup PDU is a fixed sequence (Session CONNECT + Presentation CP
// + ACSE AARQ + MMS Initiate-Request) derived by analysing libiec61850 wire captures.
// The values reflect: maxPduSize=65000, maxOutstanding=5, nestingLevel=10.

/// Pre-built MMS connection-setup COTP payload (everything after the COTP DT header).
///
/// Layout (from outer to inner):
/// ```text
/// [Session CONNECT SPDU] [Presentation CP PDU] [ACSE AARQ] [MMS InitiateRequest]
/// ```
static MMS_CONNECT_PAYLOAD: &[u8] = &[
    // ── Session CONNECT SPDU ─────────────────────────────────────────────────
    0x0D, 0x95, // SPDU type=13 (CONNECT), param-length=149
    0x05, 0x06, 0x13, 0x01, 0x00, 0x16, 0x01,
    0x02, // Connect Accept Item (options=0, version=2)
    0x14, 0x02, 0x00, 0x02, // Session Requirement: duplex (0x0002)
    0x33, 0x02, 0x00, 0x01, // Calling Session Selector: {0x00, 0x01}
    0x34, 0x02, 0x00, 0x01, // Called Session Selector: {0x00, 0x01}
    0xC1, 0x7F, // User Data PGI, length=127 (Presentation PDU follows)
    // ── Presentation CP PDU ──────────────────────────────────────────────────
    0x31, 0x7D, // SET, length=125
    0xA0, 0x03, 0x80, 0x01, 0x01, // mode-selector: normal (1)
    0xA2, 0x76, // normal-mode-parameters, length=118
    0x81, 0x04, 0x00, 0x00, 0x00, 0x01, // calling-presentation-selector
    0x82, 0x04, 0x00, 0x00, 0x00, 0x01, // called-presentation-selector
    // presentation-context-id-list (A4, length=35)
    0xA4, 0x23, 0x30, 0x0F, // ACSE context (id=1)
    0x02, 0x01, 0x01, 0x06, 0x04, 0x52, 0x01, 0x00, 0x01, // ACSE abstract syntax OID
    0x30, 0x04, 0x06, 0x02, 0x51, 0x01, // BER transfer syntax
    0x30, 0x10, // MMS context (id=3)
    0x02, 0x01, 0x03, 0x06, 0x05, 0x28, 0xCA, 0x22, 0x02, 0x01, // MMS abstract syntax OID
    0x30, 0x04, 0x06, 0x02, 0x51, 0x01, // BER transfer syntax
    // fully-encoded-data (user data)
    0x61, 0x43, 0x30, 0x41, 0x02, 0x01, 0x03, // context-id=3 (MMS)
    0xA0, 0x3C, // single-ASN1-type, length=60 (ACSE AARQ follows)
    // ── ACSE AARQ ────────────────────────────────────────────────────────────
    0x60, 0x3A, // AARQ, length=58
    0xA1, 0x07, 0x06, 0x05, 0x28, 0xCA, 0x22, 0x02, 0x03, // app-context: MMS
    0xBE, 0x2F, // user-information, length=47
    0x28, 0x2D, // association-data, length=45
    0x02, 0x01, 0x03, // indirect-reference=3
    0xA0, 0x28, // encoding, length=40 (MMS Initiate-Request follows)
    // ── MMS Initiate-Request ─────────────────────────────────────────────────
    0xA8, 0x26, // InitiateRequestPDU, length=38
    0x80, 0x03, 0x00, 0xFD, 0xE8, // localDetail: 65000
    0x81, 0x01, 0x05, // proposedMaxServerOutstandingCalling: 5
    0x82, 0x01, 0x05, // proposedMaxServerOutstandingCalled: 5
    0x83, 0x01, 0x0A, // proposedDataStructureNestingLevel: 10
    0xA4, 0x16, // initRequestDetail, length=22
    0x80, 0x01, 0x01, // proposedVersionNumber: 1
    0x81, 0x03, 0x05, 0xF1, 0x00, // proposedParameterCBB
    // servicesSupportedCalling (bit string, 85 bits, 3 unused)
    0x82, 0x0C, 0x03, 0xEE, 0x1C, 0x00, 0x00, 0x04, 0x08, 0x00, 0x00, 0x79, 0xEF, 0x18,
];

// ── Session / Presentation data-PDU prefix ────────────────────────────────────
//
// After the connection is established, every MMS PDU is wrapped:
//   [Session DT: 01 00 01 00] [Presentation: 61 xx 30 xx 02 01 03 A0 xx] [MMS PDU]

/// Build the COTP DT + Session DT + Presentation user-data wrapper for an MMS PDU.
pub fn wrap_data_pdu(mms_pdu: &[u8]) -> Vec<u8> {
    let payload_len = mms_pdu.len();
    // Presentation encoding:
    //   presentation-selector: 02 01 03  (3 bytes)
    //   A0 encoding:           A0 + len  (2 bytes) + payload
    let pdv_content_len = 3 + 2 + payload_len; // 5 + payload
    let pdv_seq_len = pdv_content_len; // content of 30
    let fed_content_len = 2 + pdv_seq_len; // 30 hdr + content
    let fed_total_len = fed_content_len; // content of 61

    // Session Data SPDU: 01 00 01 00  (4 bytes fixed)
    let session_len = 4;

    // Presentation:  61 [len] 30 [len] 02 01 03 A0 [len]
    let pres_hdr = pres_data_header(payload_len);

    // COTP DT:  02 F0 80  (3 bytes fixed)
    let total = 3 + session_len + pres_hdr.len() + payload_len;
    let mut buf = Vec::with_capacity(4 + total); // 4 = TPKT header

    // TPKT header
    let frame_len = (4 + total) as u16;
    buf.push(TPKT_VER);
    buf.push(0x00);
    buf.push((frame_len >> 8) as u8);
    buf.push(frame_len as u8);

    // COTP DT
    buf.extend_from_slice(&[0x02, 0xF0, 0x80]);

    // Session Data SPDU
    buf.extend_from_slice(&[0x01, 0x00, 0x01, 0x00]);

    // Presentation headers
    buf.extend_from_slice(&pres_hdr);

    // MMS PDU
    buf.extend_from_slice(mms_pdu);

    let _ = (fed_total_len, fed_content_len, pdv_content_len, pdv_seq_len);

    buf
}

/// Build the Presentation fully-encoded-data header for a data PDU.
fn pres_data_header(payload_len: usize) -> Vec<u8> {
    // PDV-list content: 02 01 03 + A0 [plen]
    let pdv_inner = 3 + ber_len_size(payload_len) + 1 + payload_len; // 02 01 03 A0 [l] [payload]
    let pdv_content = 3 + ber_len_size(payload_len) + 1; // 02 01 03 A0 [l]  (without payload)

    let seq_len = pdv_content + payload_len; // content of 30 tag
    let seq_content = pdv_content; // header part

    let fed_len = 1 + ber_len_size(seq_len) + seq_len; // 30 [l] [content+payload]
    let fed_content = 1 + ber_len_size(seq_len) + seq_content; // header part of 30

    let _ = (pdv_inner, fed_content);

    let outer_len = 1 + ber_len_size(fed_len) + fed_len; // 61 [l] [...]
    let _ = outer_len;

    // Write actual header bytes
    let mut h = Vec::new();

    // 61 [fed_len]
    h.push(0x61);
    push_ber_len(&mut h, fed_len);

    // 30 [seq_len]
    h.push(0x30);
    push_ber_len(&mut h, seq_len);

    // 02 01 03  (context-id = 3 = MMS)
    h.extend_from_slice(&[0x02, 0x01, 0x03]);

    // A0 [payload_len]
    h.push(0xA0);
    push_ber_len(&mut h, payload_len);

    h
}

/// Extract the raw MMS PDU from a received data TPKT payload.
///
/// Strips: COTP DT (3 bytes) + Session DT (4 bytes) + Presentation headers.
///
/// Returns `None` if the buffer is too short or malformed.
pub fn unwrap_data_pdu(cotp_payload: &[u8]) -> Option<&[u8]> {
    // Skip COTP DT: 02 F0 80
    let p = cotp_payload.strip_prefix(&[0x02, 0xF0, 0x80])?;

    // Skip Session DT: 01 00 01 00
    let p = p.strip_prefix(&[0x01, 0x00, 0x01, 0x00])?;

    // Parse Presentation 61 → descend into its content
    let (_, fed_content) = parse_tlv(p, 0x61)?; // fully-encoded-data content

    // Parse 30 → descend into its content
    let (_, pdv_content) = parse_tlv(fed_content, 0x30)?; // PDV-list content

    // Skip context-id: 02 01 03
    let p = pdv_content.strip_prefix(&[0x02, 0x01, 0x03])?;

    // A0 [len] → MMS PDU is the content of this A0
    let (_, mms) = parse_tlv(p, 0xA0)?;
    Some(mms)
}

// ── Framer ────────────────────────────────────────────────────────────────────

/// TCP stream wrapped with TPKT/COTP/Session/Presentation framing.
pub struct Framer {
    stream: TcpStream,
    /// Unconfirmed MMS PDUs (tag 0xA3, i.e. reports) received out-of-band
    /// while waiting for a confirmed response.  Callers drain this with
    /// [`Framer::take_pending_reports`] or [`Framer::drain_socket`].
    pending_reports: Vec<Vec<u8>>,
}

impl Framer {
    pub fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            pending_reports: Vec::new(),
        }
    }

    /// Send a COTP Connection Request and wait for the Connection Confirm.
    pub async fn handshake_cotp(&mut self) -> Result<()> {
        // COTP CR: LI=17, type=CR, dst-ref=0, src-ref=1, class=0,
        //          options: TPDU-size=1024 (0x0A), calling-TSAP={0,1}, called-TSAP={0,1}
        let cr_payload: &[u8] = &[
            0x11, COTP_CR, 0x00, 0x00, 0x00, 0x01, 0x00, 0xC0, 0x01, 0x0A, // TPDU size = 1024
            0xC1, 0x02, 0x00, 0x01, // calling TSAP
            0xC2, 0x02, 0x00, 0x01, // called TSAP
        ];
        self.send_tpkt(cr_payload).await?;

        let cc_buf = self.recv_tpkt().await?;
        if cc_buf.len() < 2 || cc_buf[1] != COTP_CC {
            return Err(GatewayError::Protocol(format!(
                "IEC 61850: expected COTP CC (0x{:02X}), got 0x{:02X}",
                COTP_CC,
                cc_buf.get(1).copied().unwrap_or(0)
            )));
        }
        Ok(())
    }

    /// Send the MMS connection-setup PDU and wait for the Initiate-Response.
    pub async fn handshake_mms(&mut self) -> Result<()> {
        // COTP DT header: 02 F0 80
        let mut payload = Vec::with_capacity(3 + MMS_CONNECT_PAYLOAD.len());
        payload.extend_from_slice(&[0x02, 0xF0, 0x80]);
        payload.extend_from_slice(MMS_CONNECT_PAYLOAD);

        self.send_tpkt(&payload).await?;

        let resp = self.recv_tpkt().await?;
        // Minimal validation: response should start with COTP DT
        if resp.len() < 3 || resp[0] != 0x02 || resp[1] != 0xF0 {
            return Err(GatewayError::Protocol(
                "IEC 61850: invalid MMS Initiate response".into(),
            ));
        }
        Ok(())
    }

    /// Send an MMS PDU wrapped in the full framing stack.
    pub async fn send_mms(&mut self, mms_pdu: &[u8]) -> Result<()> {
        let frame = wrap_data_pdu(mms_pdu);
        self.stream.write_all(&frame).await.map_err(io_err)?;
        Ok(())
    }

    /// Receive one TPKT and return the raw MMS PDU (unwrapped from all framing layers).
    ///
    /// Unconfirmed PDUs (tag `0xA3`, i.e. IEC 61850 Reports) are **silently**
    /// buffered into `self.pending_reports` and the loop continues until a
    /// confirmed response (or error) is returned.  This means callers never
    /// see spurious report PDUs mixed in with their expected responses.
    pub async fn recv_mms(&mut self) -> Result<Vec<u8>> {
        loop {
            let pdu = self.recv_mms_raw().await?;
            if pdu.first() == Some(&0xA3) {
                self.pending_reports.push(pdu);
            } else {
                return Ok(pdu);
            }
        }
    }

    /// Return all buffered unconfirmed PDUs and clear the buffer.
    pub fn take_pending_reports(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.pending_reports)
    }

    /// Actively drain any additional unconfirmed PDUs already sitting in the
    /// TCP receive buffer.  Uses a very short per-read timeout so it returns
    /// quickly when no more data is waiting.
    ///
    /// Should be called at the *start* of a poll cycle to pick up reports that
    /// arrived while the channel was idle between cycles.
    pub async fn drain_socket(&mut self) -> Vec<Vec<u8>> {
        // First return everything already in the pending buffer.
        let mut out = std::mem::take(&mut self.pending_reports);

        // Then try to read more frames from the socket with a very short
        // timeout.  We stop as soon as the socket has no data ready.
        // This is safe because recv_mms_raw uses read_exact: if it starts
        // reading a TPKT header we will finish reading the full frame before
        // the timeout fires (the data is already in the OS buffer).
        loop {
            match tokio::time::timeout(std::time::Duration::from_millis(20), self.recv_mms_raw())
                .await
            {
                Ok(Ok(pdu)) if pdu.first() == Some(&0xA3) => out.push(pdu),
                Ok(Ok(_)) => break, // unexpected confirmed PDU between cycles, discard
                _ => break,         // timeout or IO error → no more data
            }
        }
        out
    }

    /// Low-level: receive exactly one TPKT and unwrap it to a raw MMS PDU.
    async fn recv_mms_raw(&mut self) -> Result<Vec<u8>> {
        let cotp_payload = self.recv_tpkt().await?;
        let mms = unwrap_data_pdu(&cotp_payload).ok_or_else(|| {
            GatewayError::Protocol("IEC 61850: failed to unwrap MMS PDU from data frame".into())
        })?;
        Ok(mms.to_vec())
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    async fn send_tpkt(&mut self, payload: &[u8]) -> Result<()> {
        let total = 4 + payload.len();
        let mut buf = Vec::with_capacity(total);
        buf.push(TPKT_VER);
        buf.push(0x00);
        buf.push(((total >> 8) & 0xFF) as u8);
        buf.push((total & 0xFF) as u8);
        buf.extend_from_slice(payload);
        self.stream.write_all(&buf).await.map_err(io_err)?;
        Ok(())
    }

    async fn recv_tpkt(&mut self) -> Result<Vec<u8>> {
        let mut hdr = [0u8; 4];
        self.stream.read_exact(&mut hdr).await.map_err(io_err)?;

        if hdr[0] != TPKT_VER {
            return Err(GatewayError::Protocol(format!(
                "IEC 61850: invalid TPKT version byte 0x{:02X}",
                hdr[0]
            )));
        }
        let total_len = u16::from_be_bytes([hdr[2], hdr[3]]) as usize;
        if !(4..=MAX_PDU_SIZE).contains(&total_len) {
            return Err(GatewayError::Protocol(format!(
                "IEC 61850: TPKT length {} out of range",
                total_len
            )));
        }
        let payload_len = total_len - 4;
        let mut payload = vec![0u8; payload_len];
        self.stream.read_exact(&mut payload).await.map_err(io_err)?;
        Ok(payload)
    }
}

// ── BER helpers ───────────────────────────────────────────────────────────────

/// Number of bytes required to encode `len` in BER length form.
fn ber_len_size(len: usize) -> usize {
    if len < 0x80 {
        1
    } else if len < 0x100 {
        2
    } else {
        3
    }
}

/// Append a BER length to `buf`.
pub fn push_ber_len(buf: &mut Vec<u8>, len: usize) {
    if len < 0x80 {
        buf.push(len as u8);
    } else if len < 0x100 {
        buf.push(0x81);
        buf.push(len as u8);
    } else {
        buf.push(0x82);
        buf.push((len >> 8) as u8);
        buf.push(len as u8);
    }
}

/// Parse a BER TLV with the expected `tag` and return `(rest, content)`.
pub fn parse_tlv(buf: &[u8], tag: u8) -> Option<(&[u8], &[u8])> {
    if buf.is_empty() || buf[0] != tag {
        return None;
    }
    let (len, hdr_size) = parse_ber_len(&buf[1..])?;
    let start = 1 + hdr_size;
    let end = start + len;
    if end > buf.len() {
        return None;
    }
    Some((&buf[end..], &buf[start..end]))
}

/// Parse a BER length field, returning `(length, bytes_consumed)`.
pub fn parse_ber_len(buf: &[u8]) -> Option<(usize, usize)> {
    if buf.is_empty() {
        return None;
    }
    let first = buf[0];
    if first < 0x80 {
        Some((first as usize, 1))
    } else if first == 0x81 {
        if buf.len() < 2 {
            return None;
        }
        Some((buf[1] as usize, 2))
    } else if first == 0x82 {
        if buf.len() < 3 {
            return None;
        }
        Some(((buf[1] as usize) << 8 | buf[2] as usize, 3))
    } else {
        None
    }
}

fn io_err(e: io::Error) -> GatewayError {
    GatewayError::Io(e)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::net::TcpListener;

    use super::*;

    /// Build a raw TPKT frame around `payload` (server-side test helper).
    fn tpkt(payload: &[u8]) -> Vec<u8> {
        let total = 4 + payload.len();
        let mut buf = vec![TPKT_VER, 0x00, (total >> 8) as u8, total as u8];
        buf.extend_from_slice(payload);
        buf
    }

    /// Connect a Framer to an in-test TCP peer and return both ends.
    async fn framer_pair() -> (Framer, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("local address");
        let client = TcpStream::connect(address).await.expect("connect");
        let (server, _) = listener.accept().await.expect("accept");
        (Framer::new(client), server)
    }

    #[test]
    fn wrap_unwrap_roundtrip() {
        let mms = b"\xA0\x03\x02\x01\x01";
        let frame = wrap_data_pdu(mms);

        // Strip TPKT header (4 bytes) to get COTP payload
        let cotp_payload = &frame[4..];
        let recovered = unwrap_data_pdu(cotp_payload).unwrap();
        assert_eq!(recovered, mms);
    }

    #[test]
    fn mms_connect_payload_length() {
        // COTP DT (3) + MMS_CONNECT_PAYLOAD = total COTP payload
        // Session header: 24 bytes, Presentation header: 67 bytes, ACSE: 20 bytes, MMS: 40 bytes
        assert_eq!(MMS_CONNECT_PAYLOAD.len(), 151); // 24 + 67 + 20 + 40
    }

    #[test]
    fn ber_len_roundtrips_one_two_and_three_byte_forms() {
        for len in [0usize, 1, 0x7F, 0x80, 0xFF, 0x100, 0x1234, 0xFFFF] {
            let mut buf = Vec::new();
            push_ber_len(&mut buf, len);
            assert_eq!(buf.len(), ber_len_size(len), "encoded size for {len}");
            assert_eq!(
                parse_ber_len(&buf),
                Some((len, buf.len())),
                "roundtrip for {len}"
            );
        }
    }

    #[test]
    fn parse_ber_len_rejects_empty_truncated_and_unsupported_forms() {
        for buf in [
            &[][..],
            &[0x81],              // 2-byte form missing its length byte
            &[0x82],              // 3-byte form missing both length bytes
            &[0x82, 0x01],        // 3-byte form missing one length byte
            &[0x80],              // indefinite length is not supported
            &[0x83, 0, 0, 1][..], // 4-byte form is not supported
        ] {
            assert_eq!(parse_ber_len(buf), None, "{buf:02X?}");
        }
    }

    #[test]
    fn parse_tlv_returns_content_and_rest_and_rejects_wrong_tag_or_truncation() {
        let buf = [0x30, 0x02, 0xAA, 0xBB, 0xCC];
        let (rest, content) = parse_tlv(&buf, 0x30).expect("valid TLV");
        assert_eq!(content, &[0xAA, 0xBB]);
        assert_eq!(rest, &[0xCC]);

        assert_eq!(parse_tlv(&buf, 0x31), None, "wrong tag");
        assert_eq!(parse_tlv(&[], 0x30), None, "empty buffer");
        assert_eq!(
            parse_tlv(&[0x30, 0x05, 0x01], 0x30),
            None,
            "declared length beyond buffer"
        );
        assert_eq!(parse_tlv(&[0x30], 0x30), None, "missing length byte");
    }

    #[test]
    fn wrap_data_pdu_emits_tpkt_cotp_session_and_presentation_layers() {
        let mms = [0xA0, 0x03, 0x02, 0x01, 0x2A];
        let frame = wrap_data_pdu(&mms);

        // TPKT: version 3, reserved 0, big-endian total length == frame length.
        assert_eq!(frame[0], TPKT_VER);
        assert_eq!(frame[1], 0x00);
        assert_eq!(
            u16::from_be_bytes([frame[2], frame[3]]) as usize,
            frame.len()
        );
        // COTP DT and Session DT are fixed byte sequences.
        assert_eq!(&frame[4..7], &[0x02, 0xF0, 0x80]);
        assert_eq!(&frame[7..11], &[0x01, 0x00, 0x01, 0x00]);
        // The MMS PDU is carried verbatim at the tail.
        assert!(frame.ends_with(&mms));
    }

    #[test]
    fn wrap_unwrap_roundtrip_spans_ber_length_boundaries() {
        // Sizes chosen to cross the 1-byte/2-byte/3-byte BER length borders in
        // every presentation-layer header.
        for size in [
            0usize, 1, 120, 121, 122, 127, 128, 200, 255, 256, 1000, 4000,
        ] {
            let mms = vec![0x5A; size];
            let frame = wrap_data_pdu(&mms);
            assert_eq!(
                u16::from_be_bytes([frame[2], frame[3]]) as usize,
                frame.len(),
                "TPKT length for payload size {size}"
            );
            let recovered = unwrap_data_pdu(&frame[4..])
                .unwrap_or_else(|| panic!("unwrap failed for payload size {size}"));
            assert_eq!(recovered, &mms[..], "payload size {size}");
        }
    }

    #[test]
    fn unwrap_data_pdu_rejects_missing_or_malformed_layers() {
        let good: Vec<u8> = wrap_data_pdu(&[0xA0, 0x03, 0x02, 0x01, 0x01])[4..].to_vec();
        assert!(
            unwrap_data_pdu(&good).is_some(),
            "baseline frame must parse"
        );

        assert_eq!(unwrap_data_pdu(&[]), None, "empty buffer");

        let mut bad_cotp = good.clone();
        bad_cotp[0] = 0x03;
        assert_eq!(unwrap_data_pdu(&bad_cotp), None, "wrong COTP prefix");

        let mut bad_session = good.clone();
        bad_session[3] = 0xFF;
        assert_eq!(unwrap_data_pdu(&bad_session), None, "wrong session prefix");

        let mut bad_pres = good.clone();
        bad_pres[7] = 0x62;
        assert_eq!(unwrap_data_pdu(&bad_pres), None, "wrong presentation tag");

        let mut bad_context = good.clone();
        bad_context[13] = 0x04;
        assert_eq!(unwrap_data_pdu(&bad_context), None, "wrong context id");

        assert_eq!(
            unwrap_data_pdu(&good[..good.len() - 1]),
            None,
            "truncated frame"
        );
    }

    #[tokio::test]
    async fn handshake_cotp_completes_on_connection_confirm() {
        let (mut framer, mut server) = framer_pair().await;
        // Pre-write the CC so the exchange needs no task coordination: the CR
        // fits in the socket buffer and the reply is already queued.
        server
            .write_all(&tpkt(&[0x05, COTP_CC, 0x00, 0x00, 0x00, 0x00]))
            .await
            .expect("connection confirm");
        framer.handshake_cotp().await.expect("handshake");
    }

    #[tokio::test]
    async fn handshake_cotp_rejects_non_confirm_tpdu() {
        let (mut framer, mut server) = framer_pair().await;
        server
            .write_all(&tpkt(&[0x05, COTP_CR, 0x00, 0x00, 0x00, 0x00]))
            .await
            .expect("bogus reply");
        let error = framer.handshake_cotp().await.expect_err("must refuse CR");
        assert!(
            error.to_string().contains("expected COTP CC"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn recv_mms_rejects_bad_tpkt_version() {
        let (mut framer, mut server) = framer_pair().await;
        server
            .write_all(&[0x02, 0x00, 0x00, 0x08, 1, 2, 3, 4])
            .await
            .expect("bad frame");
        let error = framer.recv_mms().await.expect_err("must refuse version 2");
        assert!(
            error.to_string().contains("TPKT version"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn recv_mms_rejects_out_of_range_tpkt_length() {
        for header in [[0x03, 0x00, 0x00, 0x00], [0x03, 0x00, 0x00, 0x03]] {
            let (mut framer, mut server) = framer_pair().await;
            server.write_all(&header).await.expect("bad frame");
            let error = framer.recv_mms().await.expect_err("must refuse length");
            assert!(
                error.to_string().contains("out of range"),
                "unexpected error for {header:02X?}: {error}"
            );
        }
    }

    #[tokio::test]
    async fn recv_mms_errors_when_frame_is_not_a_data_pdu() {
        let (mut framer, mut server) = framer_pair().await;
        // Valid TPKT + COTP DT, but garbage where the session layer belongs.
        server
            .write_all(&tpkt(&[0x02, 0xF0, 0x80, 0xFF, 0xFF]))
            .await
            .expect("garbage frame");
        let error = framer.recv_mms().await.expect_err("must refuse frame");
        assert!(
            error.to_string().contains("failed to unwrap"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn recv_mms_buffers_reports_and_returns_the_confirmed_response() {
        let (mut framer, mut server) = framer_pair().await;
        let report: &[u8] = &[0xA3, 0x03, 0xA0, 0x01, 0x00];
        let response: &[u8] = &[0xA1, 0x03, 0x02, 0x01, 0x07];
        server
            .write_all(&wrap_data_pdu(report))
            .await
            .expect("report");
        server
            .write_all(&wrap_data_pdu(response))
            .await
            .expect("response");

        let got = framer.recv_mms().await.expect("confirmed response");
        assert_eq!(got, response, "reports must never mask responses");
        assert_eq!(framer.take_pending_reports(), vec![report.to_vec()]);
        assert!(
            framer.take_pending_reports().is_empty(),
            "take must drain the buffer"
        );
    }

    #[tokio::test]
    async fn drain_socket_returns_pending_then_socket_reports_and_stops() {
        let (mut framer, mut server) = framer_pair().await;
        let seeded = vec![0xA3, 0x01, 0x00];
        framer.pending_reports.push(seeded.clone());

        let wire: &[u8] = &[0xA3, 0x03, 0xA0, 0x01, 0x01];
        server
            .write_all(&wrap_data_pdu(wire))
            .await
            .expect("report");
        // Let the loopback deliver before the 20 ms per-read window starts.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let pdus = framer.drain_socket().await;
        assert_eq!(pdus, vec![seeded, wire.to_vec()]);
        assert!(
            framer.drain_socket().await.is_empty(),
            "second drain must find nothing"
        );
    }

    #[tokio::test]
    async fn drain_socket_discards_confirmed_pdus_between_cycles() {
        let (mut framer, mut server) = framer_pair().await;
        server
            .write_all(&wrap_data_pdu(&[0xA1, 0x03, 0x02, 0x01, 0x01]))
            .await
            .expect("stray confirmed PDU");
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert!(
            framer.drain_socket().await.is_empty(),
            "a stray confirmed PDU between cycles is discarded, not reported"
        );
    }
}
