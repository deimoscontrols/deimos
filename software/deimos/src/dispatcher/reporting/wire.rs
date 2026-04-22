//! Wire-format types for the realtime reporting transport.
//!
//! Two message kinds are sent on the UDP multicast channel:
//!
//! - [`ReportingMessage::Schema`] — emitted once when the controller enters Operating, and then
//!   periodically while Operating so late-joining viewers can discover channel metadata.
//! - [`ReportingMessage::Row`] — emitted once per control-loop cycle, carrying the latest values
//!   for every dispatched channel.
//!
//! # Encoding
//!
//! Messages are encoded with [`postcard`]. Because `postcard` serializes Rust enums with a
//! compact leading varint tag (tag 0 for `Schema`, tag 1 for `Row`), no separate framing byte
//! is required — the enum tag *is* the leading byte tag called for in the design. For these two
//! variants the tag fits in a single byte (varint encoding of 0 or 1).
//!
//! Use [`ReportingMessage::encode_into`] to serialize into an existing buffer and
//! [`ReportingMessage::decode`] to deserialize from a received byte slice.

use serde::{Deserialize, Serialize};

/// A single wire message sent by the reporting dispatcher.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ReportingMessage {
    /// Metadata describing a run's channel layout.
    ///
    /// Emitted when the controller enters Operating and periodically thereafter (default every
    /// 2 s). Viewers that join mid-run buffer `Row` messages until they receive a `Schema`.
    ///
    /// When `is_session_end` is `true`, this is the final packet for the run. Viewers should
    /// treat receipt of this packet as a signal to close the current session, finalize their
    /// forensic log, and display a "session ended" indicator.
    Schema {
        /// Ordered list of channel names, parallel to `channel_units` and to the `values` vec
        /// in each `Row`.
        channel_names: Vec<String>,

        /// Per-channel unit labels, parallel to `channel_names`. `None` means the unit is
        /// unknown or not applicable.
        channel_units: Vec<Option<String>>,

        /// Anchor that maps the controller's monotonic clock to wall time.
        ///
        /// Wall-clock Unix-epoch nanoseconds captured by the dispatcher at `init` time,
        /// approximately equal to the wall time at which the controller's session-relative
        /// timestamp read zero. The approximation error is the interval between
        /// `ReportingDispatcher::init` and the first Operating cycle (typically one
        /// configure-phase duration, on the order of tens of milliseconds).
        ///
        /// Viewers use this to convert `Row::timestamp` (seconds from session start) to an
        /// approximate wall-clock display time.
        monotonic_epoch_ns: u64,

        /// Set to `true` on the final Schema packet emitted by `terminate`. Viewers use this
        /// to detect a clean session end (as opposed to a stale/dropped connection).
        /// Always `false` for normal periodic Schema packets.
        ///
        /// `#[serde(default)]` makes struct construction convenient when the field is omitted
        /// in key-value formats (e.g., TOML config), but postcard binary encoding always
        /// includes this byte on the wire.
        #[serde(default)]
        is_session_end: bool,
    },

    /// One cycle of channel values from the control loop.
    Row {
        /// Monotonically increasing sequence number, incremented once per Row regardless of
        /// whether the send succeeded. Gaps in `seq` observed by the viewer indicate frames
        /// that were dropped by the dispatcher (WouldBlock, ENETUNREACH, or any other send
        /// error); the dispatcher's `dropped_frames` counter tracks the same events.
        seq: u64,

        /// Controller monotonic timestamp in seconds, measured from the start of the run.
        timestamp: f64,

        /// Controller system time formatted as an ISO-8601 string (e.g.
        /// `"2026-04-19T14:32:01.123456789Z"`).
        system_time: String,

        /// Channel values, in the same order as `Schema::channel_names`.
        values: Vec<f64>,
    },
}

impl ReportingMessage {
    /// Serialize `self` into `buf`, appending to any existing content.
    ///
    /// Returns a slice of the bytes that were appended. The buffer is extended as needed;
    /// callers typically pass a pre-allocated `Vec<u8>` that is cleared between calls.
    ///
    /// # Errors
    ///
    /// Returns a [`postcard::Error`] if serialization fails (e.g. out-of-memory).
    pub fn encode_into<'a>(&self, buf: &'a mut Vec<u8>) -> Result<&'a [u8], postcard::Error> {
        let start = buf.len();
        let owned = std::mem::take(buf);
        *buf = postcard::to_extend(self, owned)?;
        Ok(&buf[start..])
    }

    /// Deserialize a `ReportingMessage` from a byte slice produced by [`encode_into`].
    ///
    /// # Errors
    ///
    /// Returns a [`postcard::Error`] if the bytes are malformed or truncated.
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
