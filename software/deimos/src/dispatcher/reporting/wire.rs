//! Wire-format types for the realtime reporting transport. Encoded with [`postcard`]; the enum
//! variant tag (0 = `Schema`, 1 = `Row`) serves as the leading byte tag, no extra framing.

use serde::{Deserialize, Serialize};

/// A single wire message sent by the reporting dispatcher.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ReportingMessage {
    /// Run channel layout. Emitted at start of Operating and re-emitted every `schema_period`
    /// so late-joining viewers can discover channel metadata. `is_session_end = true` marks
    /// the final packet of the run.
    Schema {
        channel_names: Vec<String>,
        /// `None` means unknown or not applicable.
        channel_units: Vec<Option<String>>,
        /// Wall-clock Unix-epoch ns captured at dispatcher `init`, approximately the wall time
        /// at which `Row::timestamp` reads zero. Viewers add `Row::timestamp` to recover an
        /// approximate display time.
        monotonic_epoch_ns: u64,
        /// `#[serde(default)]` keeps struct construction convenient; postcard always encodes
        /// the byte on the wire.
        #[serde(default)]
        is_session_end: bool,
    },

    /// One control-loop cycle.
    Row {
        /// Incremented once per Row regardless of send outcome; viewer-observed gaps reveal
        /// dropped frames (also counted by the dispatcher's `dropped_frames`).
        seq: u64,
        /// Seconds from the start of the run.
        timestamp: f64,
        /// ISO-8601 (e.g. `"2026-04-19T14:32:01.123456789Z"`).
        system_time: String,
        /// In the order declared by `Schema::channel_names`.
        values: Vec<f64>,
    },
}

impl ReportingMessage {
    /// Append-encode `self` into `buf` and return the appended slice. Callers typically pass a
    /// cleared pre-allocated `Vec<u8>` to avoid per-frame allocation.
    pub fn encode_into<'a>(&self, buf: &'a mut Vec<u8>) -> Result<&'a [u8], postcard::Error> {
        let start = buf.len();
        let owned = std::mem::take(buf);
        *buf = postcard::to_extend(self, owned)?;
        Ok(&buf[start..])
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_round_trip() {
        let msg = ReportingMessage::Schema {
            channel_names: vec!["daq7_tc_K".to_string(), "daq7_rtd_ohm".to_string()],
            channel_units: vec![Some("K".to_string()), Some("ohm".to_string())],
            monotonic_epoch_ns: 1_713_530_000_000_000_000_u64,
            is_session_end: false,
        };

        let mut buf = Vec::new();
        msg.encode_into(&mut buf).expect("encode Schema");
        let decoded = ReportingMessage::decode(&buf).expect("decode Schema");
        assert_eq!(msg, decoded);
    }

    #[test]
    fn row_round_trip() {
        let msg = ReportingMessage::Row {
            seq: 42,
            timestamp: std::f64::consts::PI,
            system_time: "2026-04-19T14:32:01.123456789Z".to_string(),
            values: vec![300.15, 99.87, -1.0],
        };

        let mut buf = Vec::new();
        msg.encode_into(&mut buf).expect("encode Row");
        let decoded = ReportingMessage::decode(&buf).expect("decode Row");
        assert_eq!(msg, decoded);
    }

    #[test]
    fn schema_tag_is_zero() {
        // postcard encodes enum variant 0 (Schema) as a leading 0x00 byte.
        let msg = ReportingMessage::Schema {
            channel_names: vec![],
            channel_units: vec![],
            monotonic_epoch_ns: 0,
            is_session_end: false,
        };
        let mut buf = Vec::new();
        msg.encode_into(&mut buf).expect("encode");
        assert_eq!(buf[0], 0x00, "Schema variant tag must be 0");
    }

    #[test]
    fn row_tag_is_one() {
        // postcard encodes enum variant 1 (Row) as a leading 0x01 byte.
        let msg = ReportingMessage::Row {
            seq: 0,
            timestamp: 0.0,
            system_time: String::new(),
            values: vec![],
        };
        let mut buf = Vec::new();
        msg.encode_into(&mut buf).expect("encode");
        assert_eq!(buf[0], 0x01, "Row variant tag must be 1");
    }

    #[test]
    fn session_end_schema_round_trip() {
        // A Schema with is_session_end=true must round-trip correctly.
        let msg = ReportingMessage::Schema {
            channel_names: vec!["ch0".to_string()],
            channel_units: vec![None],
            monotonic_epoch_ns: 42,
            is_session_end: true,
        };
        let mut buf = Vec::new();
        msg.encode_into(&mut buf)
            .expect("encode session-end Schema");
        let decoded = ReportingMessage::decode(&buf).expect("decode session-end Schema");
        assert_eq!(msg, decoded);
    }

    #[test]
    fn encode_into_appends_without_clearing() {
        // Verify that encode_into appends rather than overwrites.
        let mut buf = vec![0xFFu8; 4];
        let msg = ReportingMessage::Row {
            seq: 1,
            timestamp: 0.0,
            system_time: String::new(),
            values: vec![],
        };
        let written_len = msg.encode_into(&mut buf).expect("encode").len();
        assert_eq!(&buf[..4], &[0xFF; 4], "prefix must be preserved");
        assert!(written_len > 0);
    }
}
