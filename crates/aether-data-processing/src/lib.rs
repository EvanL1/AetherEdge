//! Strict Data Processing wire codec.
//!
//! Aether owns frame assembly. This crate only converts complete domain
//! requests, untrusted processor results, and Aether-accepted derived data at
//! a single JSON boundary. Derived data is deliberately encode-only, so an
//! external payload cannot claim Aether acceptance. This crate contains no
//! source callback, storage access, or model execution API.

mod codec;
mod dto;

pub use codec::{
    CodecError, compute_input_digest, decode_request, decode_result, encode_derived_data,
    encode_request, encode_result, validate_commissioned_route_contract, validate_task_contract,
};
pub use dto::{DataProcessingRequestDto, DerivedDataDto, ProcessingResultDto};

/// Vendor media type used by processor HTTP adapters.
pub const MEDIA_TYPE: &str = "application/vnd.aether.data-processing+json";

/// Complete-frame schema identifier.
pub const FRAME_SCHEMA: &str = "aether.processing-frame";

/// Processor-request schema identifier.
pub const REQUEST_SCHEMA: &str = "aether.data-processing.request";

/// Processor-result schema identifier.
pub const RESULT_SCHEMA: &str = "aether.data-processing.result";

/// Aether-accepted derived-data schema identifier.
pub const DERIVED_DATA_SCHEMA: &str = "aether.derived-data";

/// Typed forecast-output schema identifier.
pub const FORECAST_OUTPUT_SCHEMA: &str = "aether.data-processing.output.forecast";
