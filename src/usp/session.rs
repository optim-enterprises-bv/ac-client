//! USP Session Context — per-endpoint sequence ID tracking.
//!
//! A `SessionContext` is maintained per connected agent.  When using
//! `SessionContextRecord` (MQTT MTP), each outgoing record gets a
//! monotonically increasing `sequence_id` and the receiver's acknowledged
//! up-to value is tracked in `expected_id`.

use std::collections::VecDeque;

/// Per-endpoint session state.
#[derive(Debug)]
pub struct SessionContext {
    pub session_id: u64,
    /// Next sequence_id to stamp on an outgoing record.
    pub next_seq: u64,
    /// The sequence_id we have told the remote we expect (i.e. we have
    /// received all records up to `expected_id - 1`).
    pub expected_id: u64,
    /// Outgoing records buffered for potential retransmission.
    retransmit_buf: VecDeque<(u64, Vec<u8>)>,
}

impl SessionContext {
    pub fn new(session_id: u64) -> Self {
        SessionContext {
            session_id,
            next_seq: 1,
            expected_id: 1,
            retransmit_buf: VecDeque::new(),
        }
    }

    /// Allocate the next sequence ID for an outgoing record and buffer the
    /// raw bytes for potential retransmission.
    pub fn next_sequence_id(&mut self, payload: Vec<u8>) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.retransmit_buf.push_back((seq, payload));
        // Keep a bounded retransmit buffer (last 256 records).
        if self.retransmit_buf.len() > 256 {
            self.retransmit_buf.pop_front();
        }
        seq
    }

    /// Advance `expected_id` when we receive records in order.
    pub fn advance_expected(&mut self) {
        self.expected_id += 1;
    }

    /// Return buffered record bytes for retransmission of `seq_id`.
    pub fn retransmit(&self, seq_id: u64) -> Option<&[u8]> {
        self.retransmit_buf
            .iter()
            .find(|(id, _)| *id == seq_id)
            .map(|(_, b)| b.as_slice())
    }
}
