//! Motion from channel state information — `Device.X_OptimACS_Sensing.Csi.*`.
//!
//! [`super::motion`] senses a body from one number per link per tick: the
//! RSSI, which is the whole channel collapsed to a scalar. This module senses
//! the same body from the channel itself. With CFR capture enabled, the
//! QCN9074 reports, for every sounding, the complex response of each receive
//! chain on each of 256 subcarriers — about a thousand numbers where RSSI has
//! one. A person moving through the path changes some subcarriers and leaves
//! others alone, and that is visible in a two-second window long before the
//! sum of them moves a whole dB.
//!
//! # What is actually measured
//!
//! Per subcarrier-chain position, the temporal variance of magnitude within
//! the window divided by the square of its mean. That ratio is dimensionless,
//! which is the point: a link at -50 dBm and a link at -80 dBm produce
//! magnitudes an order of magnitude apart, and an unnormalised variance would
//! rank them by how loud they are rather than by how much they moved. The
//! per-position ratios are averaged, and the average is reported in dB.
//!
//! Measured on a real QCN9074 mesh link with nobody moving: 0.006, which is
//! -22 dB, steady to within 0.6 dB across every window of a 97-record capture.
//! That stability is what makes the number usable — an ambient this tight
//! means a detector watching it can afford to fire on a few dB.
//!
//! # Why the existing detector is reused rather than replaced
//!
//! Everything hard about motion sensing is the false-positive behaviour, and
//! [`LinkDetector`] already encodes it: outlier rejection, a learnt ambient it
//! will not let the signal teach, a variance ratio with hysteresis, and a
//! dwell so one person crossing one doorway is one event. None of that is
//! about RSSI. It is about a scalar in dB that sits still when a room is empty
//! — which is exactly what the energy above is. Feeding it a second kind of
//! scalar costs one method ([`LinkDetector::push_f64`]) and inherits every
//! false-positive test that was written for the first.
//!
//! The one detector field that is genuinely about RSSI is `saturation_dbm`,
//! which asks whether the absolute level is so high the receiver's gain stage
//! is flickering. A motion energy has no such level, so CSI links disable that
//! gate by setting the field to infinity rather than by feeding it a number
//! and hoping. See [`csi_detector`].
//!
//! # Why the reader is a thread and not a task
//!
//! `cfr_capture0` is a relayfs file, and relayfs has a habit that breaks the
//! obvious loop: a read returns whatever is buffered and then reports EOF,
//! even though the stream is still live. A reader that stops at EOF collects
//! one batch and exits, which looks exactly like a radio that produced one
//! batch. The reader must instead read to EOF, sleep, and read the same
//! descriptor again, forever.
//!
//! That is blocking file I/O on a file that never ends, so it does not belong
//! on the tokio runtime at any width: `tokio::fs` would occupy a blocking-pool
//! thread permanently per radio, and a plain `File` read would block a worker
//! that has the rest of the agent on it. One dedicated `std::thread` per radio
//! is the honest shape. It does nothing but read, frame, and hand off.
//!
//! Draining matters. The driver gives each radio 255 sub-buffers of ~16.7 KB;
//! a reader that falls behind does not slow the radio down, it loses captures,
//! and nothing in the record stream says so.
//!
//! # What sits on top of this
//!
//! [`super::vitals`] is a second stage over the same records. It does not touch
//! the radio, the relay, or the framing: it consumes two more per-record
//! scalars this module computes beside the magnitudes — [`mean_mag`] and
//! [`mean_phase`] — and asks two questions the two-second motion window cannot,
//! because both need half a minute of a link that is NOT moving. Breathing and
//! heart rates are experimental candidates and not measurements; the fall
//! candidate is a shape in this module's own energy series. Consent is the same
//! consent: with `csi_enabled` false neither stage exists.
//!
//! # Consent
//!
//! Off by default, and for a stronger version of the reason motion sensing is.
//! Motion says somebody moved. CSI measures the shape of the multipath they
//! moved through, which is more information about the inside of a home, and
//! obtaining it puts the radio into a mode that sounds the channel with extra
//! frames. With `csi_enabled` false the reader is never started and
//! `enable_cfr` is never written, so the driver never allocates the ~5 MB of
//! relay buffer either — the feature is absent, not merely quiet.
//!
//! # Instance numbers
//!
//! Stable and sparse, for the reason spelled out at length in
//! [`super::motion`]: a ValueChange Notify names an instance, so an instance
//! must keep meaning the same radio and the same peer for as long as the
//! controller might come back and ask about it. `CsiNumberOfEntries` is a
//! COUNT, not a range.

use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use log::{debug, info, warn};

use crate::config::ClientConfig;
use crate::usp::agent::StatusSender;
use crate::usp::dm::motion::{self, LinkDetector, LinkKind, State, Transition};
use crate::usp::dm::vitals::{FallDetector, FallEvent, Vitals, VitalsWindow};
use crate::usp::endpoint::EndpointId;
use crate::usp::message::{build_value_change_notify, encode_msg};
use crate::usp::record;

// ── The wire format ──────────────────────────────────────────────────────────
//
// Upstream ath11k CFR, metadata version 4 / data version 1, as produced by a
// QCN9074 and verified byte for byte against real captures. Little-endian and
// packed throughout; every field below was read off a live radio rather than
// taken from a header, and the offsets are asserted by the fixture tests.

/// Marks the start of a record. Deliberately not a valid anything else, which
/// is what makes resynchronisation after a corrupt record possible at all.
const START_MAGIC: u32 = 0xDEAD_BEAF;
/// Marks the end of a record, after the payload.
const END_MAGIC: u32 = 0xBEAF_DEAD;
/// `csi_cfr_header`: 16 bytes of framing plus a 92-byte `cfr_metadata`.
const HEADER_LEN: usize = 108;
/// Offset of `cfr_metadata` within the header.
const META: usize = 16;
/// The enhanced DMA header that opens the payload. Its own `info0` says how
/// long the full DMA header is (48 bytes on the captures seen); these 16 are
/// the part whose layout is known.
const DMA_LEN: usize = 16;
/// Both magics are four bytes.
const MAGIC_LEN: usize = 4;

/// Largest payload a record may claim before it is treated as corrupt.
///
/// Not decoration. The payload length comes out of the record's own header, so
/// a corrupt header can claim any length at all — and the framing loop's
/// response to "the buffer does not hold the whole record yet" is to WAIT for
/// more bytes. A corrupt length of 4 GB would therefore stall the reader for
/// that radio permanently, with a carry buffer growing behind it. 32 KB is
/// comfortably above the largest real record (160 MHz, 8 chains: 8 x 512 tones
/// x 4 bytes = 16 KB plus a header) and far below anything that could wedge a
/// router.
const MAX_PAYLOAD: usize = 32 * 1024;

/// The enhanced DMA header that prefixes the I/Q data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DmaHdr {
    /// `info0` bits 0-7. Identifies the TLV; not interpreted here.
    pub tag: u8,
    /// `info0` bits 8-13, in 32-bit WORDS. Observed 12, i.e. a 48-byte header,
    /// of which the first 16 bytes are the fields below. The I/Q data starts
    /// after the full length, not after these 16 bytes — reading it at a fixed
    /// 16 would silently shift every sample by 32 bytes and still produce
    /// plausible magnitudes.
    pub header_words: u8,
    /// `info1` bit 0.
    pub upload_done: bool,
    /// `info1` bits 1-3.
    pub capture_type: u8,
    /// `info1` bits 4-5.
    pub preamble: u8,
    /// `info1` bits 6-8.
    pub nss: u8,
    /// `info1` bits 9-11, stored by the driver as one less than the count.
    pub num_chains: u8,
    /// `info1` bits 12-14.
    pub upload_pkt_bw: u8,
    pub sw_peer_id: u16,
    pub phy_ppdu_id: u16,
    /// Bytes of I/Q data following the DMA header. Observed 4096.
    pub total_bytes: u16,
}

impl DmaHdr {
    /// Full DMA header length in bytes, floored at the part that is parsed.
    ///
    /// A header claiming to be shorter than the fields already read from it is
    /// corrupt; clamping rather than rejecting keeps one bad bit from
    /// discarding a record whose end magic still checks out.
    fn header_len(self) -> usize {
        (self.header_words as usize * 4).max(DMA_LEN)
    }

    /// Subcarriers per chain, derived rather than assumed.
    ///
    /// 80 MHz with four chains gives 4096 / (4 x 4) = 256. Derived because the
    /// bandwidth is a runtime property of the interface: the same code has to
    /// be right when an operator moves the radio to 40 MHz and the driver
    /// starts sending half as many tones.
    fn tones(self) -> usize {
        // `checked_div` rather than a guarded divide: `num_chains` is read out
        // of a corruptible header, and a zero there must produce 0 tones
        // rather than a division that takes the reader thread down.
        (self.total_bytes as usize)
            .checked_div(self.num_chains as usize * 4)
            .unwrap_or(0)
    }
}

/// One channel-sounding record.
#[derive(Debug, Clone)]
pub struct Record {
    pub peer: [u8; 6],
    /// 1 is a successful capture. Anything else is a sounding the firmware
    /// abandoned, and its payload is not a measurement of anything.
    pub status: u8,
    /// 0 = 20 MHz, 1 = 40, 2 = 80, 3 = 160.
    pub capture_bw: u8,
    pub chan_bw: u8,
    pub phy_mode: u8,
    pub prim20: u16,
    pub cf1: u16,
    pub cf2: u16,
    pub num_rx_chain: u8,
    pub timestamp: u32,
    /// Per-chain RSSI in dBm.
    ///
    /// Stored by the driver in a `u32` field while being a negative number, so
    /// -56 dBm arrives as 4294967240. Decoded as `i32` here rather than at
    /// every use: a reader that forgets once reports a link at four billion
    /// dBm, and every comparison against it is false.
    pub chain_rssi: [i32; 8],
    pub dma: DmaHdr,
    /// Interleaved I and Q, `num_chains` x `tones` pairs.
    pub iq: Vec<i16>,
}

impl Record {
    /// Did the firmware complete this sounding?
    pub fn is_ok(&self) -> bool {
        self.status == 1
    }

    /// Subcarriers per chain in this record.
    pub fn tones(&self) -> usize {
        self.dma.tones()
    }

    /// Receive chains in this record, as the DMA header reports them.
    pub fn chains(&self) -> usize {
        self.dma.num_chains as usize
    }

    /// The peer address in the spelling the data model uses.
    fn peer_str(&self) -> String {
        mac_upper(&self.peer)
    }
}

/// What one pass over a buffer produced.
///
/// A plain `(Vec<Record>, usize)` would be the tidier signature, and it is not
/// enough: `ParseErrors` and the resync count are REPORTED PARAMETERS, not
/// diagnostics. A controller reading a link whose energy has gone flat needs
/// to be able to tell "the room is empty" from "this radio's relay is handing
/// us rubbish", and that distinction only exists here, in the framing.
#[derive(Debug, Default)]
pub struct Parsed {
    pub records: Vec<Record>,
    /// Bytes of `buf` that will never be needed again. The caller keeps the
    /// rest as carry: records straddle read boundaries routinely.
    pub consumed: usize,
    /// Records whose framing was well-formed enough to find but whose end
    /// magic did not check out, plus headers claiming impossible lengths.
    pub errors: u32,
    /// Times the parser had to skip bytes to find a record boundary — because
    /// the bytes it was looking at were not a start magic, or because a record
    /// that began well did not end where its own header said it would.
    ///
    /// Counted separately from `errors` because the two answer different
    /// questions: `errors` is how many records were lost, `resyncs` is how
    /// often the stream's framing broke. A relay dropping whole sub-buffers
    /// raises the second without the first.
    pub resyncs: u32,
}

/// Read a little-endian `u16`, or `None` past the end of the buffer.
fn u16_at(b: &[u8], off: usize) -> Option<u16> {
    b.get(off..off + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
}

/// Read a little-endian `u32`, or `None` past the end of the buffer.
fn u32_at(b: &[u8], off: usize) -> Option<u32> {
    b.get(off..off + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// Index of the next start magic at or after `from`.
fn find_start(buf: &[u8], from: usize) -> Option<usize> {
    let pat = START_MAGIC.to_le_bytes();
    let tail = buf.get(from..)?;
    tail.windows(MAGIC_LEN).position(|w| w == pat).map(|i| from + i)
}

/// Where to resume after deciding the bytes at `pos` are not a record.
///
/// Scanning from `pos + 1` rather than `pos + MAGIC_LEN` matters when the
/// magic itself was the corrupt part: a stream that lost a byte puts a
/// perfectly good record one byte late, and skipping four would step over its
/// magic and lose that record too.
///
/// Returning `None` means there is no magic left in the buffer. The caller
/// then keeps only the last three bytes, because a fourth could be the first
/// byte of a magic split across this read and the next.
fn resync_from(buf: &[u8], pos: usize) -> Option<usize> {
    find_start(buf, pos + 1)
}

/// Frame `buf` into records, tolerating anything.
///
/// This function is the only thing in the module that sees bytes a router does
/// not control: the relay hands over whatever the firmware DMA'd, truncated
/// wherever the read happened to stop, and a driver that drops sub-buffers
/// leaves a hole in the middle of a record. It must therefore never panic and
/// never stall — every read is bounds-checked, every length is bounded, and
/// every path either consumes bytes or gives up on the buffer.
///
/// The contract with the caller is: everything before `consumed` is finished
/// with, everything after it must be presented again with more bytes appended.
pub fn parse(buf: &[u8]) -> Parsed {
    let mut out = Parsed::default();
    // What a caller should keep when the parser has run out of interpretable
    // bytes but a magic could still be split across the read boundary.
    let tail_keep = |len: usize| len.saturating_sub(MAGIC_LEN - 1);
    let mut pos = 0usize;

    loop {
        if pos >= buf.len() {
            break;
        }
        // Not enough for a header yet. Whether these bytes are a record's
        // beginning or rubbish cannot be decided until more arrive, so they
        // are carried either way.
        if buf.len() - pos < HEADER_LEN + DMA_LEN {
            break;
        }
        if u32_at(buf, pos) != Some(START_MAGIC) {
            out.resyncs += 1;
            match resync_from(buf, pos) {
                Some(next) => {
                    pos = next;
                    continue;
                }
                None => {
                    pos = tail_keep(buf.len()).max(pos);
                    break;
                }
            }
        }

        let dma = match read_dma(buf, pos + HEADER_LEN) {
            Some(d) => d,
            None => break,
        };
        let payload = dma.header_len() + dma.total_bytes as usize;
        if payload > MAX_PAYLOAD {
            // A length this large is a corrupt header, not a large record.
            // Treated as an error and resynced past rather than waited for:
            // waiting is what would stall this radio forever.
            out.errors += 1;
            out.resyncs += 1;
            match resync_from(buf, pos) {
                Some(next) => {
                    pos = next;
                    continue;
                }
                None => {
                    pos = tail_keep(buf.len()).max(pos);
                    break;
                }
            }
        }

        let total = HEADER_LEN + payload + MAGIC_LEN;
        if buf.len() - pos < total {
            // A genuine straddle: the record starts here and the rest has not
            // been read yet. Carry it whole.
            break;
        }
        if u32_at(buf, pos + HEADER_LEN + payload) != Some(END_MAGIC) {
            // The start magic was real but the record did not end where its
            // own header said it would, so the length or the payload is
            // damaged. Neither can be trusted, so nothing is salvaged.
            out.errors += 1;
            out.resyncs += 1;
            match resync_from(buf, pos) {
                Some(next) => {
                    pos = next;
                    continue;
                }
                None => {
                    pos = tail_keep(buf.len()).max(pos);
                    break;
                }
            }
        }

        match read_record(buf, pos, dma) {
            Some(r) => out.records.push(r),
            // Unreachable given the bounds already checked, and handled
            // anyway: the alternative is an `unwrap` in the one function that
            // must never panic.
            None => out.errors += 1,
        }
        pos += total;
    }

    out.consumed = pos;
    out
}

/// Decode the 16 known bytes of the enhanced DMA header.
fn read_dma(buf: &[u8], off: usize) -> Option<DmaHdr> {
    let info0 = u16_at(buf, off)?;
    let info1 = u16_at(buf, off + 2)?;
    Some(DmaHdr {
        tag: (info0 & 0xFF) as u8,
        header_words: ((info0 >> 8) & 0x3F) as u8,
        upload_done: info1 & 1 != 0,
        capture_type: ((info1 >> 1) & 0x7) as u8,
        preamble: ((info1 >> 4) & 0x3) as u8,
        nss: ((info1 >> 6) & 0x7) as u8,
        // Stored as one less than the count: a single-chain capture writes 0.
        num_chains: (((info1 >> 9) & 0x7) as u8) + 1,
        upload_pkt_bw: ((info1 >> 12) & 0x7) as u8,
        sw_peer_id: u16_at(buf, off + 4)?,
        phy_ppdu_id: u16_at(buf, off + 6)?,
        total_bytes: u16_at(buf, off + 8)?,
    })
}

/// Decode a record whose framing has already been checked.
fn read_record(buf: &[u8], pos: usize, dma: DmaHdr) -> Option<Record> {
    let m = pos + META;
    let mut peer = [0u8; 6];
    peer.copy_from_slice(buf.get(m..m + 6)?);

    let mut chain_rssi = [0i32; 8];
    for (i, slot) in chain_rssi.iter_mut().enumerate() {
        // Reinterpreted rather than converted: the driver stores a negative
        // dBm in an unsigned field, so -56 arrives as 4294967240 and a
        // saturating cast would report it as i32::MAX.
        *slot = u32_at(buf, m + 28 + i * 4)? as i32;
    }

    let iq_off = pos + HEADER_LEN + dma.header_len();
    let iq_len = dma.total_bytes as usize;
    let raw = buf.get(iq_off..iq_off + iq_len)?;
    let iq = raw
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect();

    Some(Record {
        peer,
        status: *buf.get(m + 6)?,
        capture_bw: *buf.get(m + 7)?,
        chan_bw: *buf.get(m + 8)?,
        phy_mode: *buf.get(m + 9)?,
        prim20: u16_at(buf, m + 10)?,
        cf1: u16_at(buf, m + 12)?,
        cf2: u16_at(buf, m + 14)?,
        num_rx_chain: *buf.get(m + 19)?,
        timestamp: u32_at(buf, m + 20)?,
        chain_rssi,
        dma,
        iq,
    })
}

// ── The feature ──────────────────────────────────────────────────────────────

/// Magnitudes below which a subcarrier position is treated as unoccupied.
///
/// About seven positions per chain of the 256 are guard or DC subcarriers that
/// the radio never transmits on, and they arrive as near-zero I and Q. Their
/// variance is small but their MEAN is smaller, so `var / mean²` on a dead
/// tone is enormous and entirely numerical. Left in, a handful of them swamp
/// the thousand real positions and the reported energy measures rounding
/// noise instead of the room.
const MIN_TONE_MAG: f64 = 1.0;

/// Reporting range for the energy, in dB.
///
/// Clamped rather than reported raw because both ends are degenerate: an
/// exactly-flat window gives `log10(0)` and a window holding one corrupt
/// record can give an arbitrarily large ratio. A parameter that is sometimes
/// `-inf` is worse than one that is sometimes at its floor — the controller
/// cannot compare it, and a detector fed it would never recover.
///
/// The useful range sits well inside these: an empty room measured -22 dB.
const ENERGY_DB_MIN: f64 = -60.0;
const ENERGY_DB_MAX: f64 = 20.0;

/// Per-position magnitude, `chains` x `tones` of them.
///
/// Returned as a flat vector, and the features below are deliberately
/// order-agnostic. The interleaving is chain-outer — four consecutive blocks
/// of 256 tones, confirmed on real captures by the block means tracking the
/// per-chain RSSI spread — but nothing here depends on that: every statistic
/// is computed per position and then averaged, so a driver that reordered the
/// payload tomorrow would change which position is which and not the answer.
pub fn magnitudes(rec: &Record) -> Vec<f32> {
    rec.iq
        .chunks_exact(2)
        .map(|p| f32::from(p[0]).hypot(f32::from(p[1])))
        .collect()
}

/// Mean magnitude over the live tone positions of one record.
///
/// The second of the three per-record scalars, and the plainest: it is the
/// received power of the sounding, in the receiver's own units. A chest moving
/// through the path changes it by a fraction of a percent, which is nothing to
/// a motion detector and is exactly the size of thing a thirty-second transform
/// can pull out of the noise.
///
/// Live positions only, for the reason [`MIN_TONE_MAG`] gives. `None` when the
/// payload holds no live position at all, which means it was not a channel
/// response.
pub fn mean_mag(mag: &[f32]) -> Option<f64> {
    let mut acc = 0.0f64;
    let mut used = 0usize;
    for &v in mag {
        let v = f64::from(v);
        if v < MIN_TONE_MAG {
            continue;
        }
        acc += v;
        used += 1;
    }
    (used > 0).then(|| acc / used as f64)
}

/// Mean phase of one record: per chain, the tone phases unwrapped along the
/// tones and stripped of their linear slope, averaged; then averaged over
/// chains.
///
/// # Why the slope has to go
///
/// The raw phase of a CFR capture advances almost linearly with the subcarrier
/// index. That ramp is the receiver's sampling-time offset against the
/// transmitter — a clock artefact — and it is large: a few whole cycles across
/// 256 tones, and a DIFFERENT few after every re-sync of the same completely
/// still link. Left in, it is the entire signal, and the millimetre of chest
/// wall this stage exists to see is four decimal places underneath it. Fitting
/// a least-squares line over the tone index and removing its SLOPE leaves the
/// intercept — the phase common to every subcarrier, which is the path.
///
/// Removing the slope and then averaging over tones is the same arithmetic as
/// taking that intercept, and it is deliberately not the same as removing the
/// whole fitted line: subtracting the intercept too would leave a residual
/// whose mean is exactly zero by construction, i.e. no signal at all.
///
/// # Why the dead tones are skipped in the unwrap and not just in the fit
///
/// Guard and DC subcarriers arrive as near-zero I and Q, so their `atan2` is
/// uniformly random. Unwrapping THROUGH one injects a 2-pi step into every tone
/// after it, which tilts the fitted slope and takes the answer with it. The
/// unwrap therefore steps from live tone to live tone, and the one- to
/// three-tone gaps that leaves are far short of the half-cycle per step the
/// unwrap can tolerate.
///
/// # Why the chains are aligned before they are averaged
///
/// Each chain has its own constant phase offset, so the per-chain intercepts
/// sit anywhere in `(-pi, pi]` and two of them either side of the wrap average
/// to something near zero that is not between them. Each chain is brought to
/// within half a turn of the first before it is added.
///
/// O(chains x tones), one `atan2` per position: the same order as
/// [`magnitudes`], and paid on the same reader thread.
pub fn mean_phase(rec: &Record) -> Option<f64> {
    let chains = rec.chains();
    let tones = rec.tones();
    if chains == 0 || tones == 0 {
        return None;
    }

    let mut acc = 0.0f64;
    let mut used = 0usize;
    let mut reference: Option<f64> = None;
    for c in 0..chains {
        let Some(theta) = chain_phase(rec, c, tones) else {
            continue;
        };
        let theta = match reference {
            None => {
                reference = Some(theta);
                theta
            }
            Some(r) => r + wrap_pi(theta - r),
        };
        acc += theta;
        used += 1;
    }
    (used > 0).then(|| acc / used as f64)
}

/// The slope-free phase of one chain, or `None` if it has fewer than two live
/// tones — which is fewer than a line can be fitted through.
fn chain_phase(rec: &Record, chain: usize, tones: usize) -> Option<f64> {
    let base = chain * tones * 2;
    let (mut n, mut sx, mut sy, mut sxx, mut sxy) = (0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64);
    let mut prev: Option<f64> = None;

    for k in 0..tones {
        let i = f64::from(*rec.iq.get(base + 2 * k)?);
        let q = f64::from(*rec.iq.get(base + 2 * k + 1)?);
        if i.hypot(q) < MIN_TONE_MAG {
            continue;
        }
        let raw = q.atan2(i);
        // Unwrapped against the last LIVE tone: the smallest step congruent to
        // the raw difference is the one a continuous phase would have taken.
        let phi = match prev {
            None => raw,
            Some(p) => p + wrap_pi(raw - p),
        };
        prev = Some(phi);

        let x = k as f64;
        n += 1.0;
        sx += x;
        sy += phi;
        sxx += x * x;
        sxy += x * phi;
    }

    if n < 2.0 {
        return None;
    }
    let denom = n * sxx - sx * sx;
    // Zero only when every live tone is the same tone, which cannot happen
    // here; flat is the honest fallback and costs one comparison.
    let slope = if denom.abs() > f64::EPSILON {
        (n * sxy - sx * sy) / denom
    } else {
        0.0
    };
    // The mean of `phi - slope * x`, which is the fitted line's value at tone
    // zero. See the doc comment on `mean_phase` for why this is the quantity
    // and not the residual.
    Some((sy - slope * sx) / n)
}

/// Fold an angle into `(-pi, pi]`.
///
/// `pub(super)` because [`super::vitals`] needs the same fold to unwrap the
/// per-record scalar along TIME, and two copies of a modular reduction is two
/// chances to get the open end of the interval wrong.
pub(super) fn wrap_pi(a: f64) -> f64 {
    use std::f64::consts::{PI, TAU};
    let mut a = a % TAU;
    if a > PI {
        a -= TAU;
    } else if a <= -PI {
        a += TAU;
    }
    a
}

/// The last N records of one link, and the motion energy over them.
#[derive(Debug, Clone)]
pub struct CsiWindow {
    cap: usize,
    rows: VecDeque<Vec<f32>>,
    /// Running per-position sum and sum of squares over `rows`.
    ///
    /// Maintained on push and pop so the energy costs O(width) per record
    /// instead of O(width x window). At the shipped settings that is the
    /// difference between 20 and 1 passes over a thousand positions, ten times
    /// a second, per link -- on a 400 MHz router with eight links that is the
    /// difference between a few percent of a core and most of one.
    sum: Vec<f64>,
    sumsq: Vec<f64>,
    /// Pushes since the accumulators were last recomputed from `rows`.
    since_rebuild: u32,
}

/// How many incremental updates before the accumulators are rebuilt from the
/// rows they summarise.
///
/// Adding a value and later subtracting it does not return a float exactly, so
/// an accumulator updated forever drifts from the data it claims to describe --
/// and nothing downstream could tell, because the drift is a plausible energy.
/// The periodic rebuild bounds that drift to this many operations' worth
/// (~4e-6 on a `sumsq` of 5e6, against a variance of ~1500) and costs one full
/// pass every 4096 records, which is a few seconds of one link's time per hour.
const REBUILD_EVERY: u32 = 4096;

impl CsiWindow {
    /// A window holding `cap` records, floored at two.
    ///
    /// Two because the quantity is a temporal variance: one record has none,
    /// and a window that reported something from one record would report it
    /// confidently and wrongly.
    pub fn new(cap: usize) -> Self {
        Self {
            cap: cap.max(2),
            rows: VecDeque::new(),
            sum: Vec::new(),
            sumsq: Vec::new(),
            since_rebuild: 0,
        }
    }

    /// Add one record's magnitudes.
    ///
    /// A row of a different width clears the window first. That happens when
    /// the radio changes bandwidth under a running capture, and the two halves
    /// are not comparable: position 200 of a 40 MHz record and position 200 of
    /// an 80 MHz record are different subcarriers of different widths, and
    /// taking their variance together measures the reconfiguration rather than
    /// the room.
    pub fn push(&mut self, row: Vec<f32>) {
        if self.rows.front().is_some_and(|r| r.len() != row.len()) {
            self.clear();
        }
        if self.sum.len() != row.len() {
            self.sum = vec![0.0; row.len()];
            self.sumsq = vec![0.0; row.len()];
            self.rows.clear();
        }
        for (p, &v) in row.iter().enumerate() {
            let v = f64::from(v);
            self.sum[p] += v;
            self.sumsq[p] += v * v;
        }
        self.rows.push_back(row);
        while self.rows.len() > self.cap {
            if let Some(old) = self.rows.pop_front() {
                for (p, &v) in old.iter().enumerate() {
                    let v = f64::from(v);
                    self.sum[p] -= v;
                    self.sumsq[p] -= v * v;
                }
            }
        }
        self.since_rebuild += 1;
        if self.since_rebuild >= REBUILD_EVERY {
            self.rebuild();
        }
    }

    /// Recompute the accumulators from the rows. See [`REBUILD_EVERY`].
    fn rebuild(&mut self) {
        let width = self.sum.len();
        self.sum = vec![0.0; width];
        self.sumsq = vec![0.0; width];
        for row in &self.rows {
            for (p, &v) in row.iter().enumerate().take(width) {
                let v = f64::from(v);
                self.sum[p] += v;
                self.sumsq[p] += v * v;
            }
        }
        self.since_rebuild = 0;
    }

    /// Records currently held.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Is the window empty?
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Is there a full window's worth?
    pub fn is_full(&self) -> bool {
        self.rows.len() >= self.cap
    }

    /// Discard the history, keeping the capacity.
    fn clear(&mut self) {
        self.rows.clear();
        self.sum.clear();
        self.sumsq.clear();
        self.since_rebuild = 0;
    }

    /// Mean over positions of the temporal variance of magnitude divided by
    /// the square of the mean magnitude.
    ///
    /// `None` until the window is full, deliberately. A half-full window has a
    /// variance too, computed over fewer samples and therefore noisier, and
    /// emitting it would hand the detector a stream whose statistics change as
    /// the window fills — the detector would learn an ambient from one
    /// distribution and judge against another.
    ///
    /// Read off the running accumulators, so this is O(width) rather than
    /// O(width x window): the per-position sums are maintained by
    /// [`push`](Self::push) as rows enter and leave.
    ///
    /// The variance therefore comes from `E[x²] - E[x]²`, which subtracts two
    /// nearly equal large numbers -- at rest the mean magnitude is ~500 and the
    /// variance ~1500, so `sumsq/n` is ~251500 against a `mean²` of ~250000.
    /// In `f64` that cancellation costs about three of fifteen significant
    /// digits and leaves the answer good to ~1e-12 relative, which is eight
    /// orders of magnitude below anything the radio can resolve; the
    /// accumulators are `f64` for exactly this reason, even though the rows
    /// they summarise are `f32`. `max(0.0)` catches the one case the
    /// cancellation can get wrong in sign: a perfectly still position, where
    /// the true variance is zero and the subtraction may land a hair below it.
    ///
    /// Asserted against the naive two-pass form on real capture data in
    /// `the_incremental_energy_matches_the_two_pass_form`.
    pub fn motion_energy(&self) -> Option<f64> {
        if !self.is_full() {
            return None;
        }
        let n = self.rows.len();
        let width = self.sum.len();
        if n < 2 || width == 0 {
            return None;
        }

        let n = n as f64;
        let mut acc = 0.0f64;
        let mut used = 0usize;
        for p in 0..width {
            let mean = self.sum[p] / n;
            if mean < MIN_TONE_MAG {
                continue;
            }
            let var = (self.sumsq[p] / n - mean * mean).max(0.0);
            acc += var / (mean * mean);
            used += 1;
        }
        if used == 0 {
            // Every position was a guard tone, which means the payload was not
            // a channel response at all.
            return None;
        }
        Some(acc / used as f64)
    }

    /// The same quantity computed from the rows in two passes, with no running
    /// state. The reference [`motion_energy`](Self::motion_energy) is checked
    /// against; not compiled into the agent.
    #[cfg(test)]
    fn motion_energy_two_pass(&self) -> Option<f64> {
        if !self.is_full() {
            return None;
        }
        let n = self.rows.len();
        let width = self.rows.front()?.len();
        if n < 2 || width == 0 {
            return None;
        }
        let mut acc = 0.0f64;
        let mut used = 0usize;
        for p in 0..width {
            let mut sum = 0.0f64;
            for row in &self.rows {
                sum += f64::from(*row.get(p)?);
            }
            let mean = sum / n as f64;
            if mean < MIN_TONE_MAG {
                continue;
            }
            let mut var = 0.0f64;
            for row in &self.rows {
                let d = f64::from(*row.get(p)?) - mean;
                var += d * d;
            }
            acc += (var / n as f64) / (mean * mean);
            used += 1;
        }
        (used > 0).then(|| acc / used as f64)
    }

    /// The energy in dB, clamped to [`ENERGY_DB_MIN`]..=[`ENERGY_DB_MAX`].
    ///
    /// dB because the detector's thresholds are all ratios and absolute
    /// variances in dB, and because the energy spans orders of magnitude while
    /// the interesting changes are multiplicative: a room going from empty to
    /// occupied multiplies it, it does not add to it.
    pub fn motion_energy_db(&self) -> Option<f64> {
        let e = self.motion_energy()?;
        if e <= 0.0 {
            // A perfectly still window. Reported at the floor rather than as
            // -inf, for the reason on the constant.
            return Some(ENERGY_DB_MIN);
        }
        Some((10.0 * e.log10()).clamp(ENERGY_DB_MIN, ENERGY_DB_MAX))
    }
}

/// A detector tuned for a motion-energy input rather than an RSSI one.
///
/// Only one field moves. `saturation_dbm` asks whether a link is so loud that
/// the receiver's own gain stage is flickering, which is a question about an
/// absolute signal level; a motion energy has no such level, so the gate is
/// disabled outright rather than given a number that happens never to trigger.
/// Infinity says that in the code: no finite reading is greater than it, and
/// nobody reading this later has to work out whether some large constant was a
/// threshold or a disablement.
///
/// Every other threshold carries over unchanged and unconverted, because every
/// other threshold is in dB or is a ratio of variances in dB². `var_floor` at
/// 0.25 dB² and `min_short_var` at 1.0 dB² are as right for an energy that
/// sits within 0.6 dB of itself at rest as they were for a quantised RSSI.
fn csi_detector() -> LinkDetector {
    LinkDetector::unsaturating()
}

// ── The live table ───────────────────────────────────────────────────────────

/// One tracked (radio, peer) pair.
struct CsiLink {
    /// Assigned once, kept for the life of the link. See the module docs.
    instance: u32,
    phy: String,
    iface: String,
    /// Display spelling, upper case, as `Peer`.
    peer: String,
    /// Wire spelling, which is what records are keyed by.
    key: [u8; 6],
    kind: LinkKind,
    detector: LinkDetector,
    /// Records accepted for this link since it was first seen.
    records: u64,
    /// Arrival times of the recent records, pruned to the last second. Kept as
    /// times rather than as a counter and a deadline so `RecordsPerSec` is the
    /// rate over the last second at the instant it is read, not the count
    /// since some boundary the controller cannot see.
    arrivals: VecDeque<f64>,
    /// Framing failures on the RELAY this link's records came off.
    ///
    /// Per link because that is where a controller can act on it, and stream-
    /// level because that is the only place it exists: a record whose end
    /// magic is wrong has no trustworthy peer address, so it cannot be
    /// attributed to one. Two links on the same radio therefore report the
    /// same number, and that is the honest reading of it — "the relay feeding
    /// this link is dropping frames".
    errors: u32,
    resyncs: u32,
    /// Capture bandwidth code from the most recent record.
    bw: u8,
    /// Most recent energy, kept for the GET so a controller can see why.
    energy_db: Option<f64>,
    last_seen: f64,
    last_motion_at: Option<String>,
    /// The last estimate, copied in by the run loop after it ran the transform
    /// OUTSIDE the lock. See [`WinState::vitals`].
    vitals: Vitals,
    /// The window the estimate was made over, kept so [`render`] can tell a
    /// current estimate from one this link has stopped producing.
    vitals_span: f64,
    /// When this link's detector last entered `Idle`, or `None` while it is in
    /// any other state.
    ///
    /// Kept here rather than asked of the detector because the detector knows
    /// what state it is in and not how long it has been in it, and the vitals
    /// gate is entirely about the duration: a link that stopped moving one
    /// record ago still has a window full of the person who was moving.
    idle_since: Option<f64>,
    fall: FallDetector,
    last_fall_at: Option<String>,
}

impl CsiLink {
    /// One Notify-worthy change on this link.
    fn change(&self, param: &'static str, value: &'static str) -> Change {
        Change {
            instance: self.instance,
            iface: self.iface.clone(),
            peer: self.peer.clone(),
            param,
            value,
        }
    }
}

/// Every CSI link currently tracked. A `static` for the same reason
/// `motion::LINKS` is: the reader and `dm::get` have nowhere else to meet.
static LINKS: Mutex<Option<Vec<CsiLink>>> = Mutex::new(None);

/// Next free instance number. Never reused. See the module docs.
static NEXT_INSTANCE: AtomicU32 = AtomicU32::new(1);

/// Take the table, surviving a poisoned lock: a panic elsewhere must not turn
/// every later GET into a panic of its own.
fn links() -> std::sync::MutexGuard<'static, Option<Vec<CsiLink>>> {
    LINKS.lock().unwrap_or_else(|e| e.into_inner())
}

/// How long a peer may be missing before its link is dropped. Matches
/// `motion`'s, so the two objects age a peer out together.
const STALE_SECS: f64 = 60.0;

/// How often peers are rediscovered and captures reconciled.
const REDISCOVER: Duration = Duration::from_secs(5);

/// How soon to retry after a discovery that found no radios at all. Sooner
/// than a successful pass, so a device whose radios are still coming up starts
/// capturing promptly. See `discover_peers`.
const REDISCOVER_RETRY: Duration = Duration::from_secs(1);

/// How often the tokio side drains the reader threads.
///
/// Well under a second so `RecordsPerSec` is a rate rather than a sawtooth,
/// and well over the record period so each drain moves a batch rather than
/// taking the lock ten times a second per radio.
const DRAIN: Duration = Duration::from_millis(250);

/// How long the reader waits after a read returns EOF before reading again.
///
/// The relay is still live at EOF — see the module docs — so this is a poll
/// interval, not a backoff. 50 ms is short enough that the 255-sub-buffer
/// backlog is never approached at the fastest supported period and long enough
/// that an idle radio does not spin a core.
const RELAY_POLL: Duration = Duration::from_millis(50);

/// How long the reader waits before reopening a relay file that failed, and
/// the ceiling that wait climbs to.
const REOPEN_BACKOFF: Duration = Duration::from_secs(2);
const REOPEN_BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Least often a repeated open failure is logged, once the complaint has
/// backed off all the way.
const OPEN_WARN_MAX: Duration = Duration::from_secs(600);

/// Retry and complaint intervals for a relay file that will not open.
///
/// Two curves, not one: the RETRY climbs to a minute so a radio that comes
/// back is picked up promptly, while the COMPLAINT climbs to ten minutes
/// because after the first line there is nothing new to say. Collapsing them
/// into one interval forces a choice between noticing the radio late and
/// filling the log.
struct Backoff {
    attempts: u32,
    wait: Duration,
    next_log: Instant,
    log_gap: Duration,
}

impl Backoff {
    fn new() -> Self {
        Self {
            attempts: 0,
            wait: REOPEN_BACKOFF,
            // Zero so the FIRST failure is always logged: that is the line
            // that says when it started, and it is the one worth having.
            next_log: Instant::now(),
            log_gap: REOPEN_BACKOFF,
        }
    }

    /// Is this failure due to be logged?
    fn should_log(&mut self) -> bool {
        self.attempts += 1;
        if Instant::now() < self.next_log {
            return false;
        }
        self.next_log = Instant::now() + self.log_gap;
        self.log_gap = (self.log_gap * 4).min(OPEN_WARN_MAX);
        true
    }

    /// How long to wait before trying again.
    fn next_wait(&mut self) -> Duration {
        let w = self.wait;
        self.wait = (self.wait * 2).min(REOPEN_BACKOFF_MAX);
        w
    }

    /// The file opened; forget the failures.
    fn reset(&mut self) {
        if self.attempts > 0 {
            self.attempts = 0;
            self.wait = REOPEN_BACKOFF;
            self.log_gap = REOPEN_BACKOFF;
            self.next_log = Instant::now();
        }
    }
}

/// Most peers captured at once, across all radios.
///
/// A cap, not a tuning knob. Every captured peer costs one record per period
/// through a relay shared by the whole radio: at the default 100 ms that is
/// 42 KB/s each, and the driver's buffer for the radio is 255 x 16.7 KB. A
/// busy AP with thirty associated clients would ask for 1.2 MB/s of soundings,
/// overrun the relay, and lose captures from every peer including the mesh
/// links that are worth having — and nothing in the record stream would say
/// so. Eight is comfortably inside the budget and covers every mesh peer a
/// node realistically has.
const MAX_CAPTURES: usize = 8;

/// Largest carry a reader will hold before giving up on it.
///
/// The carry exists because records straddle reads. It should never exceed one
/// record, and it grows without bound if a stream is producing bytes that
/// never frame — which is what a driver writing a format this parser does not
/// know looks like. Dropped and counted as a resync rather than grown, because
/// a reader thread quietly eating a router's RAM is a worse failure than a lost
/// second of captures.
const MAX_CARRY: usize = 4 * MAX_PAYLOAD;

/// One record, reduced to what the feature needs.
///
/// The I/Q is converted to magnitudes on the READER thread and the record is
/// dropped there. That is where the square roots belong — off the tokio
/// runtime — and it means the queue between the threads holds one vector per
/// record instead of a record plus its payload.
struct Sample {
    peer: [u8; 6],
    mag: Vec<f32>,
    bw: u8,
    /// When this record was captured, on the shared process clock.
    ///
    /// Not simply the time the read returned. One `read` of the relay commonly
    /// hands over several records that the radio captured up to a period
    /// apart -- at the default 100 ms, a drain can carry three records spanning
    /// 300 ms -- and stamping them all with the read time tells the detector
    /// that three soundings happened at once. That matters twice over: the
    /// dwell and gap arithmetic are in real seconds, and `RecordsPerSec` would
    /// report bursts of three rather than a steady ten.
    ///
    /// The spacing comes from the driver's own `meta.timestamp`, which is a
    /// microsecond counter -- measured at ~103000 between records on a 100 ms
    /// capture -- anchored so that the LAST record of a read lands at the read
    /// time. The hardware supplies the intervals, the process clock supplies
    /// the epoch, and neither is asked for what it does not know.
    at: f64,
    /// [`mean_mag`] of this record, and [`mean_phase`] of it.
    ///
    /// Computed on the READER thread beside the magnitudes, for the same reason
    /// they are: the `atan2` per position belongs off the tokio runtime, and
    /// carrying two `f64` costs the queue nothing where carrying the I/Q would
    /// cost it 8 KB a record.
    ///
    /// `None` on a payload with no live tone, which is a payload that was not a
    /// channel response. Not fabricated as zero: a zero mean phase is a real
    /// phase a real link could have.
    mean_mag: Option<f64>,
    mean_phase: Option<f64>,
}

/// What one radio's reader has produced since the last drain.
#[derive(Default)]
struct Inbox {
    samples: Vec<Sample>,
    /// Cumulative for the life of the reader, never reset by a drain: these
    /// are reported as counters and a counter that resets is a rate nobody
    /// asked for.
    errors: u32,
    resyncs: u32,
    /// Samples discarded because the tokio side stopped draining.
    dropped: u64,
}

/// The hand-off between the reader threads and the agent task.
static INBOX: Mutex<Option<HashMap<String, Inbox>>> = Mutex::new(None);

/// Most samples one radio may queue between drains.
///
/// At the fastest period and the capture cap that is 8 peers x 50 records/s,
/// so four drains' worth is ~400. 2048 leaves room for a stalled runtime and
/// still bounds the queue at ~8 MB of magnitudes in the worst case. Beyond it
/// the OLDEST are dropped: if the agent has fallen behind, the recent channel
/// is what a detector can still use and the stale one is only a delay.
const INBOX_CAP: usize = 2048;

fn inbox() -> std::sync::MutexGuard<'static, Option<HashMap<String, Inbox>>> {
    INBOX.lock().unwrap_or_else(|e| e.into_inner())
}

/// Hand one radio's framed records to the agent task.
///
/// Called from a reader thread with no file open and nothing borrowed: the
/// lock is taken, a vector is appended, and the lock is released. Holding it
/// across the read would serialise every radio behind the slowest one.
fn deliver(phy: &str, samples: Vec<Sample>, errors: u32, resyncs: u32) {
    let mut guard = inbox();
    let map = guard.get_or_insert_with(HashMap::new);
    let slot = map.entry(phy.to_owned()).or_default();
    slot.errors = errors;
    slot.resyncs = resyncs;
    slot.samples.extend(samples);
    if slot.samples.len() > INBOX_CAP {
        let excess = slot.samples.len() - INBOX_CAP;
        slot.samples.drain(..excess);
        slot.dropped += excess as u64;
        // Logged rather than only counted: this means the agent task stopped
        // draining, which is a fault in the agent and not in the radio, and
        // nothing in the data model would otherwise show it -- the links go
        // on reporting a plausible rate from the records that did survive.
        warn!(
            "csi: {phy} drain fell behind, dropped {excess} records ({} total)",
            slot.dropped
        );
    }
}

/// Drop everything the readers have queued.
///
/// Called once when a run starts. See the call site for why a previous run's
/// records must not be replayed as this run's.
fn clear_inbox() {
    if let Some(map) = inbox().as_mut() {
        map.clear();
    }
}

/// One radio's worth of drained records.
struct Batch {
    phy: String,
    samples: Vec<Sample>,
    errors: u32,
    resyncs: u32,
}

/// Take everything the readers have produced.
fn drain_inbox() -> Vec<Batch> {
    let mut guard = inbox();
    let Some(map) = guard.as_mut() else {
        return Vec::new();
    };
    map.iter_mut()
        .filter(|(_, v)| !v.samples.is_empty())
        .map(|(phy, v)| Batch {
            phy: phy.clone(),
            samples: std::mem::take(&mut v.samples),
            errors: v.errors,
            resyncs: v.resyncs,
        })
        .collect()
}

/// Which interface and which side of the radio a captured peer is on.
type PeerMap = HashMap<(String, [u8; 6]), (String, LinkKind)>;

/// How a link's windows are keyed: the radio it is on and the peer it is with.
type WinKey = (String, [u8; 6]);

/// The vitals and fall settings a run was started with.
///
/// Carried into [`apply`] rather than read from the config there, for the same
/// reason the table is a parameter: `apply` is the lifecycle and it is tested
/// without a config, a radio, or a process. One struct rather than four
/// arguments because it is passed through unchanged, and a four-argument tail
/// of `f64` is where a transposed pair hides.
#[derive(Debug, Clone, Copy)]
struct Tuning {
    /// Seconds of scalars behind an estimate, and the stillness required.
    vitals_span: f64,
    /// In-band power share below which nothing is reported.
    min_confidence: f64,
    /// dB above the ambient that opens a fall candidate.
    fall_rise_db: f64,
    /// Seconds of stillness before a candidate is raised.
    fall_still_secs: f64,
    /// The sounding period in seconds, which is what the vitals ring measures
    /// an unacceptable gap against.
    period_secs: f64,
}

impl Tuning {
    /// What `cfg` asks for. The clamping happened in [`crate::config`]; this is
    /// only the widening.
    fn from_config(cfg: &ClientConfig) -> Self {
        Self {
            vitals_span: cfg.csi_vitals_window_secs as f64,
            min_confidence: cfg.csi_vitals_min_confidence,
            fall_rise_db: cfg.csi_fall_rise_db,
            fall_still_secs: cfg.csi_fall_still_secs as f64,
            period_secs: cfg.csi_period_ms as f64 / 1000.0,
        }
    }
}

impl Default for Tuning {
    /// The shipped defaults, so a test that is not about tuning does not have
    /// to state it.
    fn default() -> Self {
        Self::from_config(&ClientConfig::default())
    }
}

/// One parameter change worth a Notify.
///
/// A struct rather than the tuple this used to be: there are now two parameters
/// that notify, `State` and `FallCandidate`, and a bare
/// `(u32, String, String, &str, &str)` puts the leaf name and the value next to
/// each other as two anonymous strings.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Change {
    instance: u32,
    iface: String,
    peer: String,
    /// The leaf under `Device.X_OptimACS_Sensing.Csi.{i}.`
    param: &'static str,
    value: &'static str,
}

/// One link's feature window, and when it last had a record.
///
/// Owned by the run loop and NOT by the shared table. The windows are the
/// large part of this module's memory -- a thousand `f32` per record, twenty
/// records per link -- and nothing outside the drain ever reads them: the data
/// model wants the scalar energy, not the channel it came from. Keeping them
/// out of `LINKS` means a GET never touches them, and the lock is held only
/// for the table update itself.
struct WinState {
    window: CsiWindow,
    /// Half a minute of per-record scalars, and the transform over them.
    ///
    /// Here and NOT on the link, which is the object the data-model GET locks.
    /// The transform is the one part of this module that is not O(1) per
    /// record, and it allocates; running it inside `apply` meant running it
    /// while holding the mutex a GET contends for, which is exactly what the
    /// split between `features` and `apply` exists to prevent. The link keeps
    /// only the four scalars the estimate produced.
    vitals: VitalsWindow,
    last_at: f64,
}

/// One record, reduced to the scalar the detector consumes.
struct Feature {
    phy: String,
    peer: [u8; 6],
    bw: u8,
    at: f64,
    /// The window's energy once it is full, in dB. `None` while it fills.
    energy_db: Option<f64>,
    /// This record followed a gap, so the detector must forget its short
    /// history before taking it. See [`features`].
    resync: bool,
}

/// Turn a drain into features, holding no lock.
///
/// This is the expensive half of a tick -- a thousand square roots per record
/// were already paid on the reader thread, and this adds the window arithmetic
/// -- and it deliberately happens before `LINKS` is touched at all. `apply`
/// below then does nothing but table bookkeeping and detector state, so the
/// mutex a data-model GET contends for is held for a few microseconds rather
/// than for the whole drain.
///
/// Returns the features and, per radio, its cumulative framing counters.
fn features(
    batches: Vec<Batch>,
    windows: &mut HashMap<WinKey, WinState>,
    tune: &Tuning,
    cap: usize,
    now: f64,
) -> (Vec<Feature>, Vec<(String, u32, u32)>) {
    let mut feats = Vec::new();
    let mut streams = Vec::new();

    for batch in batches {
        streams.push((batch.phy.clone(), batch.errors, batch.resyncs));
        for sample in batch.samples {
            let key = (batch.phy.clone(), sample.peer);
            let st = windows.entry(key).or_insert_with(|| WinState {
                window: CsiWindow::new(cap),
                vitals: VitalsWindow::new(
                    tune.vitals_span,
                    tune.min_confidence,
                    tune.period_secs,
                ),
                last_at: sample.at,
            });

            // A peer that stopped reporting for a while has a hole in its
            // history, and joining the two sides of it puts a step the size of
            // the whole channel change inside the feature window -- which is
            // the shape motion has. The window is emptied and the detector
            // told to forget its short history, for exactly the reason
            // `motion::LinkDetector::resync` gives; the learnt ambient
            // survives, because the room did not change while nobody was
            // reporting from it.
            let resync = sample.at - st.last_at > STALE_GAP_SECS;
            if resync {
                st.window.clear();
                // The vitals ring has its own, much shorter, gap rule -- see
                // `vitals::MAX_GAP_PERIODS` -- and would clear itself on the
                // next push anyway. Cleared here too so that the two windows
                // are provably in step after a resync rather than one push out.
                st.vitals.clear();
            }
            st.last_at = sample.at;
            // The vitals ring takes EVERY record, including those taken while
            // the energy window is still filling: the ring is half a minute
            // long and the energy window is two seconds, so refusing the first
            // twenty would delay nothing and put a condition on the one path
            // that has to stay O(1) per record.
            if let (Some(m), Some(p)) = (sample.mean_mag, sample.mean_phase) {
                st.vitals.push(sample.at, m, p);
            }
            st.window.push(sample.mag);

            feats.push(Feature {
                phy: batch.phy.clone(),
                peer: sample.peer,
                bw: sample.bw,
                at: sample.at,
                energy_db: st.window.motion_energy_db(),
                resync,
            });
        }
    }

    // The windows age out with the links they belong to, or a peer that left
    // leaves 80 KB of magnitudes behind for the life of the process.
    windows.retain(|_, w| now - w.last_at <= STALE_SECS);
    (feats, streams)
}

/// Run the vitals transform for every link, holding NO lock.
///
/// Called between the two lock acquisitions of a tick, on the windows the run
/// loop owns, using the `idle_since` that was read off the detectors under the
/// first one. Being one drain (250 ms) behind the detector costs nothing: the
/// gate the value feeds is thirty seconds of stillness.
///
/// Returns only the links whose reported value CHANGED, which on the great
/// majority of ticks is none of them -- the estimate is gated to once every two
/// seconds and the drain runs four times a second. That is what lets the
/// storing pass below skip its lock entirely most of the time.
fn estimate_vitals(
    windows: &mut HashMap<WinKey, WinState>,
    idle: &[(WinKey, Option<f64>)],
    now: f64,
) -> Vec<(WinKey, Vitals)> {
    let mut out = Vec::new();
    for (key, since) in idle {
        if let Some(st) = windows.get_mut(key) {
            if let Some(v) = st.vitals.update(now, *since) {
                out.push((key.clone(), v));
            }
        }
    }
    out
}

/// Copy finished estimates onto their links. Four scalars each; no transform,
/// no allocation, and therefore a lock held for as long as a few moves.
fn store_vitals(table: &mut [CsiLink], vitals: &[(WinKey, Vitals)]) {
    for (key, v) in vitals {
        if let Some(link) = table.iter_mut().find(|l| l.phy == key.0 && l.key == key.1) {
            link.vitals = *v;
        }
    }
}

/// The wall-clock spelling of a time on the shared process clock.
///
/// The process clock is monotonic and has no epoch, so a time on it can only be
/// named by how far back it is from a time that does. Floored at zero: a record
/// stamped after `now` is a clamp artefact, not the future.
fn wall_clock(at: f64, now: f64) -> String {
    let back = ((now - at).max(0.0) * 1000.0).min(f64::from(i32::MAX)) as i64;
    (chrono::Utc::now() - chrono::Duration::milliseconds(back)).to_rfc3339()
}

/// Feed a tick's features into `table` and return the state changes.
///
/// Pure, and takes the table rather than reaching for the `static`, for the
/// same two reasons `motion::apply` does: it makes it provable that the lock
/// is not held across an await or a file read, and it makes the whole
/// lifecycle testable without a radio.
///
/// MUST BE CALLED EVERY TICK, including when nothing arrived. The eviction at
/// the bottom is the only thing that retracts a link that vanished mid-Motion,
/// and a link vanishes precisely when its records stop -- so skipping this on
/// an empty drain is skipping it exactly when it is needed.
fn apply(
    table: &mut Vec<CsiLink>,
    feats: Vec<Feature>,
    streams: &[(String, u32, u32)],
    peers: &PeerMap,
    tune: &Tuning,
    now: f64,
) -> Vec<Change> {
    let mut changes = Vec::new();

    for f in feats {
        let idx = match table
            .iter()
            .position(|l| l.phy == f.phy && l.key == f.peer)
        {
            Some(i) => i,
            None => {
                // The interface and kind come from the discovery pass that
                // started this capture, so they are known by the time a record
                // arrives. A peer that is NOT in the map is one the agent did
                // not ask for -- `cfr_unassoc` traffic, or a capture left
                // running by a previous process -- and it is reported under the
                // radio's own name rather than dropped, because a record
                // arriving from a peer nobody started is something an operator
                // should be able to see.
                let (iface, kind) = peers
                    .get(&(f.phy.clone(), f.peer))
                    .cloned()
                    .unwrap_or_else(|| (f.phy.clone(), LinkKind::Client));
                table.push(CsiLink {
                    instance: NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed),
                    phy: f.phy.clone(),
                    iface,
                    peer: mac_upper(&f.peer),
                    key: f.peer,
                    kind,
                    detector: csi_detector(),
                    records: 0,
                    arrivals: VecDeque::new(),
                    errors: 0,
                    resyncs: 0,
                    bw: f.bw,
                    energy_db: None,
                    last_seen: f.at,
                    last_motion_at: None,
                    vitals: Vitals::default(),
                    vitals_span: tune.vitals_span,
                    idle_since: None,
                    fall: FallDetector::new(tune.fall_rise_db, tune.fall_still_secs),
                    last_fall_at: None,
                });
                table.len() - 1
            }
        };
        let link = &mut table[idx];

        // `features` emptied the window; the detector has its own short
        // history to forget, and only it can do that.
        if f.resync {
            debug!("csi: {} {} resyncing after a gap", link.phy, link.peer);
            link.detector.resync();
            link.idle_since = None;
            link.vitals = Vitals::default();
            // A sequence in progress is a claim about the last few seconds, and
            // after a hole there is nothing to claim. Without this a `Still`
            // that began before a twenty-second gap matures on the first record
            // after it and reports a fall nothing watched -- and it does so
            // while `energy_db` is `None` through the refill, so the detector
            // never even saw the records in between.
            link.fall.reset();
        }

        link.last_seen = f.at.max(link.last_seen);
        link.records += 1;
        link.bw = f.bw;
        link.arrivals.push_back(f.at);
        while link.arrivals.front().is_some_and(|&t| f.at - t > 1.0) {
            link.arrivals.pop_front();
        }

        let Some(db) = f.energy_db else {
            continue;
        };
        link.energy_db = Some(db);
        match link.detector.push_f64(db, f.at) {
            Some(Transition::ToMotion) => {
                link.last_motion_at = Some(wall_clock(f.at, now));
                changes.push(link.change("State", "Motion"));
            }
            Some(Transition::ToIdle) => {
                changes.push(link.change("State", "Idle"));
            }
            None => {}
        }

        // Read AFTER the detector has had this record: the gate is a question
        // about the state this very record produced, not the one before it.
        if link.detector.state() == State::Idle {
            link.idle_since.get_or_insert(f.at);
        } else {
            link.idle_since = None;
        }

        // The fall stage runs on the energy the detector just judged, against
        // the ambient the detector learnt -- `None` while it is still learning,
        // which is what stops every link firing on every boot.
        if let Some(event) = link.fall.push(db, link.detector.baseline_dbm(), f.at) {
            let value = match event {
                FallEvent::Raised => {
                    // Stamped from the RECORD's time, not from the drain's.
                    // A drain runs a quarter-second after the records it
                    // carries, a fall matures `still_secs` after the impact,
                    // and a backlog can be seconds deep -- so `Utc::now()` here
                    // is the time the agent noticed, which is not what anybody
                    // reading `LastFallAt` is asking.
                    link.last_fall_at = Some(wall_clock(f.at, now));
                    "1"
                }
                FallEvent::Cleared => "0",
            };
            changes.push(link.change("FallCandidate", value));
        }
    }

    // The relay's framing counters, copied onto every link fed by that relay.
    // Done outside the feature loop so they land even on a tick that carried
    // no records -- which is exactly the tick on which a relay handing over
    // nothing but rubbish is worth seeing.
    for (phy, errors, resyncs) in streams {
        for link in table.iter_mut().filter(|l| l.phy == *phy) {
            link.errors = *errors;
            link.resyncs = *resyncs;
        }
    }

    // A link dropped mid-Motion is retracted first, for the reason
    // `motion::apply` spells out: the instance is about to leave the GET tree,
    // so "Motion" would be the controller's last and uncorrectable word on it.
    for link in table.iter() {
        if now - link.last_seen > STALE_SECS && link.detector.state() == State::Motion {
            changes.push(link.change("State", "Idle"));
        }
        // A fall candidate is retracted on the way out for the same reason:
        // "somebody fell here" must not be a controller's last and
        // uncorrectable word on an instance that is about to disappear.
        if now - link.last_seen > STALE_SECS && link.fall.candidate() {
            changes.push(link.change("FallCandidate", "0"));
        }
    }
    table.retain(|l| now - l.last_seen <= STALE_SECS);
    changes
}

/// How long a link may go without a record before its history is treated as
/// broken rather than merely sparse.
///
/// Generous relative to the record period because a missed sounding is
/// ordinary — the peer has to be awake and answering a QoS NULL for the
/// firmware to have anything to measure — while a genuine gap is seconds long.
const STALE_GAP_SECS: f64 = 2.0;

/// The data-model spelling of a peer address.
fn mac_upper(mac: &[u8; 6]) -> String {
    mac.iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// The debugfs spelling of a peer address, which is lower case.
fn mac_lower(mac: &[u8; 6]) -> String {
    mac.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Parse a MAC as `iw` prints it. `None` on anything that is not one.
fn mac_bytes(s: &str) -> Option<[u8; 6]> {
    let mut out = [0u8; 6];
    let mut parts = s.split(':');
    for slot in out.iter_mut() {
        *slot = u8::from_str_radix(parts.next()?, 16).ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some(out)
}

// ── The radio ────────────────────────────────────────────────────────────────

/// Where the driver exposes its CFR controls.
const DEBUGFS: &str = "/sys/kernel/debug/ieee80211";

/// The capture bandwidth assumed when nothing better is known.
///
/// 80 MHz is the width of the radio this feature exists for. Defaulting to 20
/// would ask the driver for a quarter of the subcarriers, which reads as a
/// working sensor at a quarter of the resolution rather than as a failure.
const DEFAULT_BW: u8 = 2;

/// Bandwidth in MHz for a capture bandwidth code.
fn bw_mhz(code: u8) -> u32 {
    match code {
        0 => 20,
        1 => 40,
        3 => 160,
        // 2 is 80 MHz and is also the default elsewhere in this module, so an
        // unknown code reads as 80 rather than as 0. A wrong width in a report
        // is a cosmetic error; a 0 would look like a radio that is off.
        _ => 80,
    }
}

/// The phys whose driver offers CFR at all.
///
/// Only `ath11k` exposes `enable_cfr`, and only on hardware that supports it,
/// so the presence of the file is the whole test. A device with a 2.4 GHz
/// radio that cannot do CFR and a 5 GHz radio that can will find exactly one.
fn discover_cfr_phys() -> Vec<String> {
    let Ok(dir) = std::fs::read_dir(DEBUGFS) else {
        return Vec::new();
    };
    let mut phys: Vec<String> = dir
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.starts_with("phy")
                .then(|| Path::new(DEBUGFS).join(&name).join("ath11k/enable_cfr"))
                .filter(|p| p.exists())
                .map(|_| name)
        })
        .collect();
    // Sorted so a device with two capable radios enables them in the same
    // order every boot, which is what makes the instance numbers reproducible
    // enough to reason about in a bug report.
    phys.sort();
    phys
}

/// Turn CFR capture on or off for a whole radio.
///
/// Writing 1 is what makes the driver allocate the relay — about 5 MB per
/// radio — so it is done once when sensing starts and undone when it stops,
/// never per peer.
fn set_enable_cfr(phy: &str, on: bool) {
    let path = Path::new(DEBUGFS).join(phy).join("ath11k/enable_cfr");
    let v = if on { "1" } else { "0" };
    if let Err(e) = std::fs::write(&path, v) {
        warn!("csi: cannot write {v} to {}: {e}", path.display());
    }
}

/// Path of a station's capture control.
fn capture_path(phy: &str, iface: &str, mac: &[u8; 6]) -> PathBuf {
    Path::new(DEBUGFS)
        .join(phy)
        .join(format!("netdev:{iface}"))
        .join("stations")
        .join(mac_lower(mac))
        .join("cfr_capture")
}

/// Start or stop a periodic capture for one peer.
///
/// The four fields are start/stop, bandwidth code, period in ms, and method.
/// Method 0 is the ACK of a QoS NULL: the radio sends the peer a frame it must
/// acknowledge and measures the channel from the acknowledgement, which is
/// what makes this work on a peer that is sending no traffic of its own.
fn set_capture(phy: &str, iface: &str, mac: &[u8; 6], bw: u8, period_ms: u64, on: bool) -> bool {
    let path = capture_path(phy, iface, mac);
    let cmd = format!("{} {} {} 0", u8::from(on), bw, period_ms);
    match std::fs::write(&path, &cmd) {
        Ok(()) => true,
        Err(e) => {
            // Expected and unremarkable when a peer left between the station
            // dump and this write, which is why it is debug and not a warning.
            debug!("csi: cannot write '{cmd}' to {}: {e}", path.display());
            false
        }
    }
}

/// One capture the agent wants running.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Want {
    phy: String,
    iface: String,
    mac: [u8; 6],
    bw: u8,
    kind: LinkKind,
}

/// The key a running capture is tracked by: everything but its parameters.
type CapKey = (String, String, [u8; 6]);

/// Decide which captures to start and which to stop.
///
/// Pure, and separated from the writes because this is the part with a bug in
/// it if there is one — the diff, the cap, and the restart-on-bandwidth-change
/// — while the writes are four `fs::write` calls that either work or do not.
///
/// A peer whose BANDWIDTH changed is stopped and started rather than left
/// alone: the driver keeps the parameters from the original write, so a radio
/// moved from 80 to 40 MHz would go on producing 80 MHz-shaped records that no
/// longer describe the channel.
fn plan(
    desired: Vec<Want>,
    active: &HashMap<CapKey, u8>,
) -> (Vec<Want>, Vec<CapKey>) {
    // Sorted before the cap so that which peers are dropped is stable across
    // ticks. An unstable cap would start and stop the same peers alternately,
    // which costs the relay two writes a tick and every dropped peer its whole
    // learnt baseline.
    //
    // Ordered by IDENTITY -- radio, interface, address -- and not by the whole
    // struct. Bandwidth and kind are properties of a capture, not of which
    // peer it is, and letting them into the ordering would move a peer up or
    // down the list when its radio was retuned: the cap would then drop a
    // different peer for a reason that has nothing to do with the peer.
    let mut desired = desired;
    let ident = |w: &Want| (w.phy.clone(), w.iface.clone(), w.mac);
    desired.sort_by_key(ident);
    desired.dedup_by_key(|w| ident(w));
    desired.truncate(MAX_CAPTURES);

    let wanted: HashMap<CapKey, u8> = desired
        .iter()
        .map(|w| ((w.phy.clone(), w.iface.clone(), w.mac), w.bw))
        .collect();

    let starts: Vec<Want> = desired
        .into_iter()
        .filter(|w| {
            active
                .get(&(w.phy.clone(), w.iface.clone(), w.mac))
                .is_none_or(|&bw| bw != w.bw)
        })
        .collect();

    let stops: Vec<CapKey> = active
        .keys()
        .filter(|k| wanted.get(*k).is_none_or(|&bw| bw != active[*k]))
        .cloned()
        .collect();

    (starts, stops)
}

/// The AP and mesh interfaces of each phy, as `iw dev` lists them.
///
/// Both kinds are taken, unlike `motion::parse_ap_ifaces` which takes only AP
/// because it samples the mesh point through a different path. Here the two
/// are the same operation on a different directory, and the mesh point is the
/// MORE interesting one: both ends are ours and both are stationary, so
/// everything the channel does is the room.
///
/// The phy is carried along because a capture's control file lives under the
/// phy's debugfs directory, not the interface's, and `iw dev` is the only
/// thing that knows which interface belongs to which phy.
fn parse_dev_phys(text: &str) -> Vec<(String, String, LinkKind)> {
    let mut out = Vec::new();
    let mut phy: Option<String> = None;
    let mut iface: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(n) = line.strip_prefix("phy#") {
            phy = Some(format!("phy{}", n.trim()));
        } else if let Some(name) = line.strip_prefix("Interface ") {
            iface = Some(name.trim().to_owned());
        } else {
            // `type AP/VLAN` and `type managed` are other things and must not
            // match: the first is a virtual interface with no stations of its
            // own, the second is a client we are not the AP for.
            let kind = match line {
                "type AP" => LinkKind::Client,
                "type mesh point" => LinkKind::Mesh,
                _ => continue,
            };
            if let (Some(p), Some(i)) = (phy.clone(), iface.take()) {
                out.push((p, i, kind));
            }
        }
    }
    out
}

/// The capture bandwidth code for an interface, from `iw dev <iface> info`.
///
/// Defaults to 80 MHz when the width cannot be read. That is the width of the
/// radio this feature exists for, and the alternative — defaulting to 20 —
/// would ask the driver for a quarter of the subcarriers on every radio whose
/// `iw` output changed format, which reads as a working sensor with a quarter
/// of the resolution rather than as a failure.
fn parse_width(text: &str) -> u8 {
    for line in text.lines() {
        // Searched for INSIDE the line, not at the start of it. `iw` prints
        // the width as part of the channel line --
        //   channel 36 (5180 MHz), width: 80 MHz, center1: 5210 MHz
        // -- so a prefix match finds it on no real interface at all, and the
        // default below would silently become the only answer this function
        // ever gave.
        let Some(rest) = line.split("width:").nth(1) else {
            continue;
        };
        let Some(mhz) = rest.split_whitespace().next() else {
            continue;
        };
        return match mhz {
            "20" => 0,
            "40" => 1,
            "160" => 3,
            _ => DEFAULT_BW,
        };
    }
    DEFAULT_BW
}

/// Run `iw` and return its stdout, or an empty string.
///
/// Every failure is an empty string, which reads downstream as "this interface
/// had no peers" — the honest answer, and one that costs nothing because a
/// peer that is really there reappears on the next pass.
fn iw(args: &[&str]) -> String {
    iw_ok(args).unwrap_or_default()
}

/// Run `iw` and distinguish a FAILURE from an empty answer.
///
/// `None` is "the command did not run, or ran and failed"; `Some("")` is "it
/// ran and had nothing to say". `iw dev <iface> station dump` collapses the
/// two on stdout -- an interface with no stations and an interface that does
/// not exist both print nothing there -- and the exit status is the only place
/// they differ. Everything downstream that treats an empty dump as "no peers"
/// needs that distinction, because the two call for opposite actions: stop the
/// captures, or change nothing at all.
fn iw_ok(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("iw").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Every peer worth capturing, across the CFR-capable radios.
///
/// Blocking: forks `iw` twice per interface. Called from `spawn_blocking`.
fn discover_peers(phys: &[String], previous: &[Want]) -> Option<Vec<Want>> {
    discover_peers_from(&iw(&["dev"]), phys, previous, |iface| {
        (
            iw_ok(&["dev", iface, "station", "dump"]),
            iw_ok(&["dev", iface, "info"]).map(|t| parse_width(&t)),
        )
    })
}

/// The half of [`discover_peers`] that decides what to capture, given the
/// output of `iw dev` and a way to probe one interface.
///
/// Split out because the decisions this function makes -- failure versus an
/// honest empty answer, at two levels -- are the ones with a bug in them if
/// there is one, and they cannot be reached through the forking half from a
/// test. `probe` returns the interface's station dump and its capture
/// bandwidth, each `None` if that command failed.
fn discover_peers_from<F>(
    dev: &str,
    phys: &[String],
    previous: &[Want],
    mut probe: F,
) -> Option<Vec<Want>>
where
    F: FnMut(&str) -> (Option<String>, Option<u8>),
{
    let ifaces = parse_dev_phys(dev);
    // `None` when `iw dev` listed no interfaces AT ALL, which is a failed fork
    // or a radio mid-reload, not a device with nobody associated. The
    // distinction is the whole point of the Option: an empty Vec means "these
    // radios exist and have no peers" and correctly stops the captures, while
    // a failure must change nothing. Clearing the captures on a transient fork
    // failure would stop every capture, and every link would then lose its
    // window and its learnt baseline and spend a minute back in Learning --
    // for a fork that failed under momentary memory pressure.
    if ifaces.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    for (phy, iface, kind) in ifaces {
        if !phys.contains(&phy) {
            continue;
        }
        let (dump, bw) = probe(&iface);
        // A failed `iw dev <iface> info` must not be read as a width.
        //
        // `parse_width` defaults to 80 MHz when it cannot find one, which is
        // right for output that simply lacks the line and wrong for output
        // that never arrived: on a 40 MHz radio a failed probe would report
        // 80, and `plan` would see a width change, stop every capture on the
        // interface and restart it at the wrong width -- then flip it back on
        // the next successful probe. Each flip costs every link its window and
        // its learnt baseline, on nothing but a fork that failed.
        //
        // So a failed probe keeps the width the interface was last known to
        // have; only a genuinely unknown interface falls back to the default.
        let bw = bw
            .or_else(|| previous.iter().find(|w| w.iface == iface).map(|w| w.bw))
            .unwrap_or(DEFAULT_BW);
        if bw != DEFAULT_BW {
            debug!("csi: {iface} is at {} MHz", bw_mhz(bw));
        }
        let Some(dump) = dump else {
            // The dump command FAILED on an interface `iw dev` still lists.
            // That is a transient -- a fork that lost to memory pressure, or
            // `iw` racing netifd reloading the interface -- not a radio whose
            // peers all left at once. Treating it as "no peers" stops every
            // capture on this interface, and each link then loses its window
            // and its learnt baseline and spends a minute back in Learning.
            //
            // So this interface's previous wants are carried forward
            // unchanged. Note that this is the FAILED case only: a dump that
            // RAN and returned nothing is an honest answer and falls through
            // below, where it contributes no wants and the captures for peers
            // that genuinely left are stopped. Carrying those forward too
            // would mean a station that walked out of the building was sounded
            // for as long as the device stayed up.
            debug!("csi: station dump failed on {iface}, keeping its previous peers");
            out.extend(previous.iter().filter(|w| w.iface == iface).cloned());
            continue;
        };
        // `motion::signals` is reused for its FILTER, not for the signal
        // levels it returns: a mesh peer counts only when its plink is ESTAB,
        // because a peer can sit in OPN_SNT with a plausible signal and no link
        // at all, and a capture aimed at one produces nothing while holding a
        // slot under the cap. Re-deriving that rule here would be two spellings
        // of one thing, drifting apart.
        for (mac, _dbm) in motion::signals(&dump, kind) {
            let Some(bytes) = mac_bytes(&mac) else {
                continue;
            };
            out.push(Want {
                phy: phy.clone(),
                iface: iface.clone(),
                mac: bytes,
                bw,
                kind,
            });
        }
    }
    Some(out)
}

// ── The reader ───────────────────────────────────────────────────────────────

/// Drain one radio's relay until told to stop.
///
/// Never returns on its own. The loop is: read to EOF, frame what arrived,
/// hand it over, sleep, read again. See the module docs for why EOF is not an
/// end.
fn read_relay(phy: String, stop: Arc<AtomicBool>) {
    let path = Path::new(DEBUGFS).join(&phy).join("ath11k/cfr_capture0");
    let mut errors = 0u32;
    let mut resyncs = 0u32;
    // Sized for one record so the common case is a single read per record and
    // the carry is usually empty.
    let mut chunk = vec![0u8; 16 * 1024];
    // A relay file that is missing stays missing -- the driver was built
    // without CFR, or the radio went away. Retrying is right, because a radio
    // can come back; saying so every two seconds is not, because the first
    // line is the only one that carries information and a router's whole log
    // is a ring buffer in RAM. Both the retry and the complaint back off, the
    // complaint to at most one line per ten minutes.
    let mut backoff = Backoff::new();
    // The last stamp handed to each peer, so this radio's clock only ever runs
    // forwards. See `stamp`.
    let mut seen: HashMap<[u8; 6], f64> = HashMap::new();

    while !stop.load(Ordering::Relaxed) {
        let mut file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(e) => {
                if backoff.should_log() {
                    warn!(
                        "csi: cannot open {} ({e}); retrying (attempt {})",
                        path.display(),
                        backoff.attempts
                    );
                }
                let wait = backoff.next_wait();
                sleep_until(&stop, wait);
                continue;
            }
        };
        backoff.reset();
        debug!("csi: reading {}", path.display());
        let mut carry: Vec<u8> = Vec::new();

        loop {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let n = match file.read(&mut chunk) {
                Ok(n) => n,
                Err(e) => {
                    // A read error means the file is gone or the driver
                    // unloaded. Reopen rather than exit: the radio may come
                    // back, and a reader that exits takes the feature with it
                    // for the life of the process.
                    warn!("csi: read error on {}: {e}", path.display());
                    break;
                }
            };
            if n == 0 {
                // EOF on a live relay. Not an end; a pause.
                sleep_until(&stop, RELAY_POLL);
                continue;
            }

            carry.extend_from_slice(&chunk[..n]);
            let parsed = parse(&carry);
            errors = errors.saturating_add(parsed.errors);
            resyncs = resyncs.saturating_add(parsed.resyncs);
            // A status other than 1 is a sounding the firmware abandoned. Its
            // payload is whatever was in the buffer, and feeding that to a
            // variance would read as a channel that changed.
            let good: Vec<Record> = parsed.records.into_iter().filter(Record::is_ok).collect();
            let samples = stamp(&good, motion::now_secs(), &mut seen);
            carry.drain(..parsed.consumed.min(carry.len()));
            if carry.len() > MAX_CARRY {
                // Bytes that never frame. See the constant.
                warn!(
                    "csi: {phy} produced {} bytes that do not frame; discarding",
                    carry.len()
                );
                carry.clear();
                resyncs = resyncs.saturating_add(1);
            }
            if !samples.is_empty() || parsed.errors > 0 || parsed.resyncs > 0 {
                deliver(&phy, samples, errors, resyncs);
            }
        }

        sleep_until(&stop, REOPEN_BACKOFF);
    }
}

/// Place a read's records in time, using the driver's own clock for the
/// spacing and the process clock for the epoch.
///
/// One `read` of the relay routinely returns several records that the radio
/// captured a capture period apart: at the default 100 ms a drain can carry
/// three spanning 300 ms. Stamping them all with the read time tells the
/// detector that three soundings happened at the same instant, which distorts
/// the dwell and gap arithmetic and turns a steady ten records a second into
/// bursts of three.
///
/// `meta.timestamp` is a microsecond counter -- measured at ~103000 between
/// records on a 100 ms capture -- so it supplies the intervals. The LAST record
/// is anchored at `now`, because it is the one that genuinely just arrived,
/// and the others are placed behind it by their own deltas.
///
/// `wrapping_sub` because the counter is a `u32` and wraps every ~72 minutes.
/// A wrap, or a driver that does not fill the field, yields an absurd offset;
/// anything beyond [`MAX_STAMP_BACKDATE`] is not trusted and that record falls
/// back to `now`. Being wrong by a few hundred milliseconds once an hour is a
/// far smaller error than back-dating a record by an hour, which would make it
/// look older than the stale threshold and resync the link.
fn stamp(records: &[Record], now: f64, seen: &mut HashMap<[u8; 6], f64>) -> Vec<Sample> {
    let last = records.last().map(|r| r.timestamp);
    let out: Vec<Sample> = records
        .iter()
        .map(|r| {
            // `wrapping_sub` on two `u32`s widened to `f64` cannot be
            // negative, so only the upper bound is worth testing.
            let at = last
                .map(|l| f64::from(l.wrapping_sub(r.timestamp)) / 1e6)
                .filter(|back| *back <= MAX_STAMP_BACKDATE)
                .map_or(now, |back| now - back);

            // Clamped so this peer's stamps never go backwards.
            //
            // The anchor moves forward with each read while the back-dating is
            // measured from within a read, so the two can cross: a read that
            // returns 50 ms after the last one (`RELAY_POLL`) can carry a
            // record back-dated up to 300 ms, landing it BEFORE a record
            // already delivered. The detector would then see time run
            // backwards -- a negative interval in the dwell arithmetic, and a
            // `RecordsPerSec` window that discards records it should count.
            //
            // The clamp costs at worst a few records bunched at one instant,
            // which is what the unclamped code did to all of them anyway.
            let at = match seen.get(&r.peer) {
                Some(&prev) if at < prev => prev,
                _ => at,
            };
            seen.insert(r.peer, at);
            let mag = magnitudes(r);
            Sample {
                peer: r.peer,
                bw: r.capture_bw,
                mean_mag: mean_mag(&mag),
                mean_phase: mean_phase(r),
                mag,
                at,
            }
        })
        .collect();

    // A peer that stopped reporting must not keep a slot for the life of the
    // process; its stamp is meaningless once the link has aged out anyway.
    seen.retain(|_, t| now - *t <= STALE_SECS);
    out
}

/// Furthest back a record may be placed from the time its read returned.
///
/// One read cannot hold more than the relay buffers, and a backlog that large
/// is seconds, not minutes. Anything beyond this is a wrapped or unfilled
/// hardware counter rather than a genuinely old record.
const MAX_STAMP_BACKDATE: f64 = 5.0;

/// Sleep, waking early if asked to stop.
///
/// Polled rather than parked on a condition variable because the only thing
/// that ever signals is shutdown, and a reader that takes an extra 50 ms to
/// notice costs nothing. Parking would mean a second synchronisation primitive
/// per radio for that.
fn sleep_until(stop: &AtomicBool, d: Duration) {
    let deadline = Instant::now() + d;
    while Instant::now() < deadline {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(RELAY_POLL.min(d));
    }
}

/// One radio's reader thread, and what is needed to notice it died.
///
/// A detached thread is a thread nobody can ask about. This keeps the handle
/// so the tick can call `is_finished`, which is the only way to tell a reader
/// that panicked from one that is quietly waiting on an idle radio -- both
/// look identical in the record stream, which is to say empty.
struct Reader {
    phy: String,
    handle: Option<std::thread::JoinHandle<()>>,
    /// When a dead reader may next be restarted.
    retry_at: Instant,
    /// Backoff between restarts, doubled on each failure.
    backoff: Duration,
    deaths: u32,
    /// How a reader thread is created.
    ///
    /// A field so a test can inject a spawn failure. `thread::spawn` fails
    /// when the process is out of memory or thread slots -- `EAGAIN` -- which
    /// is precisely the condition this backoff exists for and precisely the
    /// one a test cannot produce on demand. A plain `fn` pointer rather than a
    /// boxed closure: there is one alternative implementation, in tests.
    spawn_fn: SpawnFn,
}

/// Creates one radio's reader thread. See [`Reader::spawn_fn`].
type SpawnFn = fn(String, Arc<AtomicBool>) -> std::io::Result<std::thread::JoinHandle<()>>;

/// The real one.
fn spawn_reader(phy: String, stop: Arc<AtomicBool>) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name(format!("csi-{phy}"))
        .spawn(move || read_relay(phy, stop))
}

/// How soon a dead reader is first restarted, and the ceiling the backoff
/// climbs to. A radio whose reader panics on the first record of every read
/// would otherwise be restarted as fast as the tick runs.
const READER_RETRY: Duration = Duration::from_secs(2);
const READER_RETRY_MAX: Duration = Duration::from_secs(60);

impl Reader {
    fn new(phy: String) -> Self {
        Self {
            phy,
            handle: None,
            retry_at: Instant::now(),
            backoff: READER_RETRY,
            deaths: 0,
            spawn_fn: spawn_reader,
        }
    }

    /// Spawn the thread, if it is not running.
    ///
    /// A spawn FAILURE takes the same path as a reader that died, and that is
    /// the point: `thread::spawn` fails under `EAGAIN`, which is a sustained
    /// condition, not a momentary one. Treated as a mere log line it produced
    /// a warning every tick -- four a second, forever, in a log that is a ring
    /// buffer in RAM -- and retried just as often, on a device already out of
    /// the resource being asked for.
    fn start(&mut self, stop: &Arc<AtomicBool>) {
        match (self.spawn_fn)(self.phy.clone(), Arc::clone(stop)) {
            Ok(h) => self.handle = Some(h),
            Err(e) => self.defer(&format!("could not be started ({e})")),
        }
    }

    /// Count a failure, say so once, and put the next attempt off.
    fn defer(&mut self, what: &str) {
        self.deaths += 1;
        warn!(
            "csi: reader for {} {} (failure {}); retrying in {:?}",
            self.phy, what, self.deaths, self.backoff
        );
        self.retry_at = Instant::now() + self.backoff;
        self.backoff = (self.backoff * 2).min(READER_RETRY_MAX);
    }

    /// Restart the thread if it has ended, respecting the backoff.
    ///
    /// The reader only returns when asked to stop, so a finished handle during
    /// a run means it panicked or could not be spawned. Logged once per death
    /// rather than once per tick: a reader that panics immediately would
    /// otherwise be four lines a second in a log that lives in RAM.
    fn restart_if_dead(&mut self, stop: &Arc<AtomicBool>) {
        let dead = self.handle.as_ref().is_none_or(|h| h.is_finished());
        if !dead || stop.load(Ordering::Relaxed) {
            return;
        }
        if let Some(h) = self.handle.take() {
            // Joining a finished thread does not block, and it is what turns a
            // panic into a line in the log rather than a radio that stopped.
            let panicked = h.join().is_err();
            self.defer(if panicked { "panicked" } else { "exited" });
            return;
        }
        if Instant::now() >= self.retry_at {
            self.start(stop);
        }
    }
}

// ── Lifecycle ────────────────────────────────────────────────────────────────

/// Is a CSI reader already running in this process?
///
/// One, ever, for the same reason `motion::RUNNING` exists: `usp::agent::run`
/// is re-entered after every connection failure, and a second reader would
/// mean two threads draining one relay, each seeing half the records, and two
/// discovery passes fighting over the same `cfr_capture` files.
static RUNNING: AtomicBool = AtomicBool::new(false);

/// Releases [`RUNNING`] however the task ends, panic included.
struct RunningGuard;

impl Drop for RunningGuard {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::Release);
    }
}

/// Everything the radios must be put back to, whatever happens to the task.
///
/// A `Drop` implementation rather than cleanup at the end of `run` because the
/// thing being cleaned up is STATE ON THE DEVICE, not memory. An agent that
/// exits without writing these leaves every peer being sounded ten times a
/// second and 5 MB of relay allocated per radio, for as long as the device
/// stays up — and the next agent to start finds captures running that it did
/// not start and has no record of.
struct Captures {
    phys: Vec<String>,
    active: HashMap<CapKey, u8>,
    period_ms: u64,
    stop: Arc<AtomicBool>,
}

impl Drop for Captures {
    fn drop(&mut self) {
        // The readers are told to stop FIRST, and synchronously, so that no
        // thread is mid-read on a relay whose radio is about to have CFR
        // switched off under it. This one is a flag store, not I/O.
        self.stop.store(true, Ordering::Release);

        let active: Vec<(CapKey, u8)> = self.active.drain().collect();
        let phys = std::mem::take(&mut self.phys);
        let period_ms = self.period_ms;

        // The WRITES go to a thread of their own. `Drop` runs wherever the
        // task was dropped -- a tokio worker, or the runtime's own shutdown --
        // and there are up to nine blocking debugfs writes here. A `Drop` that
        // blocks a worker stalls every other task on it, and during runtime
        // shutdown it stalls the shutdown itself; neither is a place to
        // discover that a wedged driver takes seconds to answer a write.
        //
        // Detached deliberately. Nothing here can be waited for from a `Drop`
        // without reintroducing exactly the block being avoided, and the
        // ordering that matters -- stops before the radio is switched off --
        // is preserved INSIDE the thread.
        let spawned = std::thread::Builder::new()
            .name("csi-teardown".into())
            .spawn(move || {
                for ((phy, iface, mac), bw) in active {
                    set_capture(&phy, &iface, &mac, bw, period_ms, false);
                }
                for phy in &phys {
                    set_enable_cfr(phy, false);
                }
                info!("csi: captures stopped and CFR disabled");
            });
        if let Err(e) = spawned {
            // A device too short of memory to start a thread is one that will
            // keep sounding its peers until it reboots. Say so: the symptom
            // otherwise is a radio that is busy for no visible reason.
            warn!(
                "csi: cannot start the teardown thread ({e}); captures may still \
                 be running -- restart the agent or reboot to clear them"
            );
        }
    }
}

/// Start the CSI reader, if the operator has consented to it.
///
/// Called from the USP agent rather than from `main` for the same reason
/// `motion::spawn` is: the only way to send a Notify is the `StatusSender` the
/// agent builds with its MTP channel, and `main` has no handle on it.
///
/// Returns the stop handle, which the caller must KEEP: dropping it is what
/// retires the task, and retiring the task is what switches the radios back.
/// Returns `None` when sensing is off or a reader is already running, in both
/// cases having said so.
#[must_use = "dropping the stop handle immediately retires the reader"]
pub fn spawn(
    cfg: Arc<ClientConfig>,
    tx: StatusSender,
    agent_id: EndpointId,
) -> Option<tokio::sync::watch::Sender<()>> {
    if !cfg.csi_enabled {
        // Said once, at info: silence here is indistinguishable from a reader
        // that is running and has never seen anybody.
        info!(
            "csi: channel sensing is off (the shape of a home's multipath is personal \
             data and must be consented); enable with: uci set optimacs.agent.csi_enabled='1' \
             && uci commit optimacs && /etc/init.d/ac-client restart"
        );
        return None;
    }
    if RUNNING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        warn!("csi: a reader is already running, not starting another");
        return None;
    }

    let (stop_tx, stop_rx) = tokio::sync::watch::channel(());
    tokio::spawn(async move {
        let _guard = RunningGuard;
        run(cfg, tx, agent_id, stop_rx).await;
    });
    Some(stop_tx)
}

async fn run(
    cfg: Arc<ClientConfig>,
    tx: StatusSender,
    agent_id: EndpointId,
    stop: tokio::sync::watch::Receiver<()>,
) {
    let phys = tokio::task::spawn_blocking(discover_cfr_phys)
        .await
        .unwrap_or_default();
    if phys.is_empty() {
        info!("csi: CFR not available on this radio; csi disabled");
        return;
    }

    let period_ms = cfg.csi_period_ms;
    let tune = Tuning::from_config(&cfg);
    // Records per window, from the two settings that define it. Floored at two
    // by `CsiWindow::new`, which is the only place the floor belongs.
    let win_cap = (cfg.csi_window_secs * 1000 / period_ms.max(1)) as usize;
    let reader_stop = Arc::new(AtomicBool::new(false));

    // The inbox is a `static` and outlives any one run of the agent, which is
    // re-entered after every connection failure -- 30 s later, by `main`.
    // Whatever the last run's readers left in it was captured before that gap
    // and would be replayed now as if it had just arrived: up to 2048 stale
    // records per radio, handed to the detectors with current timestamps, as a
    // burst. Dropped rather than kept, because a sounding from before a
    // half-minute outage describes a room nobody was watching.
    clear_inbox();

    info!(
        "csi: sounding every {period_ms} ms on {}; {win_cap}-record windows over {} s",
        phys.join(", "),
        cfg.csi_window_secs
    );
    info!(
        "csi: vitals over {} s at confidence >= {:.2}; fall at +{:.0} dB then {} s still. \
         Breathing and heart rates are EXPERIMENTAL CANDIDATES, not measurements: they \
         need a still subject inside the link's own path, and nothing should act on one",
        cfg.csi_vitals_window_secs,
        cfg.csi_vitals_min_confidence,
        cfg.csi_fall_rise_db,
        cfg.csi_fall_still_secs
    );

    // The guard is created BEFORE the radios are touched, so that a failure
    // anywhere below still unwinds through it.
    let mut caps = Captures {
        phys: phys.clone(),
        active: HashMap::new(),
        period_ms,
        stop: Arc::clone(&reader_stop),
    };

    // debugfs writes are blocking file I/O. Four of them is microseconds in
    // practice, but a wedged driver can make a debugfs write take as long as
    // it likes, and on a runtime worker that stalls every other task the agent
    // has -- including the MTP connection.
    {
        let phys2 = phys.clone();
        let _ = tokio::task::spawn_blocking(move || {
            for phy in &phys2 {
                set_enable_cfr(phy, true);
            }
        })
        .await;
    }

    // Readers are KEPT, not detached. A reader thread is the only source of
    // records for its radio, so one that dies -- a panic in the framing, an
    // allocation failure -- takes that radio's sensing with it, silently and
    // for the life of the process. The tick below notices and restarts it.
    let mut readers: Vec<Reader> = phys.iter().map(|p| Reader::new(p.clone())).collect();
    for r in &mut readers {
        r.start(&reader_stop);
    }

    let mut peers: PeerMap = HashMap::new();
    let mut windows: HashMap<(String, [u8; 6]), WinState> = HashMap::new();
    // What the last successful discovery wanted, so an interface whose station
    // dump fails can have its peers carried forward. See `discover_peers_from`.
    let mut last_wants: Vec<Want> = Vec::new();
    let mut next_discovery = Instant::now();

    loop {
        tokio::time::sleep(DRAIN).await;

        // The stop handle belongs to `agent::run` and is dropped when it
        // returns. Checked rather than awaited so a drain already in flight
        // finishes; `caps` then unwinds and puts the radios back.
        if stop.has_changed().is_err() {
            info!("csi: agent stopped, reader retiring");
            return;
        }

        if Instant::now() >= next_discovery {
            let phys2 = phys.clone();
            let active = caps.active.clone();
            let prev = last_wants.clone();
            let found = tokio::task::spawn_blocking(move || {
                let desired = discover_peers(&phys2, &prev)?;
                let peers = name_peers(&desired);
                let (starts, stops) = plan(desired.clone(), &active);
                Some((
                    reconcile(starts, stops, active, period_ms),
                    peers,
                    desired,
                ))
            })
            .await;
            match found {
                Ok(Some((active, map, desired))) => {
                    caps.active = active;
                    peers = map;
                    last_wants = desired;
                    next_discovery = Instant::now() + REDISCOVER;
                }
                // Discovery found no radios at all: a failed fork, or a radio
                // mid-reload. The captures and the peer names are left exactly
                // as they were -- see `discover_peers` -- and retried sooner,
                // so a device whose radios are still coming up picks them up
                // quickly without forking `iw` on every drain.
                _ => {
                    debug!("csi: discovery found no interfaces, keeping the captures it had");
                    next_discovery = Instant::now() + REDISCOVER_RETRY;
                }
            }
        }

        for r in &mut readers {
            r.restart_if_dead(&reader_stop);
        }

        let now = motion::now_secs();
        // The windows and the square roots are worked out first, holding no
        // lock; see `features`.
        let (feats, streams) = features(drain_inbox(), &mut windows, &tune, win_cap, now);

        // `apply` runs on EVERY tick, empty drain included. It is the only
        // thing that retracts a link which vanished mid-Motion and then drops
        // it -- and a link vanishes precisely when its records stop, so a
        // `continue` on an empty drain would skip the eviction exactly when it
        // is due. A peer that left the house while in Motion would read as
        // somebody moving in it until the agent restarted.
        let (changes, idle) = {
            let mut guard = links();
            let table = guard.get_or_insert_with(Vec::new);
            let changes = apply(table, feats, &streams, &peers, &tune, now);
            // Read out under the same lock the detectors were updated under,
            // and used outside it. At most eight links, one small clone each.
            let idle: Vec<(WinKey, Option<f64>)> = table
                .iter()
                .map(|l| ((l.phy.clone(), l.key), l.idle_since))
                .collect();
            (changes, idle)
        };

        // The transforms. Outside every lock, which is the whole point of the
        // two-acquisition shape: a 512-point FFT per series per link is the
        // one piece of work here that a data-model GET must never wait behind.
        let estimates = estimate_vitals(&mut windows, &idle, now);
        if !estimates.is_empty() {
            let mut guard = links();
            store_vitals(guard.get_or_insert_with(Vec::new), &estimates);
        }

        for c in changes {
            info!("csi: {} {} {} -> {}", c.iface, c.peer, c.param, c.value);
            notify(&tx, &agent_id, &cfg.controller_id, &c);
        }
    }
}

/// Apply a plan to the radios and report what is running afterwards.
///
/// Blocking: one debugfs write per change. Returns the new active set and the
/// peer map `apply` needs to name a link's interface.
///
/// A start that FAILS is left out of the active set rather than recorded
/// optimistically. The peer is then retried on the next pass, which is right:
/// the usual cause is a station that left between the dump and the write, and
/// recording it would mean the agent believes a capture is running, never
/// retries it, and later writes a stop for something it never started.
fn reconcile(
    starts: Vec<Want>,
    stops: Vec<CapKey>,
    mut active: HashMap<CapKey, u8>,
    period_ms: u64,
) -> HashMap<CapKey, u8> {
    for (phy, iface, mac) in stops {
        let bw = active.remove(&(phy.clone(), iface.clone(), mac)).unwrap_or(2);
        set_capture(&phy, &iface, &mac, bw, period_ms, false);
        debug!("csi: stopped capture for {} on {iface}", mac_upper(&mac));
    }
    for w in starts {
        if set_capture(&w.phy, &w.iface, &w.mac, w.bw, period_ms, true) {
            active.insert((w.phy.clone(), w.iface.clone(), w.mac), w.bw);
            info!(
                "csi: capturing {} on {} at {} MHz every {period_ms} ms",
                mac_upper(&w.mac),
                w.iface,
                bw_mhz(w.bw)
            );
        }
    }
    active
}

/// Name every peer discovery saw, so `apply` can label a link when its first
/// record arrives.
///
/// Built from what DISCOVERY found and not from what was just started. After
/// the first pass almost nothing is started -- the captures are already
/// running -- so a map built from the starts would be empty, and a link
/// created any time after the first few seconds would fall back to the radio's
/// name and to `Client`. A mesh peer would then be reported as a client, which
/// is the one distinction the `Kind` parameter exists to make.
///
/// Peers that did not make the capture cap are named too. Harmless, and one
/// fewer conditional: the map is only ever consulted for a peer that produced
/// a record, and a peer that is not being captured produces none.
fn name_peers(desired: &[Want]) -> PeerMap {
    desired
        .iter()
        .map(|w| ((w.phy.clone(), w.mac), (w.iface.clone(), w.kind)))
        .collect()
}

/// Send one ValueChange Notify for a link's new state.
///
/// The same encode / record / send path the heartbeat uses, down to the
/// `status` subscription id, for the reason `motion::notify` gives: the
/// controller already routes that subscription, and a second one would produce
/// Notifies it has no subscription for.
///
/// Offered, never waited for. A stalled TCP connection fills the shared
/// channel, and blocking here would stop the drain — the records the reader
/// threads keep producing would queue and eventually be dropped, so a stalled
/// uplink would degrade the sensing rather than just its reporting.
fn notify(tx: &StatusSender, agent_id: &EndpointId, controller_id: &str, c: &Change) {
    let path = format!("Device.X_OptimACS_Sensing.Csi.{}.{}", c.instance, c.param);
    let state = c.value;
    let msg = build_value_change_notify("status", &path, state);
    let msg_bytes = match encode_msg(&msg) {
        Ok(b) => b,
        Err(e) => {
            warn!("csi: cannot encode notify: {e}");
            return;
        }
    };
    let rec = record::no_session_record(agent_id.as_str(), controller_id, msg_bytes, "1.3");
    match record::encode_record(&rec) {
        Ok(bytes) => match tx.try_send(bytes) {
            Ok(()) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => warn!(
                "csi: status channel full, dropped {path} = {state}; \
                 the current state is still readable with a GET"
            ),
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                warn!("csi: status channel closed, dropped {path} = {state}")
            }
        },
        Err(e) => warn!("csi: cannot encode record: {e}"),
    }
}

// ── The data model ───────────────────────────────────────────────────────────

/// Report the CSI sub-tree.
///
/// Routed ahead of `dm::sensing` and never through it: that module deletes its
/// spool as it reports, so answering a `.Csi` GET there would silently consume
/// a batch of firewall observations.
pub fn get(cfg: &ClientConfig, path: &str) -> HashMap<String, String> {
    if !path.starts_with("Device.X_OptimACS_Sensing") {
        return HashMap::new();
    }
    let guard = links();
    let table = guard.as_deref().unwrap_or(&[]);
    render(table, cfg.csi_enabled, motion::now_secs())
}

/// Render the table as data-model parameters.
///
/// Split from [`get`] so the shape the controller sees is testable without a
/// radio, a lock, or a process. `now` is a parameter for the same reason:
/// `RecordsPerSec` is a rate over the last second and a rate needs a clock,
/// and a test cannot assert on one it cannot set.
fn render(table: &[CsiLink], enabled: bool, now: f64) -> HashMap<String, String> {
    let mut m = HashMap::new();
    // A sibling of the table, not a member of it: `Csi.{i}.` is a
    // multi-instance object and `Csi.Enable` would be a scalar wearing an
    // instance prefix.
    //
    // Read-only, for exactly the reason `MotionEnable` is: this switch is
    // consent, and consent a controller can assert remotely over a session it
    // authenticated itself is not consent. It lives in UCI, where switching on
    // channel sensing in somebody's home takes a person with an account on the
    // device.
    m.insert(
        "Device.X_OptimACS_Sensing.CsiEnable".into(),
        if enabled { "1" } else { "0" }.into(),
    );
    m.insert(
        "Device.X_OptimACS_Sensing.CsiNumberOfEntries".into(),
        table.len().to_string(),
    );

    for link in table {
        let base = format!("Device.X_OptimACS_Sensing.Csi.{}", link.instance);
        m.insert(format!("{base}.Interface"), link.iface.clone());
        m.insert(format!("{base}.Peer"), link.peer.clone());
        m.insert(format!("{base}.Kind"), link.kind.as_str().into());
        m.insert(
            format!("{base}.State"),
            link.detector.state().as_str().into(),
        );
        // Empty until a full window exists, rather than 0: 0 dB is a real
        // energy a real link could have, and a controller cannot tell an
        // invented number from a measured one.
        // Filtered on `is_finite`, not merely on `Some`. `format!("{:.2}")`
        // renders a NaN as the four characters "NaN" and an infinity as "inf",
        // and either reaches the controller as a parameter value that no
        // comparison it makes is true for -- worse than a wrong number,
        // because it looks like a working sensor. An unmeasurable energy is
        // reported the same way an unmeasured one is: empty.
        m.insert(
            format!("{base}.MotionEnergyDb"),
            link.energy_db
                .filter(|d| d.is_finite())
                .map_or(String::new(), |d| format!("{d:.2}")),
        );
        m.insert(
            format!("{base}.RecordsPerSec"),
            link.arrivals.iter().filter(|&&t| now - t <= 1.0).count().to_string(),
        );
        m.insert(format!("{base}.Records"), link.records.to_string());
        m.insert(format!("{base}.ParseErrors"), link.errors.to_string());
        m.insert(
            format!("{base}.LastMotionAt"),
            link.last_motion_at.clone().unwrap_or_default(),
        );
        m.insert(format!("{base}.Bandwidth"), bw_mhz(link.bw).to_string());

        // Vitals. Empty is the normal answer and is not an error: it means the
        // link has not been still for a whole window, or that what was in the
        // window did not clear the confidence floor. Filtered on `is_finite`
        // for the reason `MotionEnergyDb` is -- "NaN" and "inf" reach a
        // controller as values no comparison it makes is true for.
        // Blanked once the link has stopped producing records for longer than
        // the window the estimate was made over. The link itself survives for
        // `STALE_SECS`, which is twice as long: without this a peer that went
        // away mid-estimate would go on reporting a breathing rate for a room
        // it has no measurements from, and the only thing that would say so is
        // `RecordsPerSec` reading 0 next to it.
        let v = if now - link.last_seen > link.vitals_span {
            Vitals::default()
        } else {
            link.vitals
        };
        let bpm = |x: Option<f64>| {
            x.filter(|d| d.is_finite())
                .map_or(String::new(), |d| format!("{d:.1}"))
        };
        m.insert(format!("{base}.BreathingBpm"), bpm(v.breathing_bpm));
        m.insert(format!("{base}.HeartBpm"), bpm(v.heart_bpm));
        m.insert(
            format!("{base}.VitalsConfidence"),
            v.confidence
                .filter(|d| d.is_finite())
                .map_or(String::new(), |d| format!("{d:.2}")),
        );
        // A boolean, not an empty string: unlike a rate, "no fall candidate" is
        // something the agent knows rather than something it could not work out.
        m.insert(
            format!("{base}.FallCandidate"),
            if link.fall.candidate() { "1" } else { "0" }.into(),
        );
        m.insert(
            format!("{base}.LastFallAt"),
            link.last_fall_at.clone().unwrap_or_default(),
        );
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three real records off a QCN9074: peer 02:0C:43:26:60:11, 80 MHz, four
    /// chains, every one a successful capture.
    ///
    /// Embedded rather than read at runtime so the test cannot pass because a
    /// path resolved to something else, and so the bytes are in the test
    /// binary only — `cfg(test)` keeps all 12 KB of it out of the 5.4 MB that
    /// ships.
    const FIXTURE: &[u8] = include_bytes!("../../../tests/fixtures/cfr-qcn9074-80mhz-3rec.bin");

    /// The record size this hardware actually produces: 108 header + 48 DMA
    /// header + 4096 of I/Q + 4 end magic.
    const REC_LEN: usize = 4256;

    // ── The wire format ──────────────────────────────────────────────────────

    /// The fixture is the specification. Every number here was read off a live
    /// radio, and if the parser stops agreeing with them it is the parser that
    /// is wrong.
    #[test]
    fn the_fixture_parses_as_three_eighty_megahertz_records() {
        assert_eq!(FIXTURE.len(), 3 * REC_LEN, "the fixture is three records");

        let p = parse(FIXTURE);
        assert_eq!(p.records.len(), 3);
        assert_eq!(p.consumed, FIXTURE.len(), "no bytes left over");
        assert_eq!(p.errors, 0);
        assert_eq!(p.resyncs, 0);

        for r in &p.records {
            assert_eq!(r.peer, [0x02, 0x0c, 0x43, 0x26, 0x60, 0x11]);
            assert_eq!(r.peer_str(), "02:0C:43:26:60:11");
            assert_eq!(r.status, 1, "every fixture record is a good capture");
            assert!(r.is_ok());
            assert_eq!(r.capture_bw, 2, "80 MHz");
            assert_eq!(r.chan_bw, 2);
            assert_eq!(bw_mhz(r.capture_bw), 80);
            assert_eq!(r.num_rx_chain, 4);
            assert_eq!(r.prim20, 5180);
            assert_eq!(r.cf1, 5210);
            assert_eq!(r.cf2, 0);

            // The DMA header, which is what says where the I/Q starts and how
            // it is shaped.
            assert_eq!(r.dma.header_words, 12, "48-byte DMA header");
            assert_eq!(r.dma.header_len(), 48);
            assert_eq!(r.dma.total_bytes, 4096);
            assert_eq!(r.chains(), 4, "num_chains is stored one less");
            assert_eq!(r.tones(), 256, "4096 / (4 chains x 4 bytes)");
            assert!(r.dma.upload_done);

            // 4 chains x 256 tones x (I, Q).
            assert_eq!(r.iq.len(), 4 * 256 * 2);
        }

        // Distinct soundings, not the same record three times.
        let ts: Vec<u32> = p.records.iter().map(|r| r.timestamp).collect();
        assert_eq!(ts, vec![300_486_499, 300_493_184, 300_499_640]);
    }

    /// The per-chain RSSI is stored in an unsigned field while being negative.
    ///
    /// Read as `u32` it is 4294967217, which is not a signal level any
    /// comparison is true for — and it looks like a working parser, because
    /// every other field around it is right.
    #[test]
    fn a_negative_chain_rssi_survives_its_unsigned_field() {
        let p = parse(FIXTURE);
        let r = &p.records[0];
        assert_eq!(&r.chain_rssi[..4], &[-79, -85, -71, -73]);
        assert!(
            r.chain_rssi[..4].iter().all(|&v| (-100..0).contains(&v)),
            "a chain RSSI outside -100..0 dBm was not decoded as signed: {:?}",
            r.chain_rssi
        );
        // The unused chains are genuinely zero, not sign-extended rubbish.
        assert_eq!(&r.chain_rssi[4..], &[0, 0, 0, 0]);
    }

    /// Records straddle read boundaries as a matter of course: the relay hands
    /// over whatever is buffered, which is never a whole number of records.
    ///
    /// The parser must therefore return the records it completed and say how
    /// far it got, so the caller can carry the rest into the next read. A
    /// parser that consumed the partial record would lose one in every read.
    #[test]
    fn a_record_split_across_two_reads_is_carried_not_lost() {
        // A boundary inside the second record's payload.
        let cut = REC_LEN + 1000;
        let first = parse(&FIXTURE[..cut]);
        assert_eq!(first.records.len(), 1, "only the whole record is complete");
        assert_eq!(first.consumed, REC_LEN, "the partial record is carried");
        assert_eq!(first.errors, 0, "a straddle is not an error");

        // The caller carries the tail and appends the next read.
        let mut carry = FIXTURE[first.consumed..cut].to_vec();
        carry.extend_from_slice(&FIXTURE[cut..]);
        let second = parse(&carry);
        assert_eq!(second.records.len(), 2, "the split record completes");
        assert_eq!(second.consumed, carry.len());
        assert_eq!(second.errors, 0);
    }

    /// A relay that dropped a sub-buffer leaves a hole in the middle of a
    /// record. The parser must find the next record rather than give up on the
    /// stream — losing one record is the cost, losing the radio is not.
    #[test]
    fn a_corrupted_record_resyncs_onto_the_next_one() {
        let mut bytes = FIXTURE.to_vec();
        // Break the first record's end magic. Its framing is otherwise intact,
        // so this is precisely the "length or payload is damaged" case.
        let end = REC_LEN - MAGIC_LEN;
        bytes[end..end + MAGIC_LEN].copy_from_slice(&[0, 0, 0, 0]);

        let p = parse(&bytes);
        assert_eq!(p.errors, 1, "the damaged record is counted");
        assert_eq!(p.records.len(), 2, "the two good records still arrive");
        assert!(p.resyncs >= 1, "finding the next record is a resync");
        assert!(
            p.records.iter().all(|r| r.tones() == 256),
            "a resync must land on a record boundary, not mid-payload"
        );
    }

    /// Bytes in front of the first record — the tail of something the previous
    /// reader missed — must be skipped, not treated as a header.
    #[test]
    fn leading_rubbish_is_skipped() {
        let mut bytes = vec![0x5Au8; 777];
        bytes.extend_from_slice(FIXTURE);
        let p = parse(&bytes);
        assert_eq!(p.records.len(), 3);
        assert_eq!(p.resyncs, 1);
        assert_eq!(p.consumed, bytes.len());
    }

    /// A magic split across a read boundary must not be thrown away.
    ///
    /// Keeping only `len - 3` on a buffer with no magic in it is what makes
    /// this work; consuming the whole buffer would drop the first byte or two
    /// of a magic and desynchronise the stream permanently.
    #[test]
    fn a_magic_split_across_reads_survives() {
        let mut head = vec![0x11u8; 200];
        // The first two bytes of the start magic, little-endian.
        head.extend_from_slice(&START_MAGIC.to_le_bytes()[..2]);
        let p = parse(&head);
        assert!(p.records.is_empty());
        assert!(
            head.len() - p.consumed >= 2,
            "the partial magic was consumed and the stream would never resync"
        );

        let mut whole = head[p.consumed..].to_vec();
        whole.extend_from_slice(&START_MAGIC.to_le_bytes()[2..]);
        whole.extend_from_slice(&FIXTURE[MAGIC_LEN..]);
        assert_eq!(parse(&whole).records.len(), 3, "the split record completes");
    }

    /// A corrupt header can claim any payload length at all, and the parser's
    /// answer to "the record is not all here yet" is to WAIT. A length of 4 GB
    /// would therefore stall this radio forever behind a growing carry.
    #[test]
    fn an_impossible_length_is_an_error_not_a_wait() {
        let mut bytes = FIXTURE.to_vec();
        // total_bytes is at offset 8 of the DMA header.
        let off = HEADER_LEN + 8;
        bytes[off..off + 2].copy_from_slice(&u16::MAX.to_le_bytes());

        let p = parse(&bytes);
        assert_eq!(p.errors, 1, "an unusable length is an error");
        assert!(
            p.consumed > 0,
            "the parser waited for bytes that will never come"
        );
        assert_eq!(
            p.records.len(),
            2,
            "the two undamaged records must still be recovered"
        );
    }

    /// The parser is the only thing in the module that sees bytes the device
    /// does not control, so it must survive all of them. A panic here takes the
    /// reader thread for a whole radio down with it.
    #[test]
    fn no_input_makes_the_parser_panic() {
        // A tiny deterministic PRNG. Deterministic so a failure is a bug
        // report and not a story about a seed nobody wrote down.
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };

        for round in 0..200 {
            let len = (next() % 9000) as usize;
            let mut bytes: Vec<u8> = (0..len).map(|_| (next() & 0xFF) as u8).collect();
            // Splice a real record in at a random offset, so the fuzz explores
            // half-valid framing and not only noise.
            if round % 2 == 0 && !bytes.is_empty() {
                let at = (next() as usize) % bytes.len();
                // Up to the WHOLE fixture, not up to one record length. Capped
                // at `REC_LEN` the splice could never contain a complete
                // record, so every assertion below about a parsed record was
                // unreachable and the fuzz tested only the reject paths.
                let take = (next() as usize) % (FIXTURE.len() + 1);
                bytes.splice(at..at, FIXTURE[..take].iter().copied());
            }
            let p = parse(&bytes);
            assert!(p.consumed <= bytes.len(), "consumed past the end of the buffer");
            for r in &p.records {
                // Anything the parser claims is a record must be internally
                // consistent, or the features downstream are measuring noise.
                assert_eq!(r.iq.len(), r.dma.total_bytes as usize / 2);
                assert!(r.chains() >= 1 && r.chains() <= 8);
            }
        }

        // The fuzz above found at least one real record, or its record
        // assertions proved nothing. This is the guard on the guard.
        assert!(
            parse(&[&[0x77u8; 100][..], FIXTURE].concat()).records.len() == 3,
            "the splice can no longer produce a parseable record"
        );

        // Every truncation of the fixture: the exhaustive version of the
        // straddle, including every offset inside the header, inside the DMA
        // header, and one byte short of the end magic.
        for cut in 0..FIXTURE.len() {
            let p = parse(&FIXTURE[..cut]);
            assert!(
                p.consumed <= cut,
                "consumed {} of a {cut}-byte buffer",
                p.consumed
            );
            assert!(
                p.records.len() <= cut / REC_LEN,
                "a truncated buffer yielded more records than it can hold"
            );
        }

        // The degenerate inputs, which a random fuzz reaches rarely.
        for bytes in [
            vec![],
            vec![0u8; 1],
            vec![0xFFu8; HEADER_LEN + DMA_LEN - 1],
            START_MAGIC.to_le_bytes().to_vec(),
            [START_MAGIC.to_le_bytes().as_slice(), &[0u8; 4000]].concat(),
        ] {
            let p = parse(&bytes);
            assert!(p.consumed <= bytes.len());
        }
    }

    /// A parser that never advances is a reader thread spinning a core on a
    /// buffer it will never finish.
    #[test]
    fn every_buffer_either_yields_records_or_is_consumed() {
        let junk = vec![0x00u8; 10_000];
        let p = parse(&junk);
        assert!(p.records.is_empty());
        assert!(
            p.consumed >= junk.len() - (MAGIC_LEN - 1),
            "a buffer with no magic in it must be discarded, not carried forever"
        );
    }

    // ── The feature ──────────────────────────────────────────────────────────

    /// Magnitudes are one per (chain, tone), which is what every statistic
    /// below iterates over.
    #[test]
    fn magnitudes_are_one_per_chain_and_tone() {
        let p = parse(FIXTURE);
        let m = magnitudes(&p.records[0]);
        assert_eq!(m.len(), 4 * 256);
        assert!(m.iter().all(|v| v.is_finite() && *v >= 0.0));
    }

    /// The interleaving is chain-outer: four consecutive blocks of 256 tones.
    ///
    /// Asserted through the per-chain RSSI, which is an independent
    /// measurement of the same thing: chain 2 is the strongest at -71 dBm and
    /// chain 1 the weakest at -85, and the block means must rank the same way.
    /// Under a tone-outer reading the four block means come out nearly equal,
    /// because each block would then hold every chain.
    ///
    /// Nothing in the feature code depends on this — it averages per position
    /// — but getting it wrong would mean the module's documentation lies about
    /// what a position is, and a future change that DOES depend on the order
    /// would inherit the lie.
    #[test]
    fn the_payload_is_chain_outer() {
        let p = parse(FIXTURE);
        let r = &p.records[0];
        let m = magnitudes(r);
        let tones = r.tones();
        let means: Vec<f64> = (0..r.chains())
            .map(|c| {
                m[c * tones..(c + 1) * tones]
                    .iter()
                    .map(|&v| f64::from(v))
                    .sum::<f64>()
                    / tones as f64
            })
            .collect();

        let strongest = (0..4).max_by_key(|&i| r.chain_rssi[i]).unwrap();
        let weakest = (0..4).min_by_key(|&i| r.chain_rssi[i]).unwrap();
        assert_eq!(strongest, 2, "the fixture's strongest chain");
        assert_eq!(weakest, 1);
        assert!(
            means[strongest] > means[weakest] * 2.0,
            "block means {means:?} do not track the chain RSSI {:?}; \
             the payload is not chain-outer",
            &r.chain_rssi[..4]
        );
    }

    /// A record built from complex values, so a test can state the channel it
    /// wants instead of hunting for one in a capture.
    ///
    /// `f` gives (I, Q) for a (chain, tone); the DMA header is filled in so
    /// that [`Record::tones`] derives the shape back out of it, exactly as it
    /// does on the wire.
    fn synth(chains: usize, tones: usize, mut f: impl FnMut(usize, usize) -> (f64, f64)) -> Record {
        let mut iq = Vec::with_capacity(chains * tones * 2);
        for c in 0..chains {
            for k in 0..tones {
                let (i, q) = f(c, k);
                iq.push(i.round() as i16);
                iq.push(q.round() as i16);
            }
        }
        Record {
            peer: [0; 6],
            status: 1,
            capture_bw: 2,
            chan_bw: 2,
            phy_mode: 0,
            prim20: 0,
            cf1: 0,
            cf2: 0,
            num_rx_chain: chains as u8,
            timestamp: 0,
            chain_rssi: [0; 8],
            dma: DmaHdr {
                header_words: 12,
                num_chains: chains as u8,
                total_bytes: (chains * tones * 4) as u16,
                ..DmaHdr::default()
            },
            iq,
        }
    }

    /// A tone at unit magnitude and phase `p`, big enough that the i16
    /// quantisation is four decimal places below anything asserted.
    fn tone(p: f64) -> (f64, f64) {
        (10_000.0 * p.cos(), 10_000.0 * p.sin())
    }

    /// The mean magnitude is over LIVE positions, for the reason
    /// [`MIN_TONE_MAG`] gives: a guard tone is not a quiet subcarrier, it is
    /// not a subcarrier, and averaging it in scales the answer by however many
    /// of them this bandwidth happens to have.
    #[test]
    fn the_mean_magnitude_skips_the_dead_subcarriers() {
        let m: Vec<f32> = (0..64).map(|k| if k < 4 { 0.0 } else { 1000.0 }).collect();
        assert!(
            (mean_mag(&m).expect("sixty live tones") - 1000.0).abs() < 1e-9,
            "the dead tones dragged the mean down"
        );
        assert!(
            mean_mag(&[0.0f32; 16]).is_none(),
            "a payload with no live tone is not a channel response"
        );
    }

    /// The phase ramp across subcarriers is a clock, not a channel.
    ///
    /// Every CFR capture carries a phase that advances linearly with the
    /// subcarrier index, and the slope is the receiver's sampling-time offset
    /// against the transmitter — it changes with every re-sync of the same
    /// still link and swamps everything a chest does. Removing the
    /// least-squares slope across tones is what leaves the part of the phase
    /// that is the path.
    ///
    /// Two records of the SAME channel with two different ramps must produce
    /// the same scalar. Without the slope removal they differ by half the
    /// ramp, which is radians — hundreds of times a heartbeat.
    #[test]
    fn a_timing_ramp_across_tones_does_not_reach_the_mean_phase() {
        let theta = 0.4;
        let a = synth(2, 128, |_, k| tone(theta + 0.05 * k as f64));
        let b = synth(2, 128, |_, k| tone(theta - 0.09 * k as f64));
        let pa = mean_phase(&a).expect("a live record");
        let pb = mean_phase(&b).expect("a live record");

        assert!((pa - theta).abs() < 0.01, "ramp +0.05 gave {pa:.4}, not {theta}");
        assert!((pb - theta).abs() < 0.01, "ramp -0.09 gave {pb:.4}, not {theta}");
        assert!(
            (pa - pb).abs() < 0.02,
            "two ramps over one channel gave {pa:.4} and {pb:.4}; \
             the timing slope reached the scalar"
        );
    }

    /// What the slope removal must NOT remove: a rotation of the whole
    /// channel, which is what a body moving through the path does.
    #[test]
    fn a_rotation_of_the_whole_channel_does_reach_the_mean_phase() {
        let at = |theta: f64| {
            mean_phase(&synth(1, 128, move |_, k| tone(theta + 0.05 * k as f64)))
                .expect("a live record")
        };
        let moved = at(0.3) - at(0.0);
        assert!(
            (moved - 0.3).abs() < 0.01,
            "a 0.3 rad rotation of the channel moved the scalar by {moved:.4}"
        );
    }

    /// The guard and DC subcarriers arrive as near-zero I and Q, so their
    /// `atan2` is uniformly random. Carried into the unwrap they inject a
    /// 2-pi step into every tone after them, and the fitted slope — and with
    /// it the whole answer — is then whatever the noise chose.
    #[test]
    fn a_dead_subcarrier_does_not_break_the_unwrap() {
        // The shape a real 80 MHz block has: guards at both edges, DC in the
        // middle.
        let dead = |k: usize| !(3..=124).contains(&k) || k == 64;
        let r = synth(1, 128, |_, k| {
            if dead(k) {
                (0.0, 0.0)
            } else {
                tone(0.4 + 0.05 * k as f64)
            }
        });
        let p = mean_phase(&r).expect("a live record");
        assert!(
            (p - 0.4).abs() < 0.01,
            "seven dead subcarriers moved the scalar to {p:.4}"
        );
    }

    /// Both scalars, on the real capture, must be numbers a ring can hold.
    #[test]
    fn the_fixture_yields_finite_per_record_scalars() {
        let p = parse(FIXTURE);
        for r in &p.records {
            let m = mean_mag(&magnitudes(r)).expect("the fixture has live tones");
            assert!(m.is_finite() && m > 0.0, "mean magnitude {m}");
            let ph = mean_phase(r).expect("the fixture has live tones");
            assert!(
                ph.is_finite() && ph.abs() < 1e3,
                "mean phase {ph} is not a phase"
            );
        }
    }

    /// A record with no payload at all has no scalars, and must say so rather
    /// than divide by the zero tones it has.
    #[test]
    fn an_empty_record_has_no_scalars() {
        let r = synth(0, 0, |_, _| (0.0, 0.0));
        assert!(mean_phase(&r).is_none());
        assert!(mean_mag(&magnitudes(&r)).is_none());
    }

    /// A window that is not full reports nothing.
    ///
    /// Not a detail: a partly-filled window's variance is computed over fewer
    /// samples and is a different, noisier statistic. Emitting it would let the
    /// detector learn its ambient from one distribution and then judge against
    /// another, which is a false positive once per link at startup.
    #[test]
    fn a_partial_window_reports_nothing() {
        let mut w = CsiWindow::new(4);
        assert!(w.is_empty());
        for _ in 0..3 {
            w.push(vec![100.0; 64]);
            assert!(w.motion_energy().is_none(), "reported from a partial window");
        }
        w.push(vec![100.0; 64]);
        assert!(w.is_full());
        assert!(w.motion_energy().is_some());
    }

    /// A channel that does not move has zero energy, and reaches the data
    /// model as a number rather than as -inf.
    #[test]
    fn a_perfectly_still_channel_is_at_the_floor() {
        let mut w = CsiWindow::new(8);
        for _ in 0..8 {
            w.push(vec![250.0; 128]);
        }
        assert_eq!(w.motion_energy(), Some(0.0));
        assert_eq!(w.motion_energy_db(), Some(ENERGY_DB_MIN));
        let db = w.motion_energy_db().unwrap();
        assert!(db.is_finite(), "log10(0) reached the data model");
    }

    /// The energy is scale-invariant, which is the whole reason it is a ratio.
    ///
    /// A link at -50 dBm and a link at -80 dBm produce magnitudes an order of
    /// magnitude apart. If the feature were an unnormalised variance the loud
    /// link would always score higher, and the detector's learnt baseline would
    /// be a measure of how close the peer is rather than of how still the room
    /// is — so a client walking closer would read as motion forever after.
    #[test]
    fn the_energy_does_not_depend_on_how_loud_the_link_is() {
        let shape = |gain: f32| {
            let mut w = CsiWindow::new(10);
            for i in 0..10 {
                let wobble = 1.0 + 0.05 * ((i % 3) as f32 - 1.0);
                w.push((0..64).map(|t| gain * wobble * (10.0 + t as f32)).collect());
            }
            w.motion_energy().unwrap()
        };
        let quiet = shape(1.0);
        let loud = shape(50.0);
        assert!(quiet > 0.0);
        // Compared at f32 precision, which is what the magnitudes are stored
        // in: a 50x gain changes the last bits of every row, so a tolerance
        // tighter than f32 would be testing the storage, not the invariance.
        assert!(
            (quiet - loud).abs() / quiet < 1e-4,
            "a 50x louder link scored differently: {quiet} vs {loud}"
        );
    }

    /// Guard and DC subcarriers are never transmitted on, so they arrive as
    /// near-zero I and Q. Their variance is tiny but their MEAN is tinier, and
    /// `var / mean²` on a dead tone is enormous and entirely numerical.
    ///
    /// The fixture has about four per chain. Left in, that handful swamps the
    /// thousand real positions and the reported energy measures rounding.
    #[test]
    fn dead_subcarriers_are_excluded_from_the_energy() {
        let live = |dead: usize| {
            let mut w = CsiWindow::new(6);
            for i in 0..6 {
                let mut row: Vec<f32> = (0..64).map(|t| 400.0 + (t + i) as f32).collect();
                // Dead tones that flicker between two tiny values: a ratio of
                // hundreds each.
                for slot in row.iter_mut().take(dead) {
                    *slot = if i % 2 == 0 { 0.01 } else { 0.4 };
                }
                w.push(row);
            }
            w.motion_energy().unwrap()
        };
        let none = live(0);
        let some = live(4);
        assert!(
            (none - some).abs() / none < 0.05,
            "four dead tones moved the energy from {none} to {some}"
        );
    }

    /// The incremental accumulators must agree with the form they replaced.
    ///
    /// `motion_energy` now reads running per-position sums maintained on push
    /// and pop, which is O(width) instead of O(width x window) -- but it gets
    /// the variance from `E[x²] - E[x]²`, and that subtracts two nearly equal
    /// large numbers. At rest `sumsq/n` is ~251500 against a `mean²` of
    /// ~250000, so the cancellation is real and the question is only whether
    /// `f64` has digits to spare. It does, by about eight orders of magnitude.
    ///
    /// Driven with the magnitudes of the real fixture, perturbed the way
    /// consecutive captures actually differ, and compared against the naive
    /// two-pass reference to 1e-9 relative. Also drives the window well past
    /// `REBUILD_EVERY` so the periodic rebuild is on the tested path rather
    /// than a thing that only happens in production.
    #[test]
    fn the_incremental_energy_matches_the_two_pass_form() {
        let p = parse(FIXTURE);
        let base = magnitudes(&p.records[0]);

        let mut w = CsiWindow::new(20);
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };

        let mut checked = 0;
        for i in 0..(REBUILD_EVERY as usize + 200) {
            // A record that differs from its neighbour by a fraction of a
            // percent, which is what consecutive real captures do: the
            // measured record-to-record correlation is >= 0.997.
            let row: Vec<f32> = base
                .iter()
                .map(|&m| m * (1.0 + ((next() % 2000) as f32 - 1000.0) / 200_000.0))
                .collect();
            w.push(row);

            // Checked at the start, around the rebuild, and at the end -- not
            // on all 4296 pushes, which would make this the slowest test here
            // for no extra coverage.
            let near_rebuild = i.abs_diff(REBUILD_EVERY as usize) < 40;
            if i < 40 || near_rebuild || i > REBUILD_EVERY as usize + 160 {
                let (Some(inc), Some(two)) = (w.motion_energy(), w.motion_energy_two_pass())
                else {
                    continue;
                };
                assert!(
                    (inc - two).abs() / two < 1e-9,
                    "at push {i}: incremental {inc:e} vs two-pass {two:e} \
                     (relative {:e})",
                    (inc - two).abs() / two
                );
                assert!(inc > 0.0, "the perturbed window has real variance");
                checked += 1;
            }
        }
        assert!(checked > 100, "only {checked} windows were compared");

        // And the energy the whole module is judged by is unchanged by the
        // rewrite: a still 80 MHz link still reads about -22 dB.
        let mut q = CsiWindow::new(3);
        for r in &p.records {
            q.push(magnitudes(r));
        }
        let (inc, two) = (q.motion_energy().unwrap(), q.motion_energy_two_pass().unwrap());
        assert!((inc - two).abs() / two < 1e-9, "{inc} vs {two}");
    }

    /// A subtracted float does not return exactly where it came from, so an
    /// accumulator updated forever drifts from the rows it claims to describe
    /// -- and the drift is a plausible energy, so nothing downstream could
    /// tell. The periodic rebuild is what bounds it.
    #[test]
    fn the_accumulators_are_rebuilt_and_not_left_to_drift() {
        let mut w = CsiWindow::new(4);
        for i in 0..REBUILD_EVERY + 10 {
            w.push(vec![100.0 + (i % 7) as f32, 250.0, 0.5]);
        }
        assert!(
            w.since_rebuild <= REBUILD_EVERY,
            "the accumulators were never rebuilt"
        );

        // After a rebuild the sums must equal the rows exactly, because they
        // were just computed from them.
        w.rebuild();
        for p in 0..3 {
            let want: f64 = w.rows.iter().map(|r| f64::from(r[p])).sum();
            assert_eq!(w.sum[p], want, "position {p} drifted");
        }
        assert_eq!(
            w.motion_energy(),
            w.motion_energy_two_pass(),
            "a freshly rebuilt window must agree exactly"
        );
    }

    /// A bandwidth change under a running capture makes the two halves of the
    /// window incomparable: position 200 of a 40 MHz record and of an 80 MHz
    /// record are different subcarriers. Mixing them measures the
    /// reconfiguration, and it would read as a very large motion.
    #[test]
    fn a_width_change_clears_the_window_instead_of_mixing_two_shapes() {
        let mut w = CsiWindow::new(4);
        for _ in 0..4 {
            w.push(vec![100.0; 256]);
        }
        assert!(w.is_full());
        w.push(vec![100.0; 128]);
        assert_eq!(w.len(), 1, "the 256-wide rows were kept alongside 128-wide");
        assert!(w.motion_energy().is_none());
    }

    /// The number this module exists to produce, measured on real captures.
    ///
    /// An empty room on a QCN9074 mesh link reads -22 dB and holds it to
    /// within a dB across a 97-record capture. If this drifts, either the
    /// feature changed or the parser is reading the payload at the wrong
    /// offset — both of which would otherwise show up only as a sensor that
    /// never fires.
    #[test]
    fn a_quiet_room_measures_about_minus_twenty_two_db() {
        let p = parse(FIXTURE);
        let mut w = CsiWindow::new(3);
        for r in &p.records {
            w.push(magnitudes(r));
        }
        let db = w.motion_energy_db().expect("three records fill the window");
        assert!(
            (-30.0..-12.0).contains(&db),
            "a still 80 MHz link measured {db:.2} dB, not the expected ~-22"
        );
    }

    /// The reported energy is always a number in a range a controller can
    /// compare, whatever the radio hands over.
    #[test]
    fn the_reported_energy_is_always_within_its_range() {
        let mut w = CsiWindow::new(2);
        // One enormous step: the largest ratio a pair of records can produce.
        w.push(vec![1.5f32; 16]);
        w.push(vec![f32::from(i16::MAX); 16]);
        let db = w.motion_energy_db().unwrap();
        assert!(db.is_finite());
        assert!((ENERGY_DB_MIN..=ENERGY_DB_MAX).contains(&db), "got {db}");
    }

    // ── The detector ─────────────────────────────────────────────────────────

    /// A CSI detector with the learning phase and dwell shrunk so a test can
    /// reach `Idle` in a few hundred samples instead of six hundred. Every
    /// other constant is the shipped default: shrinking those would test a
    /// detector nobody runs.
    fn tuned(baseline_secs: f64, dwell_secs: f64) -> LinkDetector {
        let mut d = csi_detector();
        d.baseline_secs = baseline_secs;
        d.dwell_secs = dwell_secs;
        d
    }

    /// The detector must not be watching for a saturated link on a scale that
    /// has no such thing.
    ///
    /// `saturation_dbm` asks whether the absolute level is high enough that the
    /// receiver's gain stage is flickering. A motion energy near 0 dB is a
    /// perfectly ordinary busy channel, and the default -20 dBm threshold would
    /// put every link that ever saw motion into Saturated permanently — a
    /// sensor that switches itself off the first time it works.
    #[test]
    fn a_csi_link_is_never_saturated() {
        let mut d = tuned(10.0, 2.0);
        assert!(d.saturation_dbm.is_infinite());
        let mut t = 0.0;
        // Energies well above the RSSI default of -20, held long enough to
        // learn a baseline from them.
        for _ in 0..200 {
            d.push_f64(-5.0, t);
            t += 0.1;
        }
        assert_eq!(d.state(), State::Idle, "a busy channel read as saturated");
    }

    /// The end-to-end claim: a channel that starts moving raises Motion, and a
    /// channel that stops returns to Idle.
    ///
    /// Driven through `push_f64` with the energies a real link produces — a
    /// -22 dB ambient with the 0.6 dB of spread measured on the fixture-class
    /// capture, then a several-dB rise. A detector that needed retuning for
    /// this input would show up here as a test that cannot be made to fire.
    #[test]
    fn a_rise_in_channel_energy_raises_motion_and_returns() {
        let mut d = tuned(10.0, 2.0);
        let dt = 0.1;
        let mut t = 0.0;
        let rest = [-22.2, -22.5, -22.0, -22.3, -22.6, -22.1];

        for i in 0..400 {
            assert!(
                d.push_f64(rest[i % rest.len()], t).is_none(),
                "a still channel fired at sample {i}"
            );
            t += dt;
        }
        assert_eq!(d.state(), State::Idle, "must learn a baseline from the rest");

        let mut fired = false;
        for i in 0..60 {
            // A body in the path: some windows move several dB, some do not.
            let v = if i % 3 == 2 { -14.0 } else { -22.0 };
            if d.push_f64(v, t) == Some(Transition::ToMotion) {
                fired = true;
            }
            t += dt;
        }
        assert!(fired, "an 8 dB rise in channel energy must raise Motion");
        assert_eq!(d.motion_count(), 1);

        let mut returned = false;
        for i in 0..400 {
            if d.push_f64(rest[i % rest.len()], t) == Some(Transition::ToIdle) {
                returned = true;
            }
            t += dt;
        }
        assert!(returned, "the room emptied and the link stayed in Motion");
        assert_eq!(d.state(), State::Idle);
        assert_eq!(d.motion_count(), 1, "the return is not a second sighting");
    }

    /// The sub-dB structure of the resting energy is the reason `push_f64`
    /// exists. Rounded to whole dB every resting window becomes the same
    /// integer, and the link's learnt ambient variance is exactly zero.
    #[test]
    fn the_resting_spread_survives_reaching_the_detector() {
        // The six resting energies measured on the real capture. Rounded to
        // whole dB they collapse into two values, so an integer detector would
        // see a 1 dB square wave where the radio reported a 0.37 dB spread --
        // and a 1 dB flicker is precisely what `min_short_var` exists to
        // refuse. The integer path would therefore sit on the gate it is
        // supposed to sit well below.
        let rest: [f64; 6] = [-22.23, -22.57, -22.28, -22.56, -22.42, -22.20];
        let mut rounded: Vec<i32> = rest.iter().map(|v| v.round() as i32).collect();
        rounded.sort_unstable();
        rounded.dedup();
        assert_eq!(rounded, vec![-23, -22], "rounding collapses six values into two");
        let spread = rest.iter().cloned().fold(f64::MIN, f64::max)
            - rest.iter().cloned().fold(f64::MAX, f64::min);
        assert!(spread < 0.5, "the real spread is under half a dB: {spread}");

        let mut f = tuned(5.0, 2.0);
        let mut t = 0.0;
        for i in 0..200 {
            f.push_f64(rest[i % rest.len()], t);
            t += 0.1;
        }
        // The f64 path learns a real baseline somewhere inside the spread; the
        // integer path could only ever learn exactly -22.
        let b = f.baseline_dbm().expect("a baseline is learnt");
        assert!(
            (-22.6..-22.1).contains(&b) && (b - b.round()).abs() > 1e-6,
            "the fractional structure did not reach the detector: {b}"
        );
    }

    // ── Discovery and control ────────────────────────────────────────────────

    /// A capture's control file lives under its PHY's debugfs directory, so
    /// the phy each interface belongs to has to come out of `iw dev` with it.
    /// Both AP and mesh interfaces are wanted; `AP/VLAN` and `managed` are not.
    #[test]
    fn interfaces_are_found_with_the_phy_they_belong_to() {
        let dev = "\
phy#1
\tInterface phy1-mesh0
\t\tifindex 12
\t\ttype mesh point
\tInterface phy1-ap0
\t\tifindex 11
\t\ttype AP
phy#0
\tInterface phy0-ap0
\t\tifindex 10
\t\ttype AP
\tInterface phy0-sta0
\t\tifindex 9
\t\ttype managed
\tInterface phy0-vlan0
\t\ttype AP/VLAN
";
        assert_eq!(
            parse_dev_phys(dev),
            vec![
                ("phy1".to_string(), "phy1-mesh0".to_string(), LinkKind::Mesh),
                ("phy1".to_string(), "phy1-ap0".to_string(), LinkKind::Client),
                ("phy0".to_string(), "phy0-ap0".to_string(), LinkKind::Client),
            ]
        );
        assert!(parse_dev_phys("nonsense\n\ttype AP\n").is_empty());
        assert!(parse_dev_phys("").is_empty());
    }

    /// The bandwidth written into `cfr_capture` decides how many subcarriers
    /// the driver reports. Reading it wrong is not a visible failure — the
    /// records still arrive, with a quarter of the resolution.
    #[test]
    fn the_capture_width_comes_from_the_interface() {
        let info = |w: &str| format!("Interface phy1-ap0\n\tchannel 36 (5180 MHz), width: {w}, center1: 5210 MHz\n");
        assert_eq!(parse_width(&info("80 MHz")), 2);
        assert_eq!(parse_width(&info("40 MHz")), 1);
        assert_eq!(parse_width(&info("20 MHz")), 0);
        assert_eq!(parse_width(&info("160 MHz")), 3);
        // An interface that is down prints no width at all. 80 is the width of
        // the radio this feature exists for; 20 would look like a working
        // sensor at a quarter of the resolution.
        assert_eq!(parse_width("Interface phy1-ap0\n"), 2);
        assert_eq!(parse_width(""), 2);
        assert_eq!(bw_mhz(2), 80);
    }

    /// The control file path is the one the driver actually exposes, down to
    /// the case of the MAC: debugfs names station directories in lower case,
    /// and `iw` prints them that way while the data model reports upper.
    #[test]
    fn the_capture_path_uses_the_lower_case_mac_debugfs_names() {
        let mac = mac_bytes("02:0c:43:26:60:11").expect("a MAC parses");
        assert_eq!(mac, [0x02, 0x0c, 0x43, 0x26, 0x60, 0x11]);
        assert_eq!(
            capture_path("phy1", "phy1-mesh0", &mac),
            PathBuf::from(
                "/sys/kernel/debug/ieee80211/phy1/netdev:phy1-mesh0/stations/02:0c:43:26:60:11/cfr_capture"
            )
        );
        assert_eq!(mac_upper(&mac), "02:0C:43:26:60:11");
        assert_eq!(mac_lower(&mac), "02:0c:43:26:60:11");

        assert!(mac_bytes("not a mac").is_none());
        assert!(mac_bytes("02:0c:43:26:60").is_none(), "five octets");
        assert!(mac_bytes("02:0c:43:26:60:11:22").is_none(), "seven octets");
    }

    /// One AP interface on the CFR-capable radio, as `iw dev` prints it.
    const DEV: &str = "phy#1\n\tInterface m0\n\t\ttype AP\n";

    fn want(phy: &str, iface: &str, last: u8, bw: u8) -> Want {
        Want {
            phy: phy.into(),
            iface: iface.into(),
            mac: [0, 0, 0, 0, 0, last],
            bw,
            kind: LinkKind::Mesh,
        }
    }

    /// A peer that is already being captured must not be restarted every five
    /// seconds: each restart costs the driver two writes and the link its
    /// window, so the feature would never accumulate one.
    #[test]
    fn an_unchanged_peer_is_left_alone() {
        let mut active = HashMap::new();
        active.insert(("phy1".to_string(), "m0".to_string(), [0, 0, 0, 0, 0, 1]), 2);

        let (starts, stops) = plan(vec![want("phy1", "m0", 1, 2)], &active);
        assert!(starts.is_empty(), "a running capture was restarted");
        assert!(stops.is_empty());
    }

    /// A peer that left has its capture stopped, or the radio goes on sounding
    /// a station that is not there for as long as the device is up.
    #[test]
    fn a_departed_peer_is_stopped() {
        let mut active = HashMap::new();
        active.insert(("phy1".to_string(), "m0".to_string(), [0, 0, 0, 0, 0, 1]), 2);
        active.insert(("phy1".to_string(), "m0".to_string(), [0, 0, 0, 0, 0, 2]), 2);

        let (starts, stops) = plan(vec![want("phy1", "m0", 1, 2)], &active);
        assert!(starts.is_empty());
        assert_eq!(
            stops,
            vec![("phy1".to_string(), "m0".to_string(), [0, 0, 0, 0, 0, 2])]
        );
    }

    /// A radio moved to a different width keeps sending records shaped for the
    /// old one, because the driver holds the parameters from the original
    /// write. The capture has to be stopped and started, not left running.
    #[test]
    fn a_width_change_restarts_the_capture() {
        let mut active = HashMap::new();
        active.insert(("phy1".to_string(), "m0".to_string(), [0, 0, 0, 0, 0, 1]), 2);

        let (starts, stops) = plan(vec![want("phy1", "m0", 1, 1)], &active);
        assert_eq!(starts.len(), 1, "the capture must be restarted at the new width");
        assert_eq!(starts[0].bw, 1);
        assert_eq!(stops.len(), 1, "and stopped at the old one first");
    }

    /// A link created after the first discovery pass must still know which
    /// interface it is on and which side of the radio it is.
    ///
    /// The peer names have to come from what DISCOVERY found, not from what
    /// was just started: after the first pass almost nothing is started
    /// because the captures are already running, so a map built from the
    /// starts is empty and every later link falls back to the radio's name and
    /// to `Client`. A mesh peer would then be reported as a client -- the one
    /// distinction the `Kind` parameter exists to make -- and only for links
    /// created more than five seconds after the agent started, which is to say
    /// almost all of them.
    #[test]
    fn peers_are_named_from_discovery_not_from_what_was_just_started() {
        let desired = vec![
            Want {
                phy: "phy1".into(),
                iface: "phy1-mesh0".into(),
                mac: [0, 0, 0, 0, 0, 1],
                bw: 2,
                kind: LinkKind::Mesh,
            },
            Want {
                phy: "phy1".into(),
                iface: "phy1-ap0".into(),
                mac: [0, 0, 0, 0, 0, 2],
                bw: 2,
                kind: LinkKind::Client,
            },
        ];
        // Everything is already running, so `plan` starts nothing.
        let active: HashMap<CapKey, u8> = desired
            .iter()
            .map(|w| ((w.phy.clone(), w.iface.clone(), w.mac), w.bw))
            .collect();
        let (starts, stops) = plan(desired.clone(), &active);
        assert!(starts.is_empty() && stops.is_empty(), "nothing should change");

        let peers = name_peers(&desired);
        assert_eq!(
            peers.get(&("phy1".to_string(), [0, 0, 0, 0, 0, 1])),
            Some(&("phy1-mesh0".to_string(), LinkKind::Mesh)),
            "a mesh peer was renamed a client once its capture was already running"
        );
        assert_eq!(
            peers.get(&("phy1".to_string(), [0, 0, 0, 0, 0, 2])),
            Some(&("phy1-ap0".to_string(), LinkKind::Client))
        );

        // And the name actually reaches the data model.
        let mut rig = Rig::default();
        rig.tick(
            batch("phy1", vec![sample(1, 1.0, vec![100.0; 32])]),
            &peers,
            1.0,
            4,
        );
        assert_eq!(rig.table[0].kind, LinkKind::Mesh);
        assert_eq!(rig.table[0].iface, "phy1-mesh0");
    }

    /// A failed `iw dev` must not tear down every capture.
    ///
    /// Discovery is a fork, and a fork fails for reasons that have nothing to
    /// do with the radios -- momentary memory pressure, `iw` losing a race with
    /// netifd reloading an interface. Treating that as "no peers" stops every
    /// capture; each link then loses its window and its learnt baseline and
    /// spends a minute back in Learning, having sensed nothing, for a fork that
    /// failed once.
    ///
    /// The distinction is between a discovery that FAILED and one that
    /// succeeded and found nobody. The second must stop the captures, or a
    /// station that left is sounded for as long as the device is up.
    #[test]
    fn a_failed_discovery_is_not_the_same_as_an_empty_one() {
        let active: HashMap<CapKey, u8> =
            [(("phy1".to_string(), "m0".to_string(), [0, 0, 0, 0, 0, 1]), 2)]
                .into_iter()
                .collect();

        // `iw dev` printed nothing at all: a failure, not an answer.
        assert!(
            discover_peers_from("", &["phy1".to_string()], &[], |_| {
                (Some(String::new()), Some(2))
            })
            .is_none(),
            "a failed fork was read as a device with no peers"
        );

        // Radios exist and have no peers: a real answer, and it must stop the
        // capture that is running for a peer that is gone.
        let empty = discover_peers_from(DEV, &["phy1".into()], &[], |_| {
            (Some(String::new()), Some(2))
        })
        .expect("listing interfaces is a successful discovery");
        assert!(empty.is_empty(), "no stations were in the dump");
        let (starts, stops) = plan(empty, &active);
        assert!(starts.is_empty());
        assert_eq!(stops.len(), 1, "a peer that genuinely left must be stopped");
    }

    /// `iw dev` still lists the interface, but its STATION DUMP failed.
    ///
    /// The third case, and the one that reads as the second. An empty dump and
    /// a failed dump are identical on stdout -- both print nothing -- so only
    /// the exit status separates "nobody is associated" from "the command did
    /// not run". Read as the former, a fork that lost a race with netifd stops
    /// every capture on the interface, and each link loses its window and its
    /// learnt baseline and spends a minute back in Learning having sensed
    /// nothing.
    ///
    /// The previous wants are therefore carried forward on FAILURE only. The
    /// successful-but-empty case above must still stop the captures, or a
    /// station that walked out of the building is sounded until the device
    /// reboots.
    #[test]
    fn a_failed_station_dump_keeps_the_peers_it_had() {
        let previous = vec![Want {
            phy: "phy1".into(),
            iface: "m0".into(),
            mac: [0, 0, 0, 0, 0, 1],
            bw: 2,
            kind: LinkKind::Mesh,
        }];
        let active: HashMap<CapKey, u8> =
            [(("phy1".to_string(), "m0".to_string(), [0, 0, 0, 0, 0, 1]), 2)]
                .into_iter()
                .collect();

        // The dump command failed.
        let kept = discover_peers_from(DEV, &["phy1".into()], &previous, |_| (None, Some(2)))
            .expect("the interface is still listed, so discovery succeeded");
        assert_eq!(kept, previous, "a failed dump discarded the interface's peers");
        let (starts, stops) = plan(kept, &active);
        assert!(
            starts.is_empty() && stops.is_empty(),
            "a failed dump changed the captures"
        );

        // The same interface, dump succeeding and genuinely empty.
        let gone = discover_peers_from(DEV, &["phy1".into()], &previous, |_| {
            (Some(String::new()), None)
        })
        .expect("a successful dump is a successful discovery");
        assert!(gone.is_empty(), "an empty dump must not resurrect old peers");
        let (_, stops) = plan(gone, &active);
        assert_eq!(stops.len(), 1, "a peer that genuinely left must be stopped");
    }

    /// The relay is per radio and finite. A busy AP asking for every associated
    /// client would overrun it and lose captures from every peer including the
    /// mesh links worth having — silently, because a dropped sub-buffer leaves
    /// nothing in the record stream.
    #[test]
    fn the_number_of_captures_is_capped_and_the_cap_is_stable() {
        let many: Vec<Want> = (0..40).map(|i| want("phy1", "m0", i, 2)).collect();
        let (starts, _) = plan(many.clone(), &HashMap::new());
        assert_eq!(starts.len(), MAX_CAPTURES);

        // Stable across ticks: an unstable cap would alternate which peers it
        // drops, costing two writes a tick and every dropped peer its baseline.
        let mut shuffled = many;
        shuffled.reverse();
        let (again, _) = plan(shuffled, &HashMap::new());
        assert_eq!(starts, again, "the cap chose a different set the second time");
    }

    // ── The table ────────────────────────────────────────────────────────────

    fn sample(peer: u8, at: f64, mag: Vec<f32>) -> Sample {
        Sample {
            peer: [0, 0, 0, 0, 0, peer],
            mean_mag: mean_mag(&mag),
            // A constant phase. The tests around this helper are about the
            // table and the detector; a link whose phase never moves simply
            // reports no vitals, which is the quiet answer they expect.
            mean_phase: Some(0.0),
            mag,
            bw: 2,
            at,
        }
    }

    fn batch(phy: &str, samples: Vec<Sample>) -> Vec<Batch> {
        vec![Batch {
            phy: phy.into(),
            samples,
            errors: 0,
            resyncs: 0,
        }]
    }

    /// The pair of structures one tick operates on, driven in the order `run`
    /// drives them: `features` first, holding no lock, then `apply`.
    ///
    /// Tests go through this rather than calling `apply` directly so that the
    /// split between the two is exercised, and so that a test which drives an
    /// EMPTY tick reaches the eviction -- which is the whole point of item 1.
    #[derive(Default)]
    struct Rig {
        table: Vec<CsiLink>,
        windows: HashMap<(String, [u8; 6]), WinState>,
        /// The shipped tuning unless a test replaces it.
        tune: Tuning,
    }

    impl Rig {
        fn tick(
            &mut self,
            batches: Vec<Batch>,
            peers: &PeerMap,
            now: f64,
            cap: usize,
        ) -> Vec<Change> {
            let (feats, streams) = features(batches, &mut self.windows, &self.tune, cap, now);
            let changes = apply(&mut self.table, feats, &streams, peers, &self.tune, now);
            // The two passes `run` does between and after its lock
            // acquisitions. Driven here so that every test through this rig
            // exercises the same order the agent runs them in, and so a test
            // can assert that the transform is NOT part of `apply`.
            let idle: Vec<(WinKey, Option<f64>)> = self
                .table
                .iter()
                .map(|l| ((l.phy.clone(), l.key), l.idle_since))
                .collect();
            let estimates = estimate_vitals(&mut self.windows, &idle, now);
            store_vitals(&mut self.table, &estimates);
            changes
        }
    }

    fn peer_map() -> PeerMap {
        let mut m = PeerMap::new();
        m.insert(
            ("phy1".to_string(), [0, 0, 0, 0, 0, 1]),
            ("phy1-mesh0".to_string(), LinkKind::Mesh),
        );
        m
    }

    /// A link is created by the first record from a peer, and takes its
    /// interface and kind from the discovery pass that started the capture.
    #[test]
    fn a_record_creates_a_link_named_by_the_discovery_that_asked_for_it() {
        let mut rig = Rig::default();
        rig.tick(
            batch("phy1", vec![sample(1, 1.0, vec![100.0; 32])]),
            &peer_map(),
            1.0,
            4,
        );
        assert_eq!(rig.table.len(), 1);
        assert_eq!(rig.table[0].iface, "phy1-mesh0");
        assert_eq!(rig.table[0].kind, LinkKind::Mesh);
        assert_eq!(rig.table[0].peer, "00:00:00:00:00:01");
        assert_eq!(rig.table[0].records, 1);

        // A peer nobody asked for is reported under the radio's own name
        // rather than dropped: a record from a capture this agent did not
        // start is something an operator should be able to see.
        rig.tick(
            batch("phy1", vec![sample(9, 1.0, vec![100.0; 32])]),
            &peer_map(),
            1.0,
            4,
        );
        assert_eq!(rig.table.len(), 2);
        let stray = rig.table.iter().find(|l| l.key[5] == 9).unwrap();
        assert_eq!(stray.iface, "phy1", "an unasked-for peer lost its radio name");
    }

    /// `RecordsPerSec` is the health counter that distinguishes "the room is
    /// empty" from "this radio stopped answering". It has to fall back to zero
    /// on its own when records stop, without anything clearing it.
    #[test]
    fn the_record_rate_is_a_rate_and_decays_on_its_own() {
        let mut rig = Rig::default();
        let mut t = 0.0;
        for _ in 0..10 {
            rig.tick(
                batch("phy1", vec![sample(1, t, vec![100.0; 32])]),
                &peer_map(),
                t,
                4,
            );
            t += 0.1;
        }
        let m = render(&rig.table, true, t);
        let base = format!("Device.X_OptimACS_Sensing.Csi.{}", rig.table[0].instance);
        assert_eq!(
            m.get(&format!("{base}.RecordsPerSec")),
            Some(&"10".to_string()),
            "ten records in the last second"
        );
        assert_eq!(m.get(&format!("{base}.Records")), Some(&"10".to_string()));

        // Half a minute later, with nothing arriving.
        let m = render(&rig.table, true, t + 30.0);
        assert_eq!(
            m.get(&format!("{base}.RecordsPerSec")),
            Some(&"0".to_string()),
            "the rate did not decay and reads as a live radio"
        );
        assert_eq!(
            m.get(&format!("{base}.Records")),
            Some(&"10".to_string()),
            "the cumulative count must not decay"
        );
    }

    /// A peer that vanishes while its detector says Motion must be corrected
    /// before it is dropped: the instance is about to leave the GET tree, so
    /// "Motion" would be the controller's last and uncorrectable word on it.
    #[test]
    fn an_evicted_link_that_was_in_motion_is_reported_idle_first() {
        let mut rig = Rig::default();
        let peers = peer_map();
        let mut t = 0.0;

        // A still channel, long enough to learn a baseline.
        for i in 0..900 {
            let row: Vec<f32> = (0..32).map(|x| 400.0 + ((i + x) % 3) as f32).collect();
            rig.tick(batch("phy1", vec![sample(1, t, row)]), &peers, t, 4);
            t += 0.1;
        }
        assert_eq!(rig.table[0].detector.state(), State::Idle, "baseline must be learnt");

        // Then a channel that keeps moving. The kick VARIES from record to
        // record, and that is not decoration: the detector measures the
        // variance OF THE ENERGY, so a channel that jumps to a new level and
        // holds it there fires once and returns to Idle within the dwell, as
        // soon as the step leaves the short window. Simulated both ways
        // against this pipeline, a fixed repeating disturbance ends at Idle
        // with a score of 0.0 while this one holds Motion at a score of ~6.
        // A person in a doorway is the second kind, not the first.
        for i in 0..120 {
            let kick = ((i * 37) % 23) as f32 * 12.0;
            let row: Vec<f32> = (0..32).map(|x| 400.0 + kick + (x % 5) as f32).collect();
            rig.tick(batch("phy1", vec![sample(1, t, row)]), &peers, t, 4);
            t += 0.1;
        }
        assert_eq!(rig.table[0].detector.state(), State::Motion, "the channel moved");
        let instance = rig.table[0].instance;

        // THE TICK PATH, with an EMPTY drain -- the shape a quiet relay
        // produces. `run` used to `continue` past `apply` on an empty drain,
        // which skipped the only code that retracts and drops a link; a peer
        // that left while in Motion then stayed Motion for the life of the
        // process. Driven through `Rig::tick` rather than `apply` so the skip
        // cannot come back without this failing.
        let changes = rig.tick(Vec::new(), &peers, t + STALE_SECS + 1.0, 4);
        assert!(rig.table.is_empty(), "the stale link must be dropped");
        assert!(
            rig.windows.is_empty(),
            "the evicted link left its magnitudes behind"
        );
        assert_eq!(
            changes,
            vec![Change {
                instance,
                iface: "phy1-mesh0".to_string(),
                peer: "00:00:00:00:00:01".to_string(),
                param: "State",
                value: "Idle",
            }],
            "an evicted link in Motion must be retracted, on its own instance"
        );
    }

    /// A peer that stopped reporting and came back has a hole in its history.
    /// Joining the two sides of it puts a step the size of the whole channel
    /// change inside the window, which is exactly the shape motion has.
    #[test]
    fn a_gap_in_the_records_does_not_look_like_motion() {
        let mut rig = Rig::default();
        let peers = peer_map();
        let key = ("phy1".to_string(), [0, 0, 0, 0, 0, 1]);
        let mut t = 0.0;
        for _ in 0..10 {
            rig.tick(
                batch("phy1", vec![sample(1, t, vec![400.0; 32])]),
                &peers,
                t,
                4,
            );
            t += 0.1;
        }
        assert!(rig.windows[&key].window.is_full());

        // Away for ten seconds, back on a different path.
        t += 10.0;
        rig.tick(
            batch("phy1", vec![sample(1, t, vec![900.0; 32])]),
            &peers,
            t,
            4,
        );
        assert_eq!(
            rig.windows[&key].window.len(),
            1,
            "the two sides of the gap were joined into one window"
        );
    }

    /// Framing failures are reported per link because that is where a
    /// controller can act on them, even though they belong to the stream: a
    /// record whose end magic is wrong has no trustworthy peer address.
    #[test]
    fn relay_framing_errors_reach_every_link_on_that_radio() {
        let mut rig = Rig::default();
        rig.tick(
            vec![Batch {
                phy: "phy1".into(),
                samples: vec![sample(1, 1.0, vec![100.0; 32]), sample(2, 1.0, vec![100.0; 32])],
                errors: 7,
                resyncs: 3,
            }],
            &peer_map(),
            1.0,
            4,
        );
        assert_eq!(rig.table.len(), 2);
        assert!(
            rig.table.iter().all(|l| l.errors == 7 && l.resyncs == 3),
            "a stream-level fault must be visible from every link on the stream"
        );

        // And a relay handing over NOTHING BUT rubbish still updates them:
        // that is the tick on which a controller most needs to tell "the room
        // is empty" from "this radio is producing garbage".
        rig.tick(
            vec![Batch {
                phy: "phy1".into(),
                samples: Vec::new(),
                errors: 9,
                resyncs: 5,
            }],
            &peer_map(),
            1.0,
            4,
        );
        assert!(
            rig.table.iter().all(|l| l.errors == 9),
            "a record-free tick did not carry the framing counters"
        );

        let m = render(&rig.table, true, 1.0);
        let base = format!("Device.X_OptimACS_Sensing.Csi.{}", rig.table[0].instance);
        assert_eq!(m.get(&format!("{base}.ParseErrors")), Some(&"9".to_string()));
    }

    // ── The data model ───────────────────────────────────────────────────────

    /// The reported tree is what the controller actually sees. Instance numbers
    /// come from the link, not from iteration order, so a peer that ages out
    /// does not renumber the ones that stayed.
    #[test]
    fn the_reported_tree_carries_one_instance_per_link() {
        let mut link = CsiLink {
            instance: 4,
            phy: "phy1".into(),
            iface: "phy1-mesh0".into(),
            peer: "02:0C:43:26:60:11".into(),
            key: [0x02, 0x0c, 0x43, 0x26, 0x60, 0x11],
            kind: LinkKind::Mesh,
            detector: csi_detector(),
            records: 512,
            arrivals: VecDeque::from(vec![9.5, 9.7, 9.9]),
            errors: 2,
            resyncs: 1,
            bw: 2,
            energy_db: Some(-22.347),
            last_seen: 10.0,
            last_motion_at: Some("2026-09-22T10:00:00+00:00".into()),
            vitals: Vitals::default(),
            vitals_span: 30.0,
            idle_since: None,
            fall: FallDetector::new(12.0, 10.0),
            last_fall_at: Some("2026-09-22T10:01:00+00:00".into()),
        };
        link.detector.push_f64(-22.0, 0.0);

        let m = render(std::slice::from_ref(&link), true, 10.0);
        let base = "Device.X_OptimACS_Sensing.Csi.4";

        assert_eq!(
            m.get("Device.X_OptimACS_Sensing.CsiEnable"),
            Some(&"1".to_string())
        );
        assert_eq!(
            m.get("Device.X_OptimACS_Sensing.CsiNumberOfEntries"),
            Some(&"1".to_string())
        );
        assert_eq!(m.get(&format!("{base}.Interface")), Some(&"phy1-mesh0".to_string()));
        assert_eq!(
            m.get(&format!("{base}.Peer")),
            Some(&"02:0C:43:26:60:11".to_string())
        );
        assert_eq!(m.get(&format!("{base}.Kind")), Some(&"mesh".to_string()));
        assert_eq!(m.get(&format!("{base}.State")), Some(&"Learning".to_string()));
        assert_eq!(
            m.get(&format!("{base}.MotionEnergyDb")),
            Some(&"-22.35".to_string()),
            "two decimals"
        );
        assert_eq!(m.get(&format!("{base}.RecordsPerSec")), Some(&"3".to_string()));
        assert_eq!(m.get(&format!("{base}.Records")), Some(&"512".to_string()));
        assert_eq!(m.get(&format!("{base}.ParseErrors")), Some(&"2".to_string()));
        assert_eq!(
            m.get(&format!("{base}.LastMotionAt")),
            Some(&"2026-09-22T10:00:00+00:00".to_string())
        );
        assert_eq!(
            m.get(&format!("{base}.Bandwidth")),
            Some(&"80".to_string()),
            "reported in MHz, not as the driver's code"
        );
        // A link that has never been still long enough reports no vitals at
        // all -- and reports the fall flag anyway, because "no candidate" is
        // something the agent knows.
        assert_eq!(m.get(&format!("{base}.BreathingBpm")), Some(&String::new()));
        assert_eq!(m.get(&format!("{base}.HeartBpm")), Some(&String::new()));
        assert_eq!(
            m.get(&format!("{base}.VitalsConfidence")),
            Some(&String::new())
        );
        assert_eq!(
            m.get(&format!("{base}.FallCandidate")),
            Some(&"0".to_string()),
            "the fall flag is a boolean, never empty"
        );
        assert_eq!(
            m.get(&format!("{base}.LastFallAt")),
            Some(&"2026-09-22T10:01:00+00:00".to_string())
        );

        // A link with no window yet reports an empty energy, not a zero: 0 dB
        // is a real energy a real link could have.
        let fresh = CsiLink {
            instance: 9,
            energy_db: None,
            last_motion_at: None,
            ..link
        };
        let m = render(&[fresh], true, 10.0);
        assert_eq!(
            m.get("Device.X_OptimACS_Sensing.Csi.9.MotionEnergyDb"),
            Some(&String::new()),
            "an unmeasured energy must be empty, not a fabricated 0"
        );
        assert_eq!(
            m.get("Device.X_OptimACS_Sensing.Csi.9.LastMotionAt"),
            Some(&String::new())
        );
    }

    /// A `Feature` with an energy stated outright, so a test can drive the
    /// shape of a fall rather than build magnitudes that happen to have that
    /// variance. The energy is what `apply` consumes; how it was computed is
    /// `CsiWindow`'s business and is tested there.
    fn feature(at: f64, db: f64) -> Feature {
        Feature {
            phy: "phy1".into(),
            peer: [0, 0, 0, 0, 0, 1],
            bw: 2,
            at,
            energy_db: Some(db),
            resync: false,
        }
    }

    /// `FallCandidate` notifies on BOTH edges, on its own path, with the same
    /// instance the link's `State` uses.
    ///
    /// Both edges because a controller that is told about a 1 and never about
    /// the 0 has a device that says somebody is on the floor for the rest of
    /// the day. The vitals do NOT appear here and must not: they change every
    /// two seconds, and a Notify each time is a Notify storm for a number the
    /// poll already carries.
    #[test]
    fn a_fall_candidate_notifies_on_both_edges() {
        let peers = peer_map();
        let tune = Tuning {
            fall_still_secs: 1.0,
            ..Tuning::default()
        };
        let mut table = Vec::new();

        // One record creates the link; its detector is then shrunk so it
        // learns an ambient in seconds rather than in ten minutes.
        apply(&mut table, vec![feature(0.0, -22.0)], &[], &peers, &tune, 0.0);
        table[0].detector = tuned(2.0, 1.0);

        let drive = |table: &mut Vec<CsiLink>, from: f64, secs: f64, db: f64| {
            let feats: Vec<Feature> = (0..(secs * 10.0) as i64)
                .map(|i| feature(from + i as f64 / 10.0, db))
                .collect();
            let now = from + secs;
            apply(table, feats, &[], &peers, &tune, now)
        };

        // Five seconds of ambient so the detector leaves Learning and has a
        // baseline for the fall stage to measure against.
        let learn = drive(&mut table, 0.1, 5.0, -22.0);
        assert!(
            table[0].detector.baseline_dbm().is_some(),
            "the detector never left Learning: {learn:?}"
        );

        // The fall: half a second of impact, then stillness.
        let mut changes = drive(&mut table, 5.1, 0.5, -6.0);
        changes.extend(drive(&mut table, 5.6, 4.0, -22.0));

        let falls: Vec<&Change> = changes
            .iter()
            .filter(|c| c.param == "FallCandidate")
            .collect();
        assert_eq!(falls.len(), 1, "expected one raise, got {changes:?}");
        assert_eq!(falls[0].value, "1");
        assert_eq!(falls[0].instance, table[0].instance);
        assert_eq!(falls[0].peer, "00:00:00:00:00:01");
        assert!(
            table[0].last_fall_at.is_some(),
            "LastFallAt was not stamped"
        );

        let m = render(&table, true, 9.6);
        let base = format!("Device.X_OptimACS_Sensing.Csi.{}", table[0].instance);
        assert_eq!(m.get(&format!("{base}.FallCandidate")), Some(&"1".to_string()));

        // Getting up clears it, on the same path.
        let cleared = drive(&mut table, 9.6, 2.0, -9.0);
        let backs: Vec<&Change> = cleared
            .iter()
            .filter(|c| c.param == "FallCandidate")
            .collect();
        assert_eq!(backs.len(), 1, "expected one clear, got {cleared:?}");
        assert_eq!(backs[0].value, "0");
        assert!(!table[0].fall.candidate());
    }

    /// The transform is not part of `apply`, and only its result is.
    ///
    /// `apply` runs under the mutex a data-model GET contends for. A 512-point
    /// FFT per series per link, plus the vectors it used to allocate, ran there
    /// — which is exactly what the split between `features` and `apply` exists
    /// to prevent, and what this module's own comments claimed was not
    /// happening. The window now lives beside the run loop's other windows and
    /// the link holds four scalars.
    ///
    /// Asserted behaviourally rather than structurally: `apply` alone, given a
    /// full window's worth of records, produces a link with no vitals on it,
    /// and the estimate appears only once the out-of-lock pass has run.
    #[test]
    fn the_transform_runs_outside_apply_and_only_its_result_reaches_the_link() {
        let peers = peer_map();
        let tune = Tuning {
            vitals_span: 20.0,
            ..Tuning::default()
        };
        let mut windows: HashMap<WinKey, WinState> = HashMap::new();
        let mut table: Vec<CsiLink> = Vec::new();

        // Thirty seconds of a still link with a 0.25 Hz breath on it, driven
        // through `features` and `apply` ONLY -- the two halves of the tick
        // that take the lock between them.
        let mut now = 0.0;
        for i in 0..300 {
            now = f64::from(i) / 10.0;
            let breath = (std::f64::consts::TAU * 0.25 * now).sin();
            let row = vec![100.0 + 2.0 * breath as f32; 32];
            let mut sm = sample(1, now, row);
            sm.mean_mag = Some(100.0 + 2.0 * breath);
            sm.mean_phase = Some(0.0);
            let (feats, streams) = features(batch("phy1", vec![sm]), &mut windows, &tune, 4, now);
            apply(&mut table, feats, &streams, &peers, &tune, now);
        }

        assert_eq!(table.len(), 1, "the link was never created");
        assert_eq!(
            table[0].vitals,
            Vitals::default(),
            "apply produced an estimate, so the transform ran under the lock"
        );
        assert!(
            windows.values().next().is_some(),
            "the vitals ring is not beside the other windows"
        );

        // Now the pass that runs with no lock held, and the copy-back.
        table[0].idle_since = Some(0.0);
        let idle: Vec<(WinKey, Option<f64>)> = table
            .iter()
            .map(|l| ((l.phy.clone(), l.key), l.idle_since))
            .collect();
        let estimates = estimate_vitals(&mut windows, &idle, now);
        assert_eq!(estimates.len(), 1, "the transform produced nothing");
        store_vitals(&mut table, &estimates);

        let bpm = table[0]
            .vitals
            .breathing_bpm
            .expect("the breath did not reach the link");
        assert!((bpm - 15.0).abs() <= 1.0, "reported {bpm:.2} BPM, not 15");

        // And it does not re-run inside the two-second gate, so the storing
        // pass takes no lock on the ticks in between.
        assert!(
            estimate_vitals(&mut windows, &idle, now + 0.25).is_empty(),
            "the transform ran again a quarter of a second later"
        );
    }

    /// An estimate is about the thirty seconds behind it, so it stops meaning
    /// anything the moment the records stop — and the link survives a further
    /// thirty seconds after that before it is evicted.
    #[test]
    fn vitals_go_blank_when_the_records_stop_not_when_the_link_is_evicted() {
        let mut link = CsiLink {
            instance: 3,
            phy: "phy1".into(),
            iface: "phy1-mesh0".into(),
            peer: "00:00:00:00:00:01".into(),
            key: [0, 0, 0, 0, 0, 1],
            kind: LinkKind::Mesh,
            detector: csi_detector(),
            records: 0,
            arrivals: VecDeque::new(),
            errors: 0,
            resyncs: 0,
            bw: 2,
            energy_db: Some(-22.0),
            last_seen: 100.0,
            last_motion_at: None,
            vitals: Vitals::default(),
            vitals_span: 30.0,
            idle_since: None,
            fall: FallDetector::new(12.0, 10.0),
            last_fall_at: None,
        };
        let mut w = VitalsWindow::new(30.0, 0.35, 0.1);
        for i in 0..300 {
            let t = f64::from(i) / 10.0;
            w.push(t, 100.0 + 2.0 * (std::f64::consts::TAU * 0.25 * t).sin(), 0.0);
        }
        let _ = w.update(30.0, Some(0.0));
        link.vitals = w.vitals();
        assert!(link.vitals.breathing_bpm.is_some(), "the fixture is wrong");

        let fresh = render(std::slice::from_ref(&link), true, 110.0);
        assert!(
            !fresh["Device.X_OptimACS_Sensing.Csi.3.BreathingBpm"].is_empty(),
            "a link still reporting must keep its estimate"
        );

        // Records stopped 31 s ago: still in the table (STALE_SECS is 60), but
        // the window the estimate was made over no longer holds any of them.
        let stale = render(std::slice::from_ref(&link), true, 131.0);
        assert_eq!(
            stale["Device.X_OptimACS_Sensing.Csi.3.BreathingBpm"],
            String::new(),
            "a link that stopped reporting kept its breathing rate"
        );
        assert_eq!(
            stale["Device.X_OptimACS_Sensing.Csi.3.VitalsConfidence"],
            String::new()
        );
        assert_eq!(
            stale["Device.X_OptimACS_Sensing.Csi.3.RecordsPerSec"],
            "0".to_string(),
            "the link itself must still be reported"
        );
    }

    /// `LastFallAt` is the time of the RECORD, not the time the agent noticed.
    ///
    /// A drain runs a quarter-second after the records it carries, a candidate
    /// matures `csi_fall_still_secs` after the impact, and a backlog can be
    /// seconds deep. Stamped at drain time, a fall that happened at 10:00:00
    /// reads 10:00:10 or later — and the one thing anybody reading this
    /// parameter wants is when it happened.
    #[test]
    fn the_fall_time_is_the_record_s_and_not_the_drain_s() {
        let before = chrono::Utc::now();
        // A record 12.5 s back on the process clock.
        let stamped = wall_clock(100.0, 112.5);
        let parsed = chrono::DateTime::parse_from_rfc3339(&stamped)
            .expect("LastFallAt must be RFC 3339");

        let back = before.signed_duration_since(parsed).num_milliseconds();
        assert!(
            (12_000..=13_000).contains(&back),
            "a record 12.5 s old was stamped {back} ms back"
        );
        // A stamp from the future is a clamp artefact, not a prophecy.
        let now = wall_clock(200.0, 100.0);
        let ahead = chrono::DateTime::parse_from_rfc3339(&now).unwrap();
        assert!(
            ahead.signed_duration_since(before).num_milliseconds() >= 0,
            "a record stamped after `now` went backwards"
        );
    }

    /// An instance that is about to leave the tree must not leave a standing
    /// fall candidate behind it: that would be the controller's last and
    /// uncorrectable word on the link.
    #[test]
    fn an_evicted_link_with_a_standing_candidate_is_retracted_first() {
        let peers = peer_map();
        let tune = Tuning::default();
        let mut table = Vec::new();
        apply(&mut table, vec![feature(0.0, -22.0)], &[], &peers, &tune, 0.0);
        // Raised by hand: the raising is `FallDetector`'s to test, the
        // retraction on eviction is `apply`'s. Driven as an UNBROKEN stream of
        // records, because a fall no longer matures across a gap -- an earlier
        // version of this fixture jumped from 0.4 s to 11 s and passed only
        // because of the defect that abort now fixes.
        table[0].fall = FallDetector::new(12.0, 1.0);
        for i in 0..30 {
            let t = f64::from(i) / 10.0;
            let db = if (5..10).contains(&i) { -6.0 } else { -22.0 };
            table[0].fall.push(db, Some(-22.0), t);
        }
        assert!(table[0].fall.candidate(), "the fixture did not raise one");

        let changes = apply(&mut table, Vec::new(), &[], &peers, &tune, STALE_SECS + 20.0);
        assert!(table.is_empty(), "the stale link must be dropped");
        assert!(
            changes
                .iter()
                .any(|c| c.param == "FallCandidate" && c.value == "0"),
            "an evicted link with a candidate must be retracted: {changes:?}"
        );
    }

    /// Consent is reported even with nothing to report: a controller must be
    /// able to tell "switched off" from "switched on and quiet".
    #[test]
    fn a_disabled_sensor_still_reports_its_switch() {
        let m = render(&[], false, 0.0);
        assert_eq!(
            m.get("Device.X_OptimACS_Sensing.CsiEnable"),
            Some(&"0".to_string())
        );
        assert_eq!(
            m.get("Device.X_OptimACS_Sensing.CsiNumberOfEntries"),
            Some(&"0".to_string())
        );
        assert_eq!(m.len(), 2, "an empty table must report nothing else");
    }

    /// Nothing a degenerate link can hold may reach the controller as a
    /// non-number: a NaN is a parameter value no comparison it makes is true
    /// for, which is worse than a wrong number because it looks like a working
    /// sensor.
    #[test]
    fn no_link_can_put_a_non_number_into_the_data_model() {
        let link = CsiLink {
            instance: 1,
            phy: "phy1".into(),
            iface: String::new(),
            peer: String::new(),
            key: [0; 6],
            kind: LinkKind::Client,
            detector: csi_detector(),
            records: 0,
            arrivals: VecDeque::new(),
            errors: 0,
            resyncs: 0,
            bw: 99,
            energy_db: Some(f64::NAN),
            last_seen: 0.0,
            last_motion_at: None,
            vitals: Vitals::default(),
            vitals_span: 30.0,
            idle_since: None,
            fall: FallDetector::new(12.0, 10.0),
            last_fall_at: None,
        };
        let m = render(&[link], true, 0.0);
        // `format!("{:.2}")` renders a NaN as "NaN" and an infinity as "inf",
        // and either reaches the controller as a value no comparison it makes
        // is true for. Both spellings are asserted: the earlier version of
        // this test checked only "inf", and a NaN sailed through it.
        assert_eq!(
            m.get("Device.X_OptimACS_Sensing.Csi.1.MotionEnergyDb"),
            Some(&String::new()),
            "a NaN energy must render empty, not as the text \"NaN\""
        );
        assert!(
            !m.values().any(|v| v.contains("NaN") || v.contains("inf")),
            "a non-number reached the data model: {m:?}"
        );
        assert_eq!(
            m.get("Device.X_OptimACS_Sensing.Csi.1.Bandwidth"),
            Some(&"80".to_string()),
            "an unknown width must read as a width, not as 0"
        );
    }

    /// The vitals parameters are floats out of a transform, which is the other
    /// place a NaN can come from. A degenerate estimate must render empty for
    /// exactly the reason a degenerate energy does.
    #[test]
    fn a_degenerate_vitals_estimate_renders_empty_not_as_text() {
        let mut link = CsiLink {
            instance: 1,
            phy: "phy1".into(),
            iface: "phy1-mesh0".into(),
            peer: "00:00:00:00:00:01".into(),
            key: [0, 0, 0, 0, 0, 1],
            kind: LinkKind::Mesh,
            detector: csi_detector(),
            records: 0,
            arrivals: VecDeque::new(),
            errors: 0,
            resyncs: 0,
            bw: 2,
            energy_db: Some(-22.0),
            last_seen: 0.0,
            last_motion_at: None,
            vitals: Vitals::default(),
            vitals_span: 30.0,
            idle_since: None,
            fall: FallDetector::new(12.0, 10.0),
            last_fall_at: None,
        };
        // A window of NaNs is what a dead link produces; the ring drops them,
        // so the estimate stays absent rather than becoming a non-number. Run
        // through a real window and copied onto the link the way the run loop
        // does it, because the link now holds only the result.
        let mut w = VitalsWindow::new(30.0, 0.35, 0.1);
        for i in 0..400 {
            w.push(f64::from(i) / 10.0, f64::NAN, f64::NAN);
        }
        let _ = w.update(40.0, Some(0.0));
        link.vitals = w.vitals();
        link.last_seen = 40.0;

        let m = render(std::slice::from_ref(&link), true, 40.0);
        assert!(
            !m.values().any(|v| v.contains("NaN") || v.contains("inf")),
            "a non-number reached the data model: {m:?}"
        );
        assert_eq!(
            m.get("Device.X_OptimACS_Sensing.Csi.1.BreathingBpm"),
            Some(&String::new())
        );
    }

    /// A still link with a chest in the path reports the rates, to two
    /// decimals of confidence, on the paths the controller subscribes to.
    #[test]
    fn a_still_link_reports_its_vitals_on_the_data_model() {
        let mut link = CsiLink {
            instance: 7,
            phy: "phy1".into(),
            iface: "phy1-mesh0".into(),
            peer: "00:00:00:00:00:01".into(),
            key: [0, 0, 0, 0, 0, 1],
            kind: LinkKind::Mesh,
            detector: csi_detector(),
            records: 0,
            arrivals: VecDeque::new(),
            errors: 0,
            resyncs: 0,
            bw: 2,
            energy_db: Some(-22.0),
            last_seen: 0.0,
            last_motion_at: None,
            vitals: Vitals::default(),
            vitals_span: 30.0,
            idle_since: None,
            fall: FallDetector::new(12.0, 10.0),
            last_fall_at: None,
        };
        let mut w = VitalsWindow::new(30.0, 0.35, 0.1);
        for i in 0..300 {
            let t = f64::from(i) / 10.0;
            let breath = (std::f64::consts::TAU * 0.25 * t).sin();
            w.push(t, 100.0 + 2.0 * breath, 0.0);
        }
        let _ = w.update(30.0, Some(0.0));
        link.vitals = w.vitals();
        link.last_seen = 30.0;

        let m = render(std::slice::from_ref(&link), true, 30.0);
        let base = "Device.X_OptimACS_Sensing.Csi.7";
        let br: f64 = m[&format!("{base}.BreathingBpm")]
            .parse()
            .expect("a breathing rate must be a number");
        assert!((br - 15.0).abs() <= 1.0, "reported {br} BPM, not 15");
        let conf = &m[&format!("{base}.VitalsConfidence")];
        assert_eq!(conf.len(), 4, "two decimals, got {conf:?}");
        assert!(
            m[&format!("{base}.HeartBpm")].is_empty(),
            "a breathing-only window reported a heart rate"
        );
    }

    // ── Lifecycle ────────────────────────────────────────────────────────────

    /// Records from one read must be placed in time by the radio's own clock,
    /// not all at the instant the read returned.
    ///
    /// A `read` of the relay routinely hands over several records captured a
    /// period apart -- at 100 ms, three spanning 300 ms. Stamped alike they
    /// tell the detector three soundings happened at once, which distorts the
    /// dwell and gap arithmetic and turns a steady ten records a second into
    /// bursts of three.
    #[test]
    fn records_from_one_read_are_spread_by_the_hardware_clock() {
        let p = parse(FIXTURE);
        let now = 1000.0;
        let mut seen = HashMap::new();
        let out = stamp(&p.records, now, &mut seen);
        assert_eq!(out.len(), 3);

        // The last record is the one that genuinely just arrived.
        assert_eq!(out[2].at, now);
        // The fixture's timestamps are 6685 and 6456 microseconds apart.
        assert!((out[1].at - (now - 0.006456)).abs() < 1e-9, "{}", out[1].at);
        assert!(
            (out[0].at - (now - 0.013141)).abs() < 1e-9,
            "{}",
            out[0].at
        );
        assert!(
            out[0].at < out[1].at && out[1].at < out[2].at,
            "records must keep their order in time"
        );

        // A 100 ms capture, which is what the shipped default produces: the
        // three records must land ~100 ms apart, not together.
        let spread: Vec<Record> = (0..3)
            .map(|i| Record {
                timestamp: 500_000 + i * 103_000,
                ..p.records[0].clone()
            })
            .collect();
        let out = stamp(&spread, now, &mut HashMap::new());
        assert!((out[2].at - out[1].at - 0.103).abs() < 1e-6);
        assert!((out[1].at - out[0].at - 0.103).abs() < 1e-6);
    }

    /// A peer's stamps must never run backwards across reads.
    ///
    /// The anchor moves forward with each read, but the back-dating is
    /// measured from within a read, so the two cross: a read arriving 50 ms
    /// after the last one -- `RELAY_POLL` -- can carry a record back-dated up
    /// to 300 ms, placing it BEFORE a record already delivered. The detector
    /// would see a negative interval in its dwell arithmetic, and the
    /// `RecordsPerSec` window would discard records it should count.
    #[test]
    fn a_peer_s_stamps_never_run_backwards_across_reads() {
        let p = parse(FIXTURE);
        let mut seen = HashMap::new();

        // A first read, anchored well forward.
        let first = stamp(&p.records[..1], 1000.0, &mut seen);
        assert_eq!(first[0].at, 1000.0);

        // A second read 50 ms later carrying THREE records, the oldest of
        // which back-dates 13 ms -- unclamped it would land at 1000.037, which
        // is fine, so force the crossing with a record that back-dates further
        // than the reads are apart.
        let deep: Vec<Record> = (0..2)
            .map(|i| Record {
                timestamp: 1_000_000 + i * 200_000,
                ..p.records[0].clone()
            })
            .collect();
        let second = stamp(&deep, 1000.05, &mut seen);
        assert!(
            second[0].at >= 1000.0,
            "a record landed before one already delivered: {} < 1000.0",
            second[0].at
        );
        assert_eq!(second[0].at, 1000.0, "the clamp pins it to the last stamp");
        assert_eq!(second[1].at, 1000.05, "the anchor is untouched");

        // Monotonic over a long run of reads, which is the property that
        // matters rather than any one value.
        let mut last = second[1].at;
        let mut t = 1000.05;
        for _ in 0..50 {
            t += 0.05;
            let out = stamp(&deep, t, &mut seen);
            for sample in &out {
                assert!(
                    sample.at >= last,
                    "time went backwards: {} after {last}",
                    sample.at
                );
                last = sample.at;
            }
        }

        // A different peer keeps its own clock, or one busy link would drag
        // every other link on the radio forward with it.
        let other = Record {
            peer: [9, 9, 9, 9, 9, 9],
            ..p.records[0].clone()
        };
        let out = stamp(&[other], t, &mut seen);
        assert_eq!(out[0].at, t, "an unrelated peer inherited another's clamp");
    }

    /// The hardware counter is a `u32` of microseconds and wraps every ~72
    /// minutes. A wrap makes one delta absurd, and back-dating a record by an
    /// hour would push it past the stale threshold and resync the link -- a
    /// fault once an hour, on the hour, for a counter doing what counters do.
    #[test]
    fn a_wrapped_hardware_clock_falls_back_to_the_read_time() {
        let p = parse(FIXTURE);
        let now = 1000.0;
        let absurd: Vec<Record> = [0u32, 1]
            .iter()
            .map(|&i| Record {
                // The first record's counter sits just after a wrap, so the
                // last-minus-this delta is most of the u32 range.
                timestamp: if i == 0 { 10 } else { 4_000_000_000 },
                ..p.records[0].clone()
            })
            .collect();
        let out = stamp(&absurd, now, &mut HashMap::new());
        assert_eq!(out[1].at, now, "the anchor is always the read time");
        assert_eq!(
            out[0].at, now,
            "an out-of-range delta must fall back, not back-date by an hour"
        );
        assert!(stamp(&[], now, &mut HashMap::new()).is_empty());
    }

    /// The inbox is a `static` and outlives a run of the agent, which `main`
    /// re-enters 30 s after a connection failure. Whatever the last run left
    /// there was captured before that gap; replayed now it arrives as a burst
    /// of up to 2048 records carrying current timestamps, describing a room
    /// nobody was watching.
    #[test]
    fn a_new_run_does_not_replay_the_last_run_s_records() {
        deliver("phy-test-stale", vec![sample(1, 1.0, vec![1.0; 4])], 3, 2);
        assert!(
            drain_inbox().iter().any(|b| b.phy == "phy-test-stale"),
            "the sample was not queued in the first place"
        );

        deliver("phy-test-stale", vec![sample(1, 1.0, vec![1.0; 4])], 3, 2);
        clear_inbox();
        assert!(
            !drain_inbox().iter().any(|b| b.phy == "phy-test-stale"),
            "a stale record survived the start of a new run"
        );
    }

    /// A relay file that is missing stays missing. Retrying is right -- a
    /// radio can come back -- but saying so every two seconds buries the first
    /// line, which is the only one that says when it started, in a log that is
    /// a ring buffer in RAM.
    #[test]
    fn a_missing_relay_file_is_logged_once_then_rarely() {
        let mut b = Backoff::new();
        assert!(b.should_log(), "the first failure must always be logged");

        // The next few are swallowed; the complaint interval grows fourfold.
        let mut logged = 1;
        for _ in 0..8 {
            if b.should_log() {
                logged += 1;
            }
        }
        assert_eq!(logged, 1, "the complaint did not back off at all");

        // The retry grows too, and both are bounded.
        let mut waits = Vec::new();
        for _ in 0..12 {
            waits.push(b.next_wait());
        }
        assert_eq!(waits[0], REOPEN_BACKOFF);
        assert!(waits[1] > waits[0], "the retry did not back off");
        assert_eq!(
            *waits.last().unwrap(),
            REOPEN_BACKOFF_MAX,
            "the retry must be bounded, or a radio that comes back is noticed late"
        );
        assert!(b.log_gap <= OPEN_WARN_MAX, "the complaint interval is unbounded");

        // And a file that opens forgets all of it, so the NEXT outage is
        // reported promptly rather than inheriting an hour-old backoff.
        b.reset();
        assert_eq!(b.attempts, 0);
        assert_eq!(b.wait, REOPEN_BACKOFF);
        assert!(b.should_log(), "a later outage was silenced by an earlier one");
    }

    /// A reader thread is the only source of records for its radio, so one
    /// that dies takes that radio's sensing with it -- silently, because a
    /// dead reader and an idle radio both produce nothing.
    #[test]
    fn a_dead_reader_is_restarted_with_a_bounded_backoff() {
        // No call below reaches `Reader::start`, so no real reader thread is
        // ever spawned -- this test is about the supervision arithmetic, and a
        // reader here would open a relay path that does not exist on a build
        // host and warn about it for the life of the run. The handle is always
        // `Some` when `restart_if_dead` is called, and that path records the
        // death and returns; a restart needs a LATER call, with the handle
        // taken and the backoff expired.
        let stop = Arc::new(AtomicBool::new(false));
        let mut r = Reader::new("phy-test".into());
        assert!(r.handle.is_none());

        // A thread that has already ended: what a panicked reader looks like.
        let finished = || {
            let h = std::thread::spawn(|| {});
            while !h.is_finished() {
                std::hint::spin_loop();
            }
            h
        };

        // Repeated deaths: the backoff must climb and be capped.
        for _ in 0..10 {
            r.handle = Some(finished());
            r.restart_if_dead(&stop);
        }
        assert_eq!(r.deaths, 10, "deaths were not counted");
        assert_eq!(
            r.backoff, READER_RETRY_MAX,
            "the restart backoff must be bounded"
        );

        // A SPAWN FAILURE must take the same path. Under `EAGAIN` this is a
        // sustained condition, so a mere log line meant a warning every tick,
        // four a second forever, and a retry just as often on a device already
        // out of the resource being asked for.
        let mut f = Reader::new("phy-test".into());
        f.spawn_fn = |_, _| {
            Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "injected EAGAIN",
            ))
        };
        let live = Arc::new(AtomicBool::new(false));
        for _ in 0..10 {
            // `retry_at` is honoured, so force each attempt to be due.
            f.retry_at = Instant::now();
            f.restart_if_dead(&live);
        }
        assert!(f.handle.is_none(), "a failed spawn must not leave a handle");
        assert_eq!(f.deaths, 10, "spawn failures were not counted");
        assert_eq!(
            f.backoff, READER_RETRY_MAX,
            "a failed spawn did not back off; this warns every tick forever"
        );
        assert!(
            f.retry_at > Instant::now(),
            "the next attempt was not put off"
        );

        // The last death took the handle, and the backoff now stretches a
        // minute ahead, so nothing is restarted yet.
        assert!(r.handle.is_none());
        assert!(r.retry_at > Instant::now(), "the backoff was not applied");

        // And during shutdown a dead reader is left dead: restarting one then
        // would race the radio being switched off under it.
        let shutting_down = Arc::new(AtomicBool::new(true));
        r.handle = Some(finished());
        let before = r.deaths;
        r.restart_if_dead(&shutting_down);
        assert_eq!(r.deaths, before, "a reader was reaped during shutdown");
        assert!(
            r.handle.is_some(),
            "shutdown must leave the handle alone, not respawn behind it"
        );
    }

    /// A width probe that FAILED must not be read as a width.
    ///
    /// `parse_width` defaults to 80 MHz when it cannot find one, which is right
    /// for output that lacks the line and wrong for output that never arrived.
    /// On a 40 MHz radio a failed probe reports 80, `plan` sees a width change
    /// and restarts every capture on the interface at the wrong width, and the
    /// next successful probe flips it back -- each flip costing every link its
    /// window and its learnt baseline, for a fork that failed.
    #[test]
    fn a_failed_width_probe_keeps_the_width_the_interface_had() {
        let dump = "Station 00:00:00:00:00:01 (on m0)\n\tsignal:\t-50 dBm\n";
        let at_40 = vec![Want {
            phy: "phy1".into(),
            iface: "m0".into(),
            mac: [0, 0, 0, 0, 0, 1],
            bw: 1,
            kind: LinkKind::Client,
        }];

        // The info command failed. The 40 MHz width must survive.
        let kept = discover_peers_from(DEV, &["phy1".into()], &at_40, |_| {
            (Some(dump.to_string()), None)
        })
        .expect("discovery succeeded");
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].bw, 1, "a failed width probe flipped the radio to 80 MHz");

        // And it must not restart the capture, which is the damage that
        // flipping would do.
        let active: HashMap<CapKey, u8> =
            [(("phy1".to_string(), "m0".to_string(), [0, 0, 0, 0, 0, 1]), 1)]
                .into_iter()
                .collect();
        let (starts, stops) = plan(kept, &active);
        assert!(
            starts.is_empty() && stops.is_empty(),
            "a failed width probe restarted the capture"
        );

        // A genuine width change still takes effect.
        let moved = discover_peers_from(DEV, &["phy1".into()], &at_40, |_| {
            (Some(dump.to_string()), Some(2))
        })
        .expect("discovery succeeded");
        assert_eq!(moved[0].bw, 2, "a real width change was ignored");

        // And an interface nobody has seen before falls back to the default.
        let fresh = discover_peers_from(DEV, &["phy1".into()], &[], |_| {
            (Some(dump.to_string()), None)
        })
        .expect("discovery succeeded");
        assert_eq!(fresh[0].bw, DEFAULT_BW);
    }

    /// One reader per process, whatever the agent does.
    ///
    /// `agent::run` is re-entered after every connection failure, so `spawn` is
    /// called again and again over a device's uptime. A second reader would
    /// mean two threads draining one relay, each seeing half the records, and
    /// two discovery passes fighting over the same `cfr_capture` files.
    ///
    /// The task never reaches a radio here: its first act is to look for
    /// CFR-capable phys, and the test runtime is gone before then.
    #[tokio::test]
    async fn a_second_spawn_is_refused_and_the_slot_is_released() {
        let (tx, _rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
        let id = EndpointId::new("os::00005A::test");

        let off = Arc::new(ClientConfig::default());
        assert!(
            spawn(Arc::clone(&off), tx.clone(), id.clone()).is_none(),
            "channel sensing is off by default and must not start"
        );
        assert!(
            !RUNNING.load(Ordering::Acquire),
            "a refusal must not claim the slot"
        );

        let on = Arc::new(ClientConfig {
            csi_enabled: true,
            ..ClientConfig::default()
        });
        let first = spawn(Arc::clone(&on), tx.clone(), id.clone());
        assert!(first.is_some(), "the first reader must start");
        assert!(
            spawn(Arc::clone(&on), tx.clone(), id).is_none(),
            "a second reader must be refused while the first holds the slot"
        );

        drop(first);
        {
            let _guard = RunningGuard;
        }
        assert!(
            !RUNNING.load(Ordering::Acquire),
            "the slot must be released or sensing is off until the next reboot"
        );
    }

    /// A stalled connection must cost the notification, not the reader: the
    /// reader threads go on producing records whatever the uplink does, and a
    /// blocked drain would turn a stalled TCP connection into lost captures.
    #[test]
    fn a_full_status_channel_drops_the_notification_instead_of_blocking() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
        tx.try_send(vec![0xAA]).expect("the channel starts empty");

        let id = EndpointId::new("os::00005A::test");
        let change = Change {
            instance: 4,
            iface: "phy1-mesh0".into(),
            peer: "00:00:00:00:00:01".into(),
            param: "State",
            value: "Motion",
        };
        notify(&tx, &id, "controller", &change);

        assert_eq!(
            rx.try_recv().expect("the queued record survives"),
            vec![0xAA],
            "the record already in flight must not be displaced"
        );
        assert!(
            rx.try_recv().is_err(),
            "the notification must have been dropped, not queued behind a stall"
        );
    }

    /// The queue between the reader threads and the agent is bounded, and it
    /// drops the OLDEST when it overflows.
    ///
    /// A reader that blocked on a full queue would stop draining the relay,
    /// and an unbounded one would grow a router out of memory the first time
    /// the runtime stalled. When the agent has fallen behind, the recent
    /// channel is what a detector can still use.
    #[test]
    fn a_stalled_drain_costs_the_oldest_records_not_the_newest() {
        let mut slot = Inbox::default();
        for i in 0..(INBOX_CAP + 100) {
            slot.samples.push(sample(1, i as f64, Vec::new()));
        }
        // The same trim `deliver` applies.
        let excess = slot.samples.len() - INBOX_CAP;
        slot.samples.drain(..excess);
        slot.dropped += excess as u64;

        assert_eq!(slot.samples.len(), INBOX_CAP);
        assert_eq!(slot.dropped, 100);
        assert_eq!(
            slot.samples.last().map(|s| s.at),
            Some((INBOX_CAP + 99) as f64),
            "the newest record was dropped instead of the oldest"
        );
    }
}
