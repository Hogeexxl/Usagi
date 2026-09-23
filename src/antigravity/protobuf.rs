//! Bounded protobuf wire reader and parser for Antigravity records.

use std::fmt;

pub const MAX_PROTO_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_LENGTH_DELIMITED_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_PROTO_NESTING_DEPTH: usize = 32;
pub const SQLITE_BLOB_DIGEST_CHUNK_BYTES: usize = 64 * 1024;
pub const MAX_RESPONSE_ID_BYTES: usize = 1024;
pub const MAX_MODEL_BYTES: usize = 1024;
pub const MAX_TITLE_BYTES: usize = 4096;

/// Wire types in protocol buffers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WireType {
    Varint = 0,
    Fixed64 = 1,
    LengthDelimited = 2,
    Fixed32 = 5,
}

impl WireType {
    pub fn from_u32(val: u32) -> Option<Self> {
        match val {
            0 => Some(Self::Varint),
            1 => Some(Self::Fixed64),
            2 => Some(Self::LengthDelimited),
            5 => Some(Self::Fixed32),
            _ => None,
        }
    }
}

/// Errors occurring during protobuf parsing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProtoError {
    UnexpectedEof,
    VarintOverflow,
    InvalidWireType(u32),
    OversizedLength(usize),
    OversizedMessage(usize),
    NestingDepthExceeded(usize),
    InvalidUtf8,
    StringTooLong(usize),
    InvalidModelSlug,
    InvalidTimestamp,
}

impl fmt::Display for ProtoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEof => formatter.write_str("unexpected EOF in protobuf payload"),
            Self::VarintOverflow => {
                formatter.write_str("varint exceeds 64 bits in protobuf payload")
            }
            Self::InvalidWireType(wt) => write!(formatter, "invalid protobuf wire type: {wt}"),
            Self::OversizedLength(len) => {
                write!(formatter, "length-delimited field exceeds limit: {len}")
            }
            Self::OversizedMessage(len) => {
                write!(formatter, "protobuf message exceeds limit: {len}")
            }
            Self::NestingDepthExceeded(depth) => {
                write!(formatter, "protobuf nesting depth {depth} exceeds limit")
            }
            Self::InvalidUtf8 => formatter.write_str("invalid UTF-8 in string field"),
            Self::StringTooLong(len) => {
                write!(formatter, "string exceeds maximum allowed bytes: {len}")
            }
            Self::InvalidModelSlug => formatter.write_str("invalid selected model slug"),
            Self::InvalidTimestamp => formatter.write_str("invalid timestamp in step metadata"),
        }
    }
}

impl std::error::Error for ProtoError {}

/// A bounded reader cursor over a protobuf byte slice.
pub struct ProtoReader<'a> {
    buffer: &'a [u8],
    offset: usize,
    depth: usize,
}

impl<'a> ProtoReader<'a> {
    pub fn new(buffer: &'a [u8]) -> Result<Self, ProtoError> {
        if buffer.len() > MAX_PROTO_MESSAGE_BYTES {
            return Err(ProtoError::OversizedMessage(buffer.len()));
        }
        Ok(Self {
            buffer,
            offset: 0,
            depth: 0,
        })
    }

    pub fn with_depth(buffer: &'a [u8], depth: usize) -> Result<Self, ProtoError> {
        if depth > MAX_PROTO_NESTING_DEPTH {
            return Err(ProtoError::NestingDepthExceeded(depth));
        }
        if buffer.len() > MAX_PROTO_MESSAGE_BYTES {
            return Err(ProtoError::OversizedMessage(buffer.len()));
        }
        Ok(Self {
            buffer,
            offset: 0,
            depth,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.offset >= self.buffer.len()
    }

    pub fn depth(&self) -> usize {
        self.depth
    }

    pub fn remaining(&self) -> usize {
        self.buffer.len().saturating_sub(self.offset)
    }

    pub fn read_varint(&mut self) -> Result<u64, ProtoError> {
        let mut result: u64 = 0;
        let mut shift = 0;
        let mut count = 0;

        loop {
            if self.offset >= self.buffer.len() {
                return Err(ProtoError::UnexpectedEof);
            }
            let byte = self.buffer[self.offset];
            self.offset += 1;
            count += 1;

            if count > 10 {
                return Err(ProtoError::VarintOverflow);
            }

            let value = (byte & 0x7F) as u64;
            if shift >= 64 || (shift == 63 && value > 1) {
                return Err(ProtoError::VarintOverflow);
            }

            result |= value << shift;
            shift += 7;

            if (byte & 0x80) == 0 {
                break;
            }
        }

        Ok(result)
    }

    pub fn read_tag(&mut self) -> Result<(u32, WireType), ProtoError> {
        let key = self.read_varint()?;
        let tag = (key >> 3) as u32;
        let wire_val = (key & 0x07) as u32;
        let wire_type =
            WireType::from_u32(wire_val).ok_or(ProtoError::InvalidWireType(wire_val))?;
        Ok((tag, wire_type))
    }

    pub fn read_bytes(&mut self) -> Result<&'a [u8], ProtoError> {
        let len = usize::try_from(self.read_varint()?)
            .map_err(|_| ProtoError::OversizedLength(usize::MAX))?;
        if len > MAX_LENGTH_DELIMITED_BYTES {
            return Err(ProtoError::OversizedLength(len));
        }
        if self.offset + len > self.buffer.len() {
            return Err(ProtoError::UnexpectedEof);
        }
        let slice = &self.buffer[self.offset..self.offset + len];
        self.offset += len;
        Ok(slice)
    }

    pub fn read_fixed32(&mut self) -> Result<u32, ProtoError> {
        if self.offset + 4 > self.buffer.len() {
            return Err(ProtoError::UnexpectedEof);
        }
        let bytes: [u8; 4] = self.buffer[self.offset..self.offset + 4]
            .try_into()
            .unwrap();
        self.offset += 4;
        Ok(u32::from_le_bytes(bytes))
    }

    pub fn read_fixed64(&mut self) -> Result<u64, ProtoError> {
        if self.offset + 8 > self.buffer.len() {
            return Err(ProtoError::UnexpectedEof);
        }
        let bytes: [u8; 8] = self.buffer[self.offset..self.offset + 8]
            .try_into()
            .unwrap();
        self.offset += 8;
        Ok(u64::from_le_bytes(bytes))
    }

    pub fn skip_field(&mut self, wire_type: WireType) -> Result<(), ProtoError> {
        match wire_type {
            WireType::Varint => {
                self.read_varint()?;
            }
            WireType::Fixed64 => {
                if self.offset + 8 > self.buffer.len() {
                    return Err(ProtoError::UnexpectedEof);
                }
                self.offset += 8;
            }
            WireType::LengthDelimited => {
                let len = usize::try_from(self.read_varint()?)
                    .map_err(|_| ProtoError::OversizedLength(usize::MAX))?;
                if len > MAX_LENGTH_DELIMITED_BYTES {
                    return Err(ProtoError::OversizedLength(len));
                }
                if self.offset + len > self.buffer.len() {
                    return Err(ProtoError::UnexpectedEof);
                }
                self.offset += len;
            }
            WireType::Fixed32 => {
                if self.offset + 4 > self.buffer.len() {
                    return Err(ProtoError::UnexpectedEof);
                }
                self.offset += 4;
            }
        }
        Ok(())
    }
}

/// Raw candidate parsed from a single `gen_metadata.data` row.
#[derive(Clone, Debug)]
pub struct RawAntigravityUsageCandidate {
    pub gen_idx: i64,
    pub payload_digest: String,
    pub response_id: Option<String>,
    pub execution_id: Option<Vec<u8>>,
    pub uncached_input_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
}

/// Result of parsing `gen_metadata.data`.
#[derive(Clone, Debug)]
pub enum ParseGenMetadataResult {
    /// Successfully parsed raw candidate with valid fields.
    Candidate(RawAntigravityUsageCandidate),
    /// Candidate has invalid responseId (must be quarantined immediately).
    ResponseIdInvalid {
        gen_idx: i64,
        payload_digest: String,
    },
    /// Placeholder record (no token counters present), ignored per [INV-USAGE-01].
    Placeholder,
    /// Malformed protobuf payload.
    Malformed(ProtoError),
}

/// Parse a `gen_metadata.data` blob into a `RawAntigravityUsageCandidate`.
pub fn parse_gen_metadata_data(
    data: &[u8],
    gen_idx: i64,
    payload_digest: String,
) -> ParseGenMetadataResult {
    let mut reader = match ProtoReader::new(data) {
        Ok(r) => r,
        Err(err) => return ParseGenMetadataResult::Malformed(err),
    };

    let mut chat_model_bytes: Option<&[u8]> = None;
    let mut execution_id: Option<Vec<u8>> = None;

    while !reader.is_empty() {
        let (tag, wire) = match reader.read_tag() {
            Ok(tw) => tw,
            Err(err) => return ParseGenMetadataResult::Malformed(err),
        };

        match (tag, wire) {
            (1, WireType::LengthDelimited) => {
                chat_model_bytes = match reader.read_bytes() {
                    Ok(b) => Some(b),
                    Err(err) => return ParseGenMetadataResult::Malformed(err),
                };
            }
            (4, WireType::LengthDelimited) => {
                execution_id = match reader.read_bytes() {
                    Ok(b) => Some(b.to_vec()),
                    Err(err) => return ParseGenMetadataResult::Malformed(err),
                };
            }
            (_, w) => {
                if let Err(err) = reader.skip_field(w) {
                    return ParseGenMetadataResult::Malformed(err);
                }
            }
        }
    }

    let chat_model_data = match chat_model_bytes {
        Some(b) => b,
        None => return ParseGenMetadataResult::Placeholder,
    };

    let mut cm_reader = match ProtoReader::with_depth(chat_model_data, 1) {
        Ok(r) => r,
        Err(err) => return ParseGenMetadataResult::Malformed(err),
    };

    let mut usage_bytes: Option<&[u8]> = None;

    while !cm_reader.is_empty() {
        let (tag, wire) = match cm_reader.read_tag() {
            Ok(tw) => tw,
            Err(err) => return ParseGenMetadataResult::Malformed(err),
        };

        match (tag, wire) {
            (4, WireType::LengthDelimited) => {
                usage_bytes = match cm_reader.read_bytes() {
                    Ok(b) => Some(b),
                    Err(err) => return ParseGenMetadataResult::Malformed(err),
                };
            }
            (_, w) => {
                if let Err(err) = cm_reader.skip_field(w) {
                    return ParseGenMetadataResult::Malformed(err);
                }
            }
        }
    }

    let usage_data = match usage_bytes {
        Some(b) => b,
        None => return ParseGenMetadataResult::Placeholder,
    };

    let mut u_reader = match ProtoReader::with_depth(usage_data, 2) {
        Ok(r) => r,
        Err(err) => return ParseGenMetadataResult::Malformed(err),
    };

    let mut input_tokens: Option<u64> = None;
    let mut output_tokens: Option<u64> = None;
    let mut cache_read_tokens: Option<u64> = None;
    let mut thinking_output_tokens: Option<u64> = None;
    let mut raw_response_id_bytes: Option<&[u8]> = None;

    while !u_reader.is_empty() {
        let (tag, wire) = match u_reader.read_tag() {
            Ok(tw) => tw,
            Err(err) => return ParseGenMetadataResult::Malformed(err),
        };

        match (tag, wire) {
            (2, WireType::Varint) => {
                input_tokens = match u_reader.read_varint() {
                    Ok(v) => Some(v),
                    Err(err) => return ParseGenMetadataResult::Malformed(err),
                };
            }
            (3, WireType::Varint) => {
                output_tokens = match u_reader.read_varint() {
                    Ok(v) => Some(v),
                    Err(err) => return ParseGenMetadataResult::Malformed(err),
                };
            }
            (5, WireType::Varint) => {
                cache_read_tokens = match u_reader.read_varint() {
                    Ok(v) => Some(v),
                    Err(err) => return ParseGenMetadataResult::Malformed(err),
                };
            }
            (9, WireType::Varint) => {
                thinking_output_tokens = match u_reader.read_varint() {
                    Ok(v) => Some(v),
                    Err(err) => return ParseGenMetadataResult::Malformed(err),
                };
            }
            (11, WireType::LengthDelimited) => {
                raw_response_id_bytes = match u_reader.read_bytes() {
                    Ok(b) => Some(b),
                    Err(err) => return ParseGenMetadataResult::Malformed(err),
                };
            }
            (_, w) => {
                if let Err(err) = u_reader.skip_field(w) {
                    return ParseGenMetadataResult::Malformed(err);
                }
            }
        }
    }

    // [INV-USAGE-01] Candidate presence: any of the 4 counters must exist
    let has_counter = input_tokens.is_some()
        || output_tokens.is_some()
        || cache_read_tokens.is_some()
        || thinking_output_tokens.is_some();

    if !has_counter {
        return ParseGenMetadataResult::Placeholder;
    }

    // Normalize response_id
    let normalized_response_id = match raw_response_id_bytes {
        None => None,
        Some(raw) => {
            let s = match std::str::from_utf8(raw) {
                Ok(s) => s,
                Err(_) => {
                    return ParseGenMetadataResult::ResponseIdInvalid {
                        gen_idx,
                        payload_digest,
                    };
                }
            };
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else if trimmed.len() > MAX_RESPONSE_ID_BYTES {
                return ParseGenMetadataResult::ResponseIdInvalid {
                    gen_idx,
                    payload_digest,
                };
            } else {
                Some(trimmed.to_string())
            }
        }
    };

    ParseGenMetadataResult::Candidate(RawAntigravityUsageCandidate {
        gen_idx,
        payload_digest,
        response_id: normalized_response_id,
        execution_id,
        uncached_input_tokens: input_tokens,
        cached_tokens: cache_read_tokens,
        output_tokens,
        reasoning_tokens: thinking_output_tokens,
    })
}

/// Read the execution identity and selected model from one executor row.
pub fn parse_executor_metadata_data(data: &[u8]) -> Result<Option<(Vec<u8>, String)>, ProtoError> {
    let mut reader = ProtoReader::new(data)?;
    let mut execution_id = None;
    let mut settings = None;
    while !reader.is_empty() {
        let (tag, wire) = reader.read_tag()?;
        match (tag, wire) {
            (9, WireType::LengthDelimited) => execution_id = Some(reader.read_bytes()?.to_vec()),
            (10, WireType::LengthDelimited) => settings = Some(reader.read_bytes()?),
            (_, wire) => reader.skip_field(wire)?,
        }
    }
    let (Some(execution_id), Some(settings)) = (execution_id, settings) else {
        return Ok(None);
    };
    let mut settings_reader = ProtoReader::with_depth(settings, 1)?;
    let mut model_settings = None;
    while !settings_reader.is_empty() {
        let (tag, wire) = settings_reader.read_tag()?;
        if (tag, wire) == (1, WireType::LengthDelimited) {
            model_settings = Some(settings_reader.read_bytes()?);
        } else {
            settings_reader.skip_field(wire)?;
        }
    }
    let Some(model_settings) = model_settings else {
        return Ok(None);
    };
    let mut model_reader = ProtoReader::with_depth(model_settings, 2)?;
    let mut model = None;
    while !model_reader.is_empty() {
        let (tag, wire) = model_reader.read_tag()?;
        if (tag, wire) == (28, WireType::LengthDelimited) {
            let raw = model_reader.read_bytes()?;
            let value = std::str::from_utf8(raw)
                .map_err(|_| ProtoError::InvalidUtf8)?
                .trim();
            if value.len() > MAX_MODEL_BYTES {
                return Err(ProtoError::StringTooLong(value.len()));
            }
            if value.chars().any(char::is_control) {
                return Err(ProtoError::InvalidModelSlug);
            }
            model = Some(value.to_owned());
        } else {
            model_reader.skip_field(wire)?;
        }
    }
    Ok(model
        .filter(|value| !value.is_empty())
        .map(|value| (execution_id, value)))
}

/// Decode the semantic `responseId` from `steps.step_payload` (for `step_type = 15`).
/// Path: tag 5 -> tag 9 -> tag 11.
pub fn parse_step_payload_response_id(payload: &[u8]) -> Result<Option<String>, ProtoError> {
    let mut reader = ProtoReader::new(payload)?;
    let mut sub5_bytes: Option<&[u8]> = None;

    while !reader.is_empty() {
        let (tag, wire) = reader.read_tag()?;
        if tag == 5 && wire == WireType::LengthDelimited {
            sub5_bytes = Some(reader.read_bytes()?);
        } else {
            reader.skip_field(wire)?;
        }
    }

    let sub5 = match sub5_bytes {
        Some(b) => b,
        None => return Ok(None),
    };

    let mut r5 = ProtoReader::with_depth(sub5, 1)?;
    let mut sub9_bytes: Option<&[u8]> = None;

    while !r5.is_empty() {
        let (tag, wire) = r5.read_tag()?;
        if tag == 9 && wire == WireType::LengthDelimited {
            sub9_bytes = Some(r5.read_bytes()?);
        } else {
            r5.skip_field(wire)?;
        }
    }

    let sub9 = match sub9_bytes {
        Some(b) => b,
        None => return Ok(None),
    };

    let mut r9 = ProtoReader::with_depth(sub9, 2)?;
    let mut resp_id_bytes: Option<&[u8]> = None;

    while !r9.is_empty() {
        let (tag, wire) = r9.read_tag()?;
        if tag == 11 && wire == WireType::LengthDelimited {
            resp_id_bytes = Some(r9.read_bytes()?);
        } else {
            r9.skip_field(wire)?;
        }
    }

    match resp_id_bytes {
        None => Ok(None),
        Some(raw) => {
            let s = std::str::from_utf8(raw).map_err(|_| ProtoError::InvalidUtf8)?;
            let trimmed = s.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else if trimmed.len() > MAX_RESPONSE_ID_BYTES {
                Err(ProtoError::StringTooLong(trimmed.len()))
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
    }
}

/// Decoded step metadata information.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedStepMetadata {
    pub source: u32,
    pub occurred_at_ms: i64,
}

/// Decode `steps.metadata` to extract `source` and timestamp.
/// Timestamp is at tag 1: (seconds = tag 1, nanos = tag 2).
/// Source is at tag 3: varint.
pub fn parse_step_metadata(data: &[u8]) -> Result<DecodedStepMetadata, ProtoError> {
    let mut reader = ProtoReader::new(data)?;
    let mut ts_bytes: Option<&[u8]> = None;
    let mut source: Option<u32> = None;

    while !reader.is_empty() {
        let (tag, wire) = reader.read_tag()?;
        match (tag, wire) {
            (1, WireType::LengthDelimited) => {
                ts_bytes = Some(reader.read_bytes()?);
            }
            (3, WireType::Varint) => {
                source = Some(
                    u32::try_from(reader.read_varint()?)
                        .map_err(|_| ProtoError::InvalidWireType(0))?,
                );
            }
            (_, w) => {
                reader.skip_field(w)?;
            }
        }
    }

    let source_val = source.ok_or(ProtoError::InvalidWireType(0))?;
    let ts_data = ts_bytes.ok_or(ProtoError::InvalidTimestamp)?;

    let mut ts_reader = ProtoReader::with_depth(ts_data, 1)?;
    let mut seconds: Option<i64> = None;
    let mut nanos: Option<u32> = None;

    while !ts_reader.is_empty() {
        let (tag, wire) = ts_reader.read_tag()?;
        match (tag, wire) {
            (1, WireType::Varint) => {
                let v = ts_reader.read_varint()?;
                seconds = Some(i64::try_from(v).map_err(|_| ProtoError::InvalidTimestamp)?);
            }
            (2, WireType::Varint) => {
                let v = ts_reader.read_varint()?;
                let n = u32::try_from(v).map_err(|_| ProtoError::InvalidTimestamp)?;
                if n > 999_999_999 {
                    return Err(ProtoError::InvalidTimestamp);
                }
                nanos = Some(n);
            }
            (_, w) => {
                ts_reader.skip_field(w)?;
            }
        }
    }

    let s = seconds.ok_or(ProtoError::InvalidTimestamp)?;
    let n = nanos.unwrap_or(0);

    // [INV-TIME-01] Formula: seconds.checked_mul(1000)?.checked_add(i64::from(nanos) / 1_000_000)?
    let ms_from_s = s.checked_mul(1000).ok_or(ProtoError::InvalidTimestamp)?;
    let ms_from_n = i64::from(n) / 1_000_000;
    let occurred_at_ms = ms_from_s
        .checked_add(ms_from_n)
        .ok_or(ProtoError::InvalidTimestamp)?;

    if occurred_at_ms < 0 {
        return Err(ProtoError::InvalidTimestamp);
    }

    Ok(DecodedStepMetadata {
        source: source_val,
        occurred_at_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_varint_test(val: u64) -> Vec<u8> {
        let mut res = Vec::new();
        let mut v = val;
        while v > 0x7F {
            res.push((v as u8 & 0x7F) | 0x80);
            v >>= 7;
        }
        res.push(v as u8 & 0x7F);
        res
    }

    fn encode_field_test(tag: u32, wire: WireType, val: &[u8]) -> Vec<u8> {
        let key = (tag << 3) | (wire as u32);
        let mut res = encode_varint_test(key as u64);
        if wire == WireType::LengthDelimited {
            res.extend(encode_varint_test(val.len() as u64));
        }
        res.extend_from_slice(val);
        res
    }

    #[test]
    fn test_td_p1_pb_01_unknown_tags_skipped_and_bounds() {
        // Unknown tag is skipped
        let mut buf = encode_field_test(99, WireType::Varint, &encode_varint_test(12345));
        buf.extend_from_slice(&encode_field_test(
            100,
            WireType::Fixed32,
            &42u32.to_le_bytes(),
        ));
        buf.extend_from_slice(&encode_field_test(
            101,
            WireType::Fixed64,
            &9999u64.to_le_bytes(),
        ));
        buf.extend_from_slice(&encode_field_test(102, WireType::LengthDelimited, b"hello"));

        let mut r = ProtoReader::new(&buf).unwrap();
        while !r.is_empty() {
            let (_tag, wire) = r.read_tag().unwrap();
            r.skip_field(wire).unwrap();
        }

        // Truncated payload
        let truncated = &buf[..buf.len() - 3];
        let mut r_trunc = ProtoReader::new(truncated).unwrap();
        let mut err_found = false;
        while !r_trunc.is_empty() {
            match r_trunc.read_tag() {
                Ok((_, w)) => {
                    if r_trunc.skip_field(w).is_err() {
                        err_found = true;
                        break;
                    }
                }
                Err(_) => {
                    err_found = true;
                    break;
                }
            }
        }
        assert!(err_found);

        // Depth 33 rejected
        assert!(matches!(
            ProtoReader::with_depth(&[0; 10], 33),
            Err(ProtoError::NestingDepthExceeded(33))
        ));
    }

    #[test]
    fn test_td_p1_presence_01_absent_present_zero_field10() {
        // 1. All absent -> Placeholder
        let chat_model_empty = encode_field_test(4, WireType::LengthDelimited, &[]);
        let data_empty = encode_field_test(1, WireType::LengthDelimited, &chat_model_empty);
        assert!(matches!(
            parse_gen_metadata_data(&data_empty, 0, "digest".into()),
            ParseGenMetadataResult::Placeholder
        ));

        // 2. Present zero -> Candidate
        let mut usage_zero = encode_field_test(2, WireType::Varint, &encode_varint_test(0));
        usage_zero.extend(encode_field_test(11, WireType::LengthDelimited, b"resp-0"));
        let chat_model_zero = encode_field_test(4, WireType::LengthDelimited, &usage_zero);
        let data_zero = encode_field_test(1, WireType::LengthDelimited, &chat_model_zero);
        match parse_gen_metadata_data(&data_zero, 0, "digest".into()) {
            ParseGenMetadataResult::Candidate(c) => {
                assert_eq!(c.uncached_input_tokens, Some(0));
                assert_eq!(c.response_id, Some("resp-0".into()));
            }
            other => panic!("expected Candidate, got {other:?}"),
        }

        // 3. Field 10 only -> Placeholder
        let usage_10 = encode_field_test(10, WireType::Varint, &encode_varint_test(100));
        let chat_model_10 = encode_field_test(4, WireType::LengthDelimited, &usage_10);
        let data_10 = encode_field_test(1, WireType::LengthDelimited, &chat_model_10);
        assert!(matches!(
            parse_gen_metadata_data(&data_10, 0, "digest".into()),
            ParseGenMetadataResult::Placeholder
        ));
    }

    #[test]
    fn gen_metadata_reads_execution_id_and_ignores_display_model_fields() {
        let execution_id = b"execution-opaque-id";
        let mut usage = encode_field_test(2, WireType::Varint, &encode_varint_test(12));
        usage.extend(encode_field_test(11, WireType::LengthDelimited, b"resp-1"));

        let mut chat_model = encode_field_test(4, WireType::LengthDelimited, &usage);
        chat_model.extend(encode_field_test(
            18,
            WireType::LengthDelimited,
            b"display-model-must-not-be-used",
        ));
        chat_model.extend(encode_field_test(
            19,
            WireType::LengthDelimited,
            b"response-model-must-not-be-used",
        ));

        let mut data = encode_field_test(1, WireType::LengthDelimited, &chat_model);
        data.extend(encode_field_test(
            4,
            WireType::LengthDelimited,
            execution_id,
        ));

        match parse_gen_metadata_data(&data, 7, "digest".into()) {
            ParseGenMetadataResult::Candidate(candidate) => {
                assert_eq!(
                    candidate.execution_id.as_deref(),
                    Some(execution_id.as_slice())
                );
                assert_eq!(candidate.response_id.as_deref(), Some("resp-1"));
                assert_eq!(candidate.uncached_input_tokens, Some(12));
            }
            other => panic!("expected Candidate, got {other:?}"),
        }
    }

    #[test]
    fn executor_metadata_reads_selected_model_path() {
        let execution_id = b"execution-selected-model";
        let model = b"gemini-3.8-pro-medium";
        let model_settings = encode_field_test(28, WireType::LengthDelimited, model);
        let settings = encode_field_test(1, WireType::LengthDelimited, &model_settings);
        let mut data = encode_field_test(9, WireType::LengthDelimited, execution_id);
        data.extend(encode_field_test(10, WireType::LengthDelimited, &settings));

        assert_eq!(
            parse_executor_metadata_data(&data).unwrap(),
            Some((execution_id.to_vec(), "gemini-3.8-pro-medium".into()))
        );
    }

    #[test]
    fn test_td_p1_num_01_timestamp_arithmetic() {
        // Valid timestamp: seconds=1000, nanos=999_999 -> 1_000_000 ms (nanos / 1_000_000 = 0)
        let mut ts_data = encode_field_test(1, WireType::Varint, &encode_varint_test(1000));
        ts_data.extend(encode_field_test(
            2,
            WireType::Varint,
            &encode_varint_test(999_999),
        ));
        let mut md = encode_field_test(1, WireType::LengthDelimited, &ts_data);
        md.extend(encode_field_test(
            3,
            WireType::Varint,
            &encode_varint_test(2),
        ));
        let decoded = parse_step_metadata(&md).unwrap();
        assert_eq!(decoded.occurred_at_ms, 1_000_000);

        // nanos = 1_000_000 -> 1_000_001 ms
        let mut ts_data2 = encode_field_test(1, WireType::Varint, &encode_varint_test(1000));
        ts_data2.extend(encode_field_test(
            2,
            WireType::Varint,
            &encode_varint_test(1_000_000),
        ));
        let mut md2 = encode_field_test(1, WireType::LengthDelimited, &ts_data2);
        md2.extend(encode_field_test(
            3,
            WireType::Varint,
            &encode_varint_test(2),
        ));
        let decoded2 = parse_step_metadata(&md2).unwrap();
        assert_eq!(decoded2.occurred_at_ms, 1_000_001);

        // nanos = 1_000_000_000 -> invalid (> 999_999_999)
        let mut ts_data3 = encode_field_test(1, WireType::Varint, &encode_varint_test(1000));
        ts_data3.extend(encode_field_test(
            2,
            WireType::Varint,
            &encode_varint_test(1_000_000_000),
        ));
        let mut md3 = encode_field_test(1, WireType::LengthDelimited, &ts_data3);
        md3.extend(encode_field_test(
            3,
            WireType::Varint,
            &encode_varint_test(2),
        ));
        assert_eq!(parse_step_metadata(&md3), Err(ProtoError::InvalidTimestamp));
    }
}
