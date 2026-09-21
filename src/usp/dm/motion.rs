//! Passive motion sensing from RSSI variance — `Device.X_OptimACS_Sensing.Motion.*`.
//!
//! A person walking between two radios changes the multipath between them, and
//! the link's reported signal moves several dB from sample to sample. A still
//! room does not do that: an idle link sits on one integer dBm, or dithers by a
//! dB. So "is someone moving" is answerable from readings the device already
//! takes for link quality — no extra radio, no extra traffic, no camera.
//!
//! # Consent
//!
//! Off by default, for the same reason `dm::sensing` is: presence in a home is
//! personal data. Motion here is not anonymous — it is "somebody is moving in
//! this house, now", tied to a device with an address. Nothing switches it on
//! but an operator writing `motion_enabled` into the config, and when it is off
//! the sampler is never spawned, so the device is not merely silent about
//! presence, it never computes it.
//!
//! # Why the detector is split from the sampler
//!
//! [`LinkDetector`] is pure: samples in, transitions out, no clock of its own
//! and no I/O. That is what makes the hard part testable — the false-positive
//! behaviour is the whole product, and it cannot be tested against a radio with
//! a person obligingly walking past. The sampler half is the part that forks
//! `iw`, and it holds no interesting logic.
//!
//! # Why only transitions are reported
//!
//! A ValueChange Notify per sample at 2 Hz per link would be several thousand
//! records an hour from a quiet house, and the controller would have to
//! rediscover the state changes from that stream anyway. The device already
//! knows when the state changed; it says so, and says nothing in between. The
//! current score and baseline are still readable with a GET, for an operator
//! who wants to see why.
//!
//! # Instance numbers are stable and sparse
//!
//! `Motion.{i}` is assigned once per tracked link and never changes while that
//! link lives. It has to be: a ValueChange Notify names an instance, and a
//! controller that was told `Motion.4` entered Motion must be able to come
//! back later and find `Motion.4` meaning the same radio and the same peer.
//! Numbering by position in the table would renumber every link behind one
//! that aged out, and silently reattribute a live state to the wrong peer.
//!
//! The cost is that the numbering is SPARSE. Instances are never reused, so
//! after a day on a busy AP the table may hold `Motion.2`, `Motion.57` and
//! `Motion.190` and nothing else. `MotionNumberOfEntries` is therefore a
//! COUNT, not a range: it says how many rows exist, never what they are
//! called, and walking `1..=MotionNumberOfEntries` finds mostly nothing. Read
//! the sub-tree by its prefix, which is how TR-369 says to walk a
//! multi-instance object anyway.
//!
//! # Consent is not settable over USP
//!
//! `Device.X_OptimACS_Sensing.MotionEnable` is reported and is read-only. See
//! the comment on it in `render`.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use log::{debug, info, warn};

use crate::config::ClientConfig;
use crate::usp::agent::StatusSender;
use crate::usp::endpoint::EndpointId;
use crate::usp::message::{build_value_change_notify, encode_msg};
use crate::usp::record;

// ── The detector ─────────────────────────────────────────────────────────────

/// What a link is doing, as far as the detector can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Still gathering the ambient baseline. Nothing is reported from here:
    /// with nothing to compare against, any reading is as plausible as any
    /// other, and firing would mean every reboot announces a presence.
    Learning,
    /// Ambient. The link is behaving the way it does when nobody is moving.
    Idle,
    /// The link's short-term variance is well above its own ambient.
    Motion,
    /// The link is too strong to sense and is left alone.
    ///
    /// Found on the lab mesh the day this shipped: two nodes a metre apart
    /// peer at -8 dBm, and at that level the receiver's gain stage toggles
    /// between two readings 7 dB apart with nobody in the room. The other end
    /// of the same path, at -12 dBm, was quiet the whole time -- so it is the
    /// receiver, not the air. Reported as its own state rather than folded
    /// into `Idle`, because "we are not looking" and "nobody is there" must
    /// never read the same on the controller.
    Saturated,
}

impl State {
    /// The value reported for `...Motion.{i}.State`.
    fn as_str(self) -> &'static str {
        match self {
            State::Learning => "Learning",
            State::Idle => "Idle",
            State::Motion => "Motion",
            State::Saturated => "Saturated",
        }
    }
}

/// A state change worth telling the controller about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    ToMotion,
    ToIdle,
}

/// Per-link motion detector, fed one signal reading at a time.
///
/// Every threshold is a field rather than a constant so a test can shrink the
/// learning phase and the dwell without shrinking the parts under test, and so
/// an operator chasing a false positive on one odd link could in principle be
/// given knobs without a recompile.
#[derive(Debug, Clone)]
pub struct LinkDetector {
    // ── Outlier rejection ────────────────────────────────────────────────────
    /// Samples in the Hampel ring. Odd, so it has a centre.
    pub hampel_window: usize,
    /// How many robust sigmas from the median before a sample is suspect.
    pub hampel_sigmas: f64,
    /// Smallest scale the Hampel stage will believe, in dB.
    ///
    /// A still link reports one integer dBm, so its MAD is exactly 0 and every
    /// deviation is infinitely many sigmas. Floored at 0.5 dB, which with
    /// `hampel_sigmas` = 3 puts the rejection threshold at 1.5 dB — above the
    /// 1 dB step that is just ath11k rounding to whole dBm, below anything a
    /// body does.
    pub mad_floor: f64,
    // ── Baseline ─────────────────────────────────────────────────────────────
    /// How long to watch a link before believing its ambient statistics.
    pub baseline_secs: f64,
    /// A learnt baseline stronger than this, in dBm, puts the link in
    /// [`State::Saturated`] instead of [`State::Idle`]. -20 dBm is well above
    /// anything a link across a room produces (the lab's longer links sit at
    /// -28 to -68) and well below the -8 that misbehaved.
    pub saturation_dbm: f64,
    /// EWMA rate at which the frozen baseline follows a drifting link.
    pub baseline_alpha: f64,
    // ── Detection ────────────────────────────────────────────────────────────
    /// Samples in the short window whose variance is compared to the baseline.
    pub short_window: usize,
    /// Smallest baseline variance used as a divisor, in dB².
    ///
    /// Without it a perfectly flat link divides by zero, and a link with a
    /// hair of variance turns 1 dB of quantisation flicker into a score of
    /// hundreds.
    pub var_floor: f64,
    /// Short-window variance below which Motion is impossible whatever the
    /// ratio says, in dB².
    ///
    /// With the shipped constants this is exactly `on_ratio` × `var_floor`,
    /// so on a flat link it is the same test twice and looks redundant. It is
    /// not: it is the half that survives someone lowering `on_ratio` to make
    /// the detector more sensitive, and it is what keeps 1 dB of quantisation
    /// flicker from becoming a presence report when they do.
    pub min_short_var: f64,
    /// Ratio of short variance to ambient variance that means motion.
    pub on_ratio: f64,
    /// Ratio below which the link counts as ambient again.
    pub off_ratio: f64,
    /// Consecutive evaluations above `on_ratio` before Motion is declared.
    pub on_evals: usize,
    /// How long the link must stay below `off_ratio` before returning to Idle.
    pub dwell_secs: f64,

    // ── State ────────────────────────────────────────────────────────────────
    state: State,
    /// Raw readings, newest last. The Hampel stage judges its centre.
    raw: VecDeque<i32>,
    /// Filtered readings feeding the short-window variance.
    short: VecDeque<f64>,
    /// Running ambient statistics, kept as counts rather than a growing vector
    /// so a link whose clock never advances cannot exhaust memory.
    ambient_n: u64,
    ambient_sum: f64,
    ambient_sumsq: f64,
    /// Timestamp of the first sample, on the caller's clock.
    start_at: Option<f64>,
    baseline_mean: f64,
    baseline_var: f64,
    score: f64,
    above: usize,
    below_since: Option<f64>,
    motion_count: u32,
    last_motion_at: Option<f64>,
}

impl Default for LinkDetector {
    fn default() -> Self {
        Self {
            hampel_window: 7,
            hampel_sigmas: 3.0,
            mad_floor: 0.5,
            baseline_secs: 60.0,
            saturation_dbm: -20.0,
            baseline_alpha: 0.01,
            short_window: 10,
            var_floor: 0.25,
            min_short_var: 1.0,
            on_ratio: 4.0,
            off_ratio: 2.0,
            on_evals: 2,
            dwell_secs: 5.0,

            state: State::Learning,
            raw: VecDeque::new(),
            short: VecDeque::new(),
            ambient_n: 0,
            ambient_sum: 0.0,
            ambient_sumsq: 0.0,
            start_at: None,
            baseline_mean: 0.0,
            baseline_var: 0.0,
            score: 0.0,
            above: 0,
            below_since: None,
            motion_count: 0,
            last_motion_at: None,
        }
    }
}

impl LinkDetector {
    /// Feed one signal reading, taken at `now_secs` on the caller's clock.
    ///
    /// Returns the state change this sample caused, if any. `None` is the
    /// overwhelmingly common answer and means "nothing to report", not "no
    /// opinion".
    pub fn push(&mut self, dbm: i32, now_secs: f64) -> Option<Transition> {
        if self.start_at.is_none() {
            self.start_at = Some(now_secs);
        }
        let value = self.hampel(dbm)?;

        self.short.push_back(value);
        while self.short.len() > self.short_window() {
            self.short.pop_front();
        }

        if self.state == State::Learning {
            self.learn(value, now_secs);
            return None;
        }
        if self.state == State::Saturated {
            return None;
        }
        if self.short.len() < self.short_window() {
            return None;
        }
        self.evaluate(value, now_secs)
    }

    /// What the link is doing.
    pub fn state(&self) -> State {
        self.state
    }

    /// Short-window variance as a multiple of the ambient variance. 0 until
    /// the first evaluation, and 0 on a link that never moves at all.
    pub fn score(&self) -> f64 {
        self.score
    }

    /// The ambient signal level, or `None` while still learning it.
    pub fn baseline_dbm(&self) -> Option<f64> {
        match self.state {
            State::Learning => None,
            _ => Some(self.baseline_mean),
        }
    }

    /// How many times this link has entered Motion since it was first seen.
    pub fn motion_count(&self) -> u32 {
        self.motion_count
    }

    /// When Motion was last entered, on the clock the caller passes to
    /// [`push`](Self::push). Rendering that as a wall-clock time is the
    /// sampler's job — the detector deliberately has no calendar.
    pub fn last_motion_at(&self) -> Option<f64> {
        self.last_motion_at
    }

    /// Hampel stage: judge the CENTRE of the ring against its neighbours.
    ///
    /// Judging the newest sample instead would be cheaper and wrong: with a
    /// flat ring behind it, the first sample of a genuine burst looks exactly
    /// like a spike, and so does the next, and the detector could never fire
    /// at all. The centre is judged with three later samples already in hand,
    /// which is what distinguishes a lone artefact from the start of
    /// something.
    ///
    /// A suspect centre is only replaced when it is the ONLY suspect sample in
    /// its window. That is the whole difference between a spurious reading and
    /// motion: a driver artefact is isolated, a person is not. Without this
    /// test an 8 dB alternation — the exact signature being detected — would
    /// be filtered away sample by sample, because half a window of outliers
    /// still leaves the MAD at zero.
    ///
    /// Returns `None` until the ring is full; those first few samples are lost
    /// once per link, at startup, and are not worth a special case.
    fn hampel(&mut self, dbm: i32) -> Option<f64> {
        // Clamped at use rather than validated at construction: the tuning
        // fields are public, a zero window indexes an empty ring, and a panic
        // in here kills the sampler task for every link on the device.
        let window = self.hampel_window.max(1);
        self.raw.push_back(dbm);
        while self.raw.len() > window {
            self.raw.pop_front();
        }
        if self.raw.len() < window {
            return None;
        }

        let mut sorted: Vec<f64> = self.raw.iter().map(|&v| f64::from(v)).collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let median = sorted[sorted.len() / 2];

        let mut devs: Vec<f64> = sorted.iter().map(|v| (v - median).abs()).collect();
        devs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        // 1.4826 rescales a MAD into the standard deviation of a normal
        // distribution, which is what makes "sigmas" mean the usual thing.
        let sigma = (1.4826 * devs[devs.len() / 2]).max(self.mad_floor);
        let limit = self.hampel_sigmas * sigma;

        let centre = f64::from(self.raw[self.raw.len() / 2]);
        let suspects = sorted.iter().filter(|v| (*v - median).abs() > limit).count();
        if (centre - median).abs() > limit && suspects == 1 {
            Some(median)
        } else {
            Some(centre)
        }
    }

    /// Accumulate ambient statistics, and freeze them once the link has been
    /// watched for long enough.
    ///
    /// Both conditions matter: the elapsed time bounds how much of the room's
    /// ordinary behaviour was seen, and the sample count stops a link that
    /// reports once a minute from freezing a baseline built from two readings.
    fn learn(&mut self, value: f64, now_secs: f64) {
        self.ambient_n += 1;
        self.ambient_sum += value;
        self.ambient_sumsq += value * value;

        let elapsed = now_secs - self.start_at.unwrap_or(now_secs);
        if elapsed < self.baseline_secs || self.ambient_n < self.short_window() as u64 {
            return;
        }
        let n = self.ambient_n as f64;
        self.baseline_mean = self.ambient_sum / n;
        self.baseline_var = (self.ambient_sumsq / n - self.baseline_mean * self.baseline_mean).max(0.0);
        if self.baseline_mean > self.saturation_dbm {
            self.state = State::Saturated;
            info!(
                "motion: link at {:.1} dBm is saturated (> {:.0} dBm); not sensing it",
                self.baseline_mean, self.saturation_dbm
            );
            return;
        }
        self.state = State::Idle;
        debug!(
            "motion: baseline ready after {:.0}s — {:.1} dBm, var {:.2} dB²",
            elapsed, self.baseline_mean, self.baseline_var
        );
    }

    /// Score the short window and apply the hysteresis.
    fn evaluate(&mut self, value: f64, now_secs: f64) -> Option<Transition> {
        let n = self.short.len() as f64;
        let mean = self.short.iter().sum::<f64>() / n;
        let short_var = self.short.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / n;
        // `f64::EPSILON` rather than 0: a zero floor on a zero baseline is
        // 0/0, and NaN reaches the controller as a parameter value that no
        // comparison it makes is true for -- worse than a wrong number,
        // because it looks like a working sensor.
        self.score = short_var / self.baseline_var.max(self.var_floor).max(f64::EPSILON);

        match self.state {
            State::Learning | State::Saturated => None,
            State::Idle => {
                if self.score > self.on_ratio && short_var >= self.min_short_var {
                    self.above += 1;
                } else {
                    self.above = 0;
                }
                if self.above >= self.on_evals {
                    self.state = State::Motion;
                    self.above = 0;
                    self.below_since = None;
                    self.motion_count += 1;
                    self.last_motion_at = Some(now_secs);
                    return Some(Transition::ToMotion);
                }
                // The baseline learns only from samples that are not already
                // candidates for motion. Letting a burst teach the baseline is
                // how a detector goes deaf: the thing it is measuring becomes
                // the thing it measures against.
                if self.above == 0 {
                    self.adapt(value);
                }
                None
            }
            State::Motion => {
                if self.score >= self.off_ratio {
                    self.below_since = None;
                    return None;
                }
                let since = *self.below_since.get_or_insert(now_secs);
                if now_secs - since < self.dwell_secs {
                    return None;
                }
                self.state = State::Idle;
                self.below_since = None;
                self.above = 0;
                Some(Transition::ToIdle)
            }
        }
    }

    /// Follow a drifting link, slowly — its LEVEL, and only its level.
    ///
    /// A link fades over minutes for reasons that are not people: a door
    /// closed, a radio retuned, the weather. A frozen level turns any of those
    /// into a link that reports motion forever, so the mean has to move with
    /// it; `baseline_alpha` is small enough (1% per sample, a ~50 s time
    /// constant at the default period) that nothing a body does in a corridor
    /// survives it.
    ///
    /// The VARIANCE is deliberately not adapted, and that asymmetry is the
    /// point. Variance is the quantity being measured, so letting it follow
    /// the link means the detector calibrates itself against its own signal:
    /// every hour of ordinary ±1 dB noise raised the divisor, every raise cut
    /// the score, and the device grew quietly deafer the longer it stayed up,
    /// with nothing in a log or a parameter to show it. A link whose ambient
    /// variance has genuinely changed is a link that should be relearnt, not
    /// one that should be silently rescaled.
    fn adapt(&mut self, value: f64) {
        self.baseline_mean += self.baseline_alpha * (value - self.baseline_mean);
    }

    /// Effective short window, never zero. See [`hampel`](Self::hampel) for
    /// why the clamp lives at the point of use.
    fn short_window(&self) -> usize {
        self.short_window.max(1)
    }

    /// How much of the short window is filled; tests use it to tell a
    /// resync apart from an ordinary sample.
    #[cfg(test)]
    fn short_len(&self) -> usize {
        self.short.len()
    }

    /// Forget the sample history, keep what was learnt.
    ///
    /// Called when a peer reappears after a gap. The samples either side of a
    /// gap are minutes apart, and joining them puts a step the size of the
    /// whole level change inside the short window -- which is precisely the
    /// shape motion has. A dozing phone that comes back on a different path
    /// would read as somebody walking past, most reliably at night when
    /// phones sleep. The baseline survives because it is still true: the room
    /// did not change while nobody was reporting from it.
    fn resync(&mut self) {
        self.raw.clear();
        self.short.clear();
        self.above = 0;
        self.below_since = None;
    }
}

// ── The live table ───────────────────────────────────────────────────────────

/// Which side of the radio a link is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    /// An 802.11s peer. Both ends are ours, both are stationary, and the path
    /// between them is usually the one a person actually crosses.
    Mesh,
    /// An associated station. It moves with its owner, so its variance mixes
    /// the room with the phone in a pocket.
    Client,
}

impl LinkKind {
    fn as_str(self) -> &'static str {
        match self {
            LinkKind::Mesh => "mesh",
            LinkKind::Client => "client",
        }
    }
}

/// One tracked (interface, peer) pair.
struct Link {
    /// Data-model instance number. Assigned once and kept for the life of the
    /// link: a controller that has been told `Motion.4` is in Motion must not
    /// find `Motion.4` meaning a different peer the next time it asks.
    instance: u32,
    iface: String,
    peer: String,
    kind: LinkKind,
    detector: LinkDetector,
    /// Sampler clock reading of the last sample, for ageing the link out.
    last_seen: f64,
    /// Wall-clock time of the last Motion, already formatted. The detector
    /// keeps the same instant on the sampler's monotonic clock, which is the
    /// right clock for dwell arithmetic and useless to a controller.
    last_motion_at: Option<String>,
}

/// Origin of the sampler's clock.
///
/// Beside the table on purpose: the two have to share a lifetime. The table
/// is a `static` that survives any one run of the USP agent, and the agent is
/// restarted on every connection failure. A clock created per run puts `now`
/// back near zero each time, and every timestamp already in the table is then
/// in the future: nothing ages out (`now - last_seen` is always tiny), and a
/// link that began learning before the restart has a negative elapsed time and
/// stays in Learning for as long as the device is up. Both are silent.
static CLOCK: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

/// The shared clock origin. Identical on every call, for the life of the
/// process.
fn clock_origin() -> Instant {
    *CLOCK.get_or_init(Instant::now)
}

/// Seconds since the sampler clock started.
fn now_secs() -> f64 {
    clock_origin().elapsed().as_secs_f64()
}

/// Every link currently tracked.
///
/// A `static` because the sampler task writes it and the data-model GET reads
/// it, and those two have no other place to meet: `dm::get` is called from the
/// USP message loop with nothing but a `&ClientConfig`.
static LINKS: Mutex<Option<Vec<Link>>> = Mutex::new(None);

/// Next free instance number. Never reused while a link lives; a number whose
/// link has aged out is not handed back out either, because a controller may
/// still be holding the old meaning.
static NEXT_INSTANCE: AtomicU32 = AtomicU32::new(1);

/// Take the table, surviving a poisoned lock.
///
/// A panic elsewhere while the table was held must not turn every later GET
/// into a panic of its own: the worst case is a half-updated detector, which
/// self-corrects within a window.
fn links() -> std::sync::MutexGuard<'static, Option<Vec<Link>>> {
    LINKS.lock().unwrap_or_else(|e| e.into_inner())
}

/// How long a peer may be missing from the dumps before its detector is
/// dropped. Long enough to survive a client that dozes between beacons, short
/// enough that a table on a busy AP does not grow all day.
const STALE_SECS: f64 = 60.0;

/// How often the interface list is rebuilt. Interfaces appear when a radio is
/// reconfigured, which is rare; re-parsing `iw dev` every tick would fork twice
/// a second for an answer that changes once a week.
const REDISCOVER: Duration = Duration::from_secs(30);

/// How soon to retry after a discovery that found nothing. See
/// [`merge_discovery`].
const REDISCOVER_RETRY: Duration = Duration::from_secs(5);

/// Least often a repeated per-interface failure is logged. A wedged radio
/// fails every tick; at the default period that would be two lines a second
/// into a router's syslog, which is how a real fault becomes invisible.
const WARN_EVERY: Duration = Duration::from_secs(60);

// ── Parsing ──────────────────────────────────────────────────────────────────

/// AP interfaces, in the order `iw dev` lists them.
///
/// The real ifname is what `iw ... station dump` needs, and UCI does not have
/// it: `ifname` is usually empty there because netifd assigns the name. Only
/// `type AP` blocks are taken — `type AP/VLAN`, `managed` and `mesh point`
/// blocks are other things, and the mesh point in particular is sampled
/// separately so matching it here would dump it twice.
fn parse_ap_ifaces(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix("Interface ") {
            current = Some(name.trim().to_owned());
        } else if line == "type AP" {
            if let Some(name) = current.take() {
                found.push(name);
            }
        }
    }
    found
}

/// Instantaneous signal per peer from one `iw ... station dump`.
///
/// `signal`, never `signal avg`: the average is exactly the quantity that
/// hides what this module looks for. A body crossing the path moves the
/// instantaneous reading several dB for a few seconds, which an 8-sample
/// hardware average flattens into half a dB of nothing.
///
/// Mesh peers are gated on `mesh plink: ESTAB` because a peer can sit in
/// OPN_SNT with a plausible signal and no link at all. Client stations have no
/// such line and need none — a station in the dump is associated.
fn signals(dump: &str, kind: LinkKind) -> Vec<(String, i32)> {
    crate::usp::dm::wifi::parse_station_dump(dump)
        .into_iter()
        .filter(|sta| match kind {
            LinkKind::Mesh => sta
                .get("mesh_plink")
                .is_some_and(|p| p.contains("ESTAB")),
            LinkKind::Client => true,
        })
        .filter_map(|sta| {
            let mac = sta.get("mac")?.clone();
            let dbm = sta.get("signal")?.parse::<i32>().ok()?;
            Some((mac, dbm))
        })
        .collect()
}

// ── The sampler ──────────────────────────────────────────────────────────────

/// Is a sampler already running in this process?
///
/// One sampler, ever. `usp::agent::run` is called again by `main` after every
/// connection failure -- and returns immediately on some configurations, such
/// as `mtp=both` with no `ws_url` -- so a `spawn` that only checked the
/// channel would leave one live sampler per reconnect, each with its own view
/// of a shared table, all forking `iw` on the same interfaces.
///
/// The channel is not a usable liveness signal either: under MQTT the
/// heartbeat task holds the receiver in an endless loop, so `is_closed` never
/// becomes true and a stale sampler would run until the device rebooted.
static RUNNING: AtomicBool = AtomicBool::new(false);

/// Releases [`RUNNING`] however the sampler task ends, panic included. A flag
/// left set is not a stuck task, it is motion sensing silently off until the
/// next reboot.
struct RunningGuard;

impl Drop for RunningGuard {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::Release);
    }
}

/// Start the motion sampler, if the operator has consented to it.
///
/// Called from the USP agent rather than from `main`, unlike `ndpid::spawn`,
/// for one reason: the only way to send a Notify is the `StatusSender` the
/// agent creates when it builds its MTP channel, and `main` has no handle on
/// it.
///
/// Returns the sampler's stop handle, which the caller must KEEP: dropping it
/// is what retires the task. `agent::run` holds it for the length of its own
/// body, so a sampler outlives its agent by at most one sampling period, and
/// the next `run` finds the flag clear. Returns `None` when sensing is off or
/// a sampler is already running, in both cases having said so.
#[must_use = "dropping the stop handle immediately retires the sampler"]
pub fn spawn(
    cfg: Arc<ClientConfig>,
    tx: StatusSender,
    agent_id: EndpointId,
) -> Option<tokio::sync::watch::Sender<()>> {
    if !cfg.motion_enabled {
        // Said once, at info, because silence here is indistinguishable from a
        // sampler that is running and has never seen anybody.
        info!(
            "motion: sensing is off (presence in a home is personal data and must be \
             consented); enable with: uci set optimacs.agent.motion_enabled='1' && \
             uci commit optimacs && /etc/init.d/ac-client restart"
        );
        return None;
    }
    if RUNNING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        // Reachable only if an agent restart beat the previous sampler's next
        // tick. `main` waits 30s between runs and the period is at most 5s, so
        // this means something is genuinely wedged; say so rather than
        // starting a second one on top of it.
        warn!("motion: a sampler is already running, not starting another");
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
    let period = Duration::from_millis(cfg.motion_period_ms);
    let period_secs = period.as_secs_f64();
    // Twice the period: one tick's worth of slack for a busy radio, and still
    // short enough that the cancellation check below is reached promptly.
    let dump_budget = period * 2;
    let mut ifaces: Vec<(String, LinkKind)> = Vec::new();
    let mut next_discovery = Instant::now();
    let mut warned: HashMap<String, Instant> = HashMap::new();
    let mut last_tick: Option<f64> = None;

    info!(
        "motion: sampling RSSI every {} ms; reporting state changes only",
        cfg.motion_period_ms
    );

    loop {
        tokio::time::sleep(period).await;

        // The stop handle belongs to `agent::run` and is dropped when it
        // returns. Checked rather than awaited so a tick already in flight
        // finishes cleanly; the cost of that is at most one period of a
        // sampler whose agent has gone.
        if stop.has_changed().is_err() {
            info!("motion: agent stopped, sampler retiring");
            return;
        }

        if Instant::now() >= next_discovery {
            let found = discover_ifaces().await;
            let (list, retry) = merge_discovery(std::mem::take(&mut ifaces), found);
            ifaces = list;
            next_discovery = Instant::now() + retry;
            debug!("motion: sampling {} interface(s)", ifaces.len());
        }

        let now = now_secs();
        // Twice the longer of the nominal period and the previous tick's real
        // duration, so a slow tick (a timing-out `iw`, a busy box) is not
        // mistaken for a peer that went away. See the gap check in `apply`.
        let gap_secs = 2.0 * period_secs.max(now - last_tick.unwrap_or(now));
        last_tick = Some(now);
        let mut samples: Vec<(&str, LinkKind, String, i32)> = Vec::new();
        for (iface, kind) in &ifaces {
            // One fork per interface per tick, and the one output is parsed
            // for both mesh peers and client stations.
            let dump = station_dump(iface, dump_budget, &mut warned).await;
            for (peer, dbm) in signals(&dump, *kind) {
                samples.push((iface.as_str(), *kind, peer, dbm));
            }
        }

        let changes = {
            let mut guard = links();
            let table = guard.get_or_insert_with(Vec::new);
            apply(table, &samples, now, gap_secs)
        };
        for (instance, iface, peer, state) in changes {
            info!("motion: {iface} {peer} -> {state}");
            notify(&tx, &agent_id, &cfg.controller_id, instance, state);
        }
    }
}

/// The interfaces worth sampling: the mesh point, and every AP.
///
/// `iw` is forked from a blocking thread because both helpers are synchronous
/// — `discover_mesh_iface` is shared with `dm::mesh`, and duplicating its
/// parser here to make it async would mean two spellings of the same thing
/// drifting apart. Twice per thirty seconds is not worth that.
async fn discover_ifaces() -> Vec<(String, LinkKind)> {
    tokio::task::spawn_blocking(|| {
        let mut out: Vec<(String, LinkKind)> = Vec::new();
        if let Some(mesh) = super::mesh::discover_mesh_iface() {
            out.push((mesh, LinkKind::Mesh));
        }
        let dev = std::process::Command::new("iw")
            .arg("dev")
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        out.extend(
            parse_ap_ifaces(&dev)
                .into_iter()
                .map(|i| (i, LinkKind::Client)),
        );
        out
    })
    .await
    .unwrap_or_default()
}

/// One `iw dev <iface> station dump`, as text, or nothing within `budget`.
///
/// Every failure — `iw` missing, the interface down, non-UTF-8 output — is an
/// empty string, which reads downstream as "this interface had no peers this
/// tick". That is the honest answer and it costs nothing: a link that is
/// really there reappears on the next tick, and one that is not ages out.
///
/// The timeout is not defensive dressing. `iw` talks to nl80211, and a wedged
/// ath11k firmware does not answer: the process never returns, this loop is
/// sequential, and one bad interface would stop the sampler sampling anything
/// at all -- forever, past the cancellation check, with a single debug line to
/// show for it. `kill_on_drop` matters as much as the timeout: without it the
/// timed-out `iw` stays a zombie and the next tick forks another.
async fn station_dump(
    iface: &str,
    budget: Duration,
    warned: &mut HashMap<String, Instant>,
) -> String {
    let mut cmd = tokio::process::Command::new("iw");
    cmd.args(["dev", iface, "station", "dump"]).kill_on_drop(true);

    match tokio::time::timeout(budget, cmd.output()).await {
        Ok(Ok(o)) => String::from_utf8_lossy(&o.stdout).into_owned(),
        Ok(Err(e)) => {
            warn_occasionally(warned, iface, &format!("cannot run iw on {iface}: {e}"));
            String::new()
        }
        Err(_) => {
            warn_occasionally(
                warned,
                iface,
                &format!(
                    "iw dev {iface} station dump did not answer within {:.1}s; \
                     the radio may be wedged",
                    budget.as_secs_f64()
                ),
            );
            String::new()
        }
    }
}

/// Log `msg` at most once per [`WARN_EVERY`] per interface.
///
/// A wedged radio fails on every tick. Logging each one buries the first
/// occurrence -- the only one that says when it started -- under thousands of
/// copies, on a device whose whole log is a ring buffer in RAM.
fn warn_occasionally(warned: &mut HashMap<String, Instant>, iface: &str, msg: &str) {
    let now = Instant::now();
    let due = warned
        .get(iface)
        .is_none_or(|last| now.duration_since(*last) >= WARN_EVERY);
    if due {
        warn!("motion: {msg}");
        warned.insert(iface.to_owned(), now);
    }
}

/// Feed this tick's samples into `table` and return the state changes.
///
/// Takes the table rather than reaching for the `static` so that the lock is
/// provably not held across an `await` or a fork -- everything that blocks
/// happens before this is called and the Notify sends happen after it returns
/// -- and so the whole lifecycle is testable without a radio or a lock.
fn apply(
    table: &mut Vec<Link>,
    samples: &[(&str, LinkKind, String, i32)],
    now: f64,
    gap_secs: f64,
) -> Vec<(u32, String, String, &'static str)> {
    let mut changes = Vec::new();

    for (iface, kind, peer, dbm) in samples {
        let idx = match table
            .iter()
            .position(|l| l.iface == *iface && l.peer == *peer)
        {
            Some(i) => i,
            None => {
                table.push(Link {
                    instance: NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed),
                    iface: (*iface).to_owned(),
                    peer: peer.clone(),
                    kind: *kind,
                    detector: LinkDetector::default(),
                    last_seen: now,
                    last_motion_at: None,
                });
                table.len() - 1
            }
        };
        let link = &mut table[idx];
        // A peer that missed ticks has a hole in its history. Joining the two
        // sides of it would fabricate a step; see `LinkDetector::resync`.
        //
        // `gap_secs` is the caller's idea of "missed a tick", and it must be
        // measured against how long ticks are actually taking, not the
        // nominal period: one wedged radio stretches every tick by its dump
        // budget, and judged against the nominal period every healthy link
        // would then look absent, resync every tick, and never fill a window
        // again. That is the failure the dump budget exists to prevent.
        if now - link.last_seen > gap_secs {
            debug!(
                "motion: {} {} back after {:.1}s, resyncing",
                link.iface,
                link.peer,
                now - link.last_seen
            );
            link.detector.resync();
        }
        link.last_seen = now;
        match link.detector.push(*dbm, now) {
            Some(Transition::ToMotion) => {
                link.last_motion_at = Some(chrono::Utc::now().to_rfc3339());
                changes.push((link.instance, link.iface.clone(), link.peer.clone(), "Motion"));
            }
            Some(Transition::ToIdle) => {
                changes.push((link.instance, link.iface.clone(), link.peer.clone(), "Idle"));
            }
            None => {}
        }
    }

    // A link that is dropped mid-Motion has to be retracted first.
    //
    // The controller's last word on that instance would otherwise be
    // "Motion", and the instance is about to disappear from the GET tree, so
    // there would be nothing left to correct it with -- a phone that left the
    // house this morning would read as somebody moving in it tonight.
    for link in table.iter() {
        if now - link.last_seen > STALE_SECS && link.detector.state() == State::Motion {
            changes.push((link.instance, link.iface.clone(), link.peer.clone(), "Idle"));
        }
    }

    // A peer that stopped being reported takes its baseline with it. Keeping
    // it would mean a phone that left the house this morning is still
    // compared against the ambient of a room it is no longer in.
    table.retain(|l| now - l.last_seen <= STALE_SECS);
    changes
}

/// Choose the interface list to sample after a discovery attempt, and when to
/// try again.
///
/// An empty result keeps the previous list. Discovery is a fork of `iw`, and a
/// fork fails for reasons that have nothing to do with the radios -- a
/// momentary fork failure under memory pressure, `iw` losing a race with
/// netifd reloading an interface. Clearing the list on one of those stops all
/// sampling until the next refresh, and two in a row age out every link in the
/// table, taking their baselines and putting the device back into Learning for
/// a minute. The retry is sooner than a successful refresh so a device that
/// genuinely has no radios yet (boot, a reloading radio) picks them up quickly
/// without forking `iw` on every tick.
fn merge_discovery(
    previous: Vec<(String, LinkKind)>,
    found: Vec<(String, LinkKind)>,
) -> (Vec<(String, LinkKind)>, Duration) {
    if found.is_empty() {
        (previous, REDISCOVER_RETRY)
    } else {
        (found, REDISCOVER)
    }
}

/// Send one ValueChange Notify for a link's new state.
///
/// The same encode / record / send path the status heartbeat uses, down to the
/// `status` subscription id: the controller already routes that subscription,
/// and inventing a second one here would produce Notifies it has no
/// subscription for.
///
/// Offered, never waited for. The channel holds ten records and is shared with
/// the heartbeat, so a stalled TCP connection fills it; blocking on `send`
/// there would stop the sampler mid-tick, and the links it had not reached
/// would miss samples and resync on the way back. A dropped notification costs
/// the controller the edge, not the answer -- `...Motion.{i}.State` still
/// reads correctly on the very next GET.
fn notify(
    tx: &StatusSender,
    agent_id: &EndpointId,
    controller_id: &str,
    instance: u32,
    state: &str,
) {
    let path = format!("Device.X_OptimACS_Sensing.Motion.{instance}.State");
    let msg = build_value_change_notify("status", &path, state);
    let msg_bytes = match encode_msg(&msg) {
        Ok(b) => b,
        Err(e) => {
            warn!("motion: cannot encode notify: {e}");
            return;
        }
    };
    let rec = record::no_session_record(agent_id.as_str(), controller_id, msg_bytes, "1.3");
    match record::encode_record(&rec) {
        Ok(bytes) => match tx.try_send(bytes) {
            Ok(()) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => warn!(
                "motion: status channel full, dropped {path} = {state}; \
                 the current state is still readable with a GET"
            ),
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                warn!("motion: status channel closed, dropped {path} = {state}")
            }
        },
        Err(e) => warn!("motion: cannot encode record: {e}"),
    }
}

// ── The data model ───────────────────────────────────────────────────────────

/// Report the motion sub-tree.
///
/// Routed ahead of `dm::sensing` in the dispatch and never through it: that
/// module deletes its spool as it reports, so answering a `.Motion` GET there
/// would silently consume a batch of firewall observations.
pub fn get(cfg: &ClientConfig, path: &str) -> HashMap<String, String> {
    if !path.starts_with("Device.X_OptimACS_Sensing") {
        return HashMap::new();
    }
    let guard = links();
    let table = guard.as_deref().unwrap_or(&[]);
    render(table, cfg.motion_enabled)
}

/// Render the table as data-model parameters.
///
/// Split from [`get`] so the shape the controller sees is testable without a
/// radio, a lock, or a process.
fn render(table: &[Link], enabled: bool) -> HashMap<String, String> {
    let mut m = HashMap::new();
    // A sibling of the table, not a member of it: `Motion.{i}.` is a
    // multi-instance object, and `Motion.Enable` would be a scalar wearing an
    // instance prefix -- a shape no controller can walk.
    //
    // Read-only by design, and the only parameter here that could plausibly
    // have been writable. This switch is consent, and consent a controller can
    // assert remotely, over a session it authenticated itself, is not consent.
    // It lives in UCI, where switching on presence detection in somebody's
    // home takes a person with an account on the device.
    m.insert(
        "Device.X_OptimACS_Sensing.MotionEnable".into(),
        if enabled { "1" } else { "0" }.into(),
    );
    m.insert(
        "Device.X_OptimACS_Sensing.MotionNumberOfEntries".into(),
        table.len().to_string(),
    );

    for link in table {
        let base = format!("Device.X_OptimACS_Sensing.Motion.{}", link.instance);
        let d = &link.detector;
        m.insert(format!("{base}.Interface"), link.iface.clone());
        m.insert(format!("{base}.Peer"), link.peer.clone());
        m.insert(format!("{base}.Kind"), link.kind.as_str().into());
        m.insert(format!("{base}.State"), d.state().as_str().into());
        m.insert(format!("{base}.Score"), format!("{:.2}", d.score()));
        // Empty while learning rather than 0: a baseline of 0 dBm is a reading
        // no radio ever produces, and the controller cannot tell an invented
        // number from a measured one.
        m.insert(
            format!("{base}.BaselineDbm"),
            d.baseline_dbm().map_or(String::new(), |b| format!("{b:.1}")),
        );
        m.insert(format!("{base}.MotionCount"), d.motion_count().to_string());
        m.insert(
            format!("{base}.LastMotionAt"),
            link.last_motion_at.clone().unwrap_or_default(),
        );
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sampling period used by the tests, in seconds — the shipped default
    /// (`motion_period_ms = 500`), so the sample counts below are the ones a
    /// real device would need.
    const DT: f64 = 0.5;
    /// What `run` passes as the gap threshold when ticks are on time.
    const GAP: f64 = 2.0 * DT;

    /// A detector with the learning phase and dwell shrunk so a test can reach
    /// `Idle` in twenty samples instead of a hundred and twenty. Every other
    /// constant is the shipped default: shrinking those would test a detector
    /// nobody runs.
    fn tuned() -> LinkDetector {
        LinkDetector {
            baseline_secs: 10.0,
            dwell_secs: 2.0,
            ..Default::default()
        }
    }

    /// A link with ±1 dB of jitter is the normal case, not motion. If this
    /// fires, every quiet house in the fleet reports someone walking around.
    #[test]
    fn a_flat_link_never_leaves_idle() {
        let mut d = tuned();
        let mut t = 0.0;
        let jitter = [0, 1, 0, -1, 1, 0, -1, 0];
        let mut fired = Vec::new();
        for i in 0..400 {
            if let Some(tr) = d.push(-50 + jitter[i % jitter.len()], t) {
                fired.push(tr);
            }
            t += DT;
        }
        assert_eq!(d.state(), State::Idle);
        assert!(fired.is_empty(), "flat link fired: {fired:?}");
        assert_eq!(d.motion_count(), 0);
    }

    /// The burst is what the whole module is for: a body between the two
    /// radios drops the link several dB for a sample or two at a time. And it
    /// must come back — a detector that latches into Motion reports an empty
    /// house as occupied forever.
    ///
    /// The burst is SPARSE on purpose — one deviating sample in three, not a
    /// square wave. That is what the link actually does on a quantised radio,
    /// and it is the case the Hampel stage is most likely to eat: each drop is
    /// large, the surrounding median is unmoved, and the MAD of a window that
    /// is mostly baseline is still zero. A filter that rejected them one by
    /// one would leave a flat stream and this would never fire.
    #[test]
    fn a_burst_flips_to_motion_and_returns_after_the_dwell() {
        let mut d = tuned();
        let mut t = 0.0;
        for _ in 0..40 {
            d.push(-50, t);
            t += DT;
        }
        assert_eq!(d.state(), State::Idle, "must finish learning on a flat link");

        let mut to_motion = false;
        for i in 0..30 {
            if d.push(if i % 3 == 2 { -58 } else { -50 }, t) == Some(Transition::ToMotion) {
                to_motion = true;
            }
            t += DT;
        }
        assert!(to_motion, "an 8 dB sparse burst must raise Motion");
        assert_eq!(d.state(), State::Motion);
        assert_eq!(d.motion_count(), 1);
        assert!(d.last_motion_at().is_some());

        let mut to_idle = false;
        for _ in 0..60 {
            if d.push(-50, t) == Some(Transition::ToIdle) {
                to_idle = true;
            }
            t += DT;
        }
        assert!(to_idle, "must return to Idle once the link flattens");
        assert_eq!(d.state(), State::Idle);
        assert_eq!(d.motion_count(), 1, "the return is not a second sighting");
    }

    /// The dwell, which is the difference between one event and a stutter.
    ///
    /// A person crossing a corridor stops being between the radios for a
    /// second or two in the middle of doing it. Without the dwell that single
    /// quiet stretch ends the event, and the controller gets Idle / Motion /
    /// Idle / Motion for what was one person walking through one doorway —
    /// which is not only noise on the wire, it is a worse answer.
    #[test]
    fn a_brief_quiet_moment_does_not_end_a_motion_event() {
        let mut d = LinkDetector {
            dwell_secs: 10.0,
            ..tuned()
        };
        let mut t = 0.0;
        for _ in 0..40 {
            d.push(-50, t);
            t += DT;
        }
        for i in 0..30 {
            d.push(if i % 3 == 2 { -58 } else { -50 }, t);
            t += DT;
        }
        assert_eq!(d.state(), State::Motion, "the burst must raise Motion first");

        // Long enough to flush the short window and start the dwell clock,
        // far short of the dwell itself.
        let mut ended = false;
        for _ in 0..14 {
            if d.push(-50, t) == Some(Transition::ToIdle) {
                ended = true;
            }
            t += DT;
        }
        assert!(!ended, "a quiet second ended the event");
        assert_eq!(d.state(), State::Motion);

        // Still there.
        for i in 0..8 {
            if d.push(if i % 2 == 0 { -58 } else { -50 }, t) == Some(Transition::ToIdle) {
                ended = true;
            }
            t += DT;
        }
        assert!(!ended, "the event ended while the link was still moving");
        assert_eq!(d.state(), State::Motion);

        // Gone.
        let mut ends = 0;
        for _ in 0..60 {
            if d.push(-50, t) == Some(Transition::ToIdle) {
                ends += 1;
            }
            t += DT;
        }
        assert_eq!(ends, 1, "one crossing must report exactly one end");
        assert_eq!(d.state(), State::Idle);
        assert_eq!(d.motion_count(), 1, "one crossing, one sighting");
    }

    /// The absolute variance gate, not just the ratio.
    ///
    /// With the shipped constants the two coincide — `on_ratio` × `var_floor`
    /// is exactly `min_short_var` — so the gate reads as redundant right up
    /// until somebody lowers the ratio to make the detector more sensitive.
    /// Then it is the only thing between the fleet and 1 dB of quantisation
    /// flicker: a link dithering between -50 and -51 has moved by the smallest
    /// amount the driver can express, and that is never a person.
    #[test]
    fn a_one_db_flicker_is_not_motion_however_sensitive_the_ratio() {
        let mut d = LinkDetector {
            on_ratio: 0.5,
            ..tuned()
        };
        let mut t = 0.0;
        for _ in 0..40 {
            d.push(-50, t);
            t += DT;
        }
        assert_eq!(d.state(), State::Idle);

        let mut fired = Vec::new();
        for i in 0..100 {
            if let Some(tr) = d.push(if i % 2 == 0 { -50 } else { -51 }, t) {
                fired.push(tr);
            }
            t += DT;
        }
        assert!(fired.is_empty(), "1 dB flicker fired: {fired:?}");
        assert!(
            d.score() > d.on_ratio,
            "the ratio alone would have fired; score {} vs ratio {}",
            d.score(),
            d.on_ratio
        );
    }

    /// One absurd sample is a driver artefact, not a person. Without the
    /// Hampel stage it lands in the short window and a single bad read from
    /// `iw` is indistinguishable from someone walking past.
    #[test]
    fn a_single_spike_never_flips() {
        let mut d = tuned();
        let mut t = 0.0;
        let mut fired = Vec::new();
        for i in 0..200 {
            let dbm = if i == 120 { -30 } else { -50 };
            if let Some(tr) = d.push(dbm, t) {
                fired.push(tr);
            }
            t += DT;
        }
        assert!(fired.is_empty(), "a lone spike fired: {fired:?}");
        assert_eq!(d.state(), State::Idle);
        assert_eq!(d.motion_count(), 0);
    }

    /// A link that fades over minutes — a door closed, a radio retuned,
    /// weather — must move the baseline with it. A frozen baseline turns a
    /// drifted link into a permanent false positive.
    #[test]
    fn a_slow_drift_rebaselines_without_firing() {
        let mut d = tuned();
        let mut t = 0.0;
        let mut first_baseline: Option<f64> = None;
        let mut fired = Vec::new();
        for i in 0..400 {
            // 0.05 dB per sample, quantised to whole dBm the way ath11k
            // reports it: a 1 dB step every twentieth sample.
            let dbm = (-70.0 + i as f64 * 0.05).round() as i32;
            if let Some(tr) = d.push(dbm, t) {
                fired.push(tr);
            }
            if first_baseline.is_none() {
                first_baseline = d.baseline_dbm();
            }
            t += DT;
        }
        assert!(fired.is_empty(), "a slow drift fired: {fired:?}");
        assert_eq!(d.state(), State::Idle);
        let start = first_baseline.expect("baseline is frozen once learning ends");
        let end = d.baseline_dbm().expect("baseline survives the drift");
        assert!(
            end - start > 10.0,
            "baseline did not follow the drift: {start} -> {end}"
        );
        assert!(end < -50.0, "baseline overshot the signal: {end}");
    }

    /// Before the ambient baseline exists there is nothing to compare against,
    /// so any reading is as plausible as any other. Reporting motion here
    /// would mean every reboot announces a presence.
    #[test]
    fn nothing_fires_during_learning() {
        let mut d = tuned();
        let mut t = 0.0;
        for i in 0..19 {
            let dbm = if i % 2 == 0 { -40 } else { -70 };
            assert!(d.push(dbm, t).is_none(), "fired while still learning");
            t += DT;
        }
        assert_eq!(d.state(), State::Learning);
        assert!(d.baseline_dbm().is_none());
    }

    /// A perfectly still link reports the same integer dBm forever, so its
    /// ambient variance is exactly zero. Without the variance floor the score
    /// is 0/0 and every link in the fleet reads NaN.
    #[test]
    fn a_perfectly_flat_integer_link_scores_zero() {
        let mut d = tuned();
        let mut t = 0.0;
        for _ in 0..100 {
            assert!(d.push(-60, t).is_none());
            t += DT;
        }
        assert_eq!(d.state(), State::Idle);
        assert!(d.score().abs() < 1e-12, "score was {}", d.score());
        assert_eq!(d.motion_count(), 0);
        assert!(d.last_motion_at().is_none());
    }

    /// `iw dev` names AP interfaces by phy, not by UCI section, and the mesh
    /// point in the same output must not be sampled twice.
    #[test]
    fn ap_interfaces_are_found_and_the_mesh_point_is_not_one() {
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
";
        assert_eq!(parse_ap_ifaces(dev), vec!["phy1-ap0", "phy0-ap0"]);
    }

    /// Mesh peers are gated on `mesh plink: ESTAB` and AP stations are not: a
    /// station is associated or it is not in the dump at all, while a mesh
    /// peer can sit in OPN_SNT with a signal reading and no link.
    #[test]
    fn only_established_mesh_peers_are_sampled_but_every_client_is() {
        let dump = "\
Station d6:f3:37:42:d3:cd (on phy1-mesh0)
\tsignal:  \t-45 [-45, -51] dBm
\tsignal avg:\t-70 dBm
\tmesh plink:\tESTAB
Station ae:5e:ca:cf:3f:1a (on phy1-mesh0)
\tsignal:  \t-60 dBm
\tmesh plink:\tOPN_SNT
";
        assert_eq!(
            signals(dump, LinkKind::Mesh),
            vec![("D6:F3:37:42:D3:CD".to_string(), -45)],
            "the instantaneous signal of the ESTAB peer only"
        );
        assert_eq!(
            signals(dump, LinkKind::Client).len(),
            2,
            "a client dump has no plink line to gate on"
        );
    }

    /// Malformed output must yield no samples rather than a panic: `iw` on a
    /// downed interface prints an error on stdout, and a panicking sampler
    /// takes the whole agent's task down with it.
    #[test]
    fn malformed_output_yields_nothing() {
        assert!(signals("command failed: No such device (-19)", LinkKind::Client).is_empty());
        assert!(signals("", LinkKind::Mesh).is_empty());
        assert!(signals("Station\n\tsignal:\n", LinkKind::Client).is_empty());
        assert!(parse_ap_ifaces("nonsense\n\ttype AP\n").is_empty());
    }

    /// The reported tree is what the controller actually sees. Instance
    /// numbers come from the link, not from iteration order, so a peer that
    /// ages out does not renumber the ones that stayed.
    #[test]
    fn the_reported_tree_carries_one_instance_per_link() {
        let mut a = tuned();
        let mut b = tuned();
        let mut t = 0.0;
        for _ in 0..100 {
            a.push(-55, t);
            b.push(-60, t);
            t += DT;
        }
        let links = vec![
            Link {
                instance: 4,
                iface: "phy1-mesh0".into(),
                peer: "D6:F3:37:42:D3:CD".into(),
                kind: LinkKind::Mesh,
                detector: a,
                last_seen: t,
                last_motion_at: Some("2026-09-21T10:00:00+00:00".into()),
            },
            Link {
                instance: 7,
                iface: "phy0-ap0".into(),
                peer: "AE:5E:CA:CF:3F:1A".into(),
                kind: LinkKind::Client,
                detector: b,
                last_seen: t,
                last_motion_at: None,
            },
        ];
        let m = render(&links, true);

        assert_eq!(
            m.get("Device.X_OptimACS_Sensing.MotionNumberOfEntries"),
            Some(&"2".to_string())
        );
        assert_eq!(
            m.get("Device.X_OptimACS_Sensing.MotionEnable"),
            Some(&"1".to_string())
        );
        assert_eq!(
            m.get("Device.X_OptimACS_Sensing.Motion.4.Interface"),
            Some(&"phy1-mesh0".to_string())
        );
        assert_eq!(
            m.get("Device.X_OptimACS_Sensing.Motion.4.Peer"),
            Some(&"D6:F3:37:42:D3:CD".to_string())
        );
        assert_eq!(
            m.get("Device.X_OptimACS_Sensing.Motion.4.Kind"),
            Some(&"mesh".to_string())
        );
        assert_eq!(
            m.get("Device.X_OptimACS_Sensing.Motion.7.Kind"),
            Some(&"client".to_string())
        );
        assert_eq!(
            m.get("Device.X_OptimACS_Sensing.Motion.7.State"),
            Some(&"Idle".to_string())
        );
        assert_eq!(
            m.get("Device.X_OptimACS_Sensing.Motion.7.Score"),
            Some(&"0.00".to_string())
        );
        assert_eq!(
            m.get("Device.X_OptimACS_Sensing.Motion.7.MotionCount"),
            Some(&"0".to_string())
        );
        assert_eq!(
            m.get("Device.X_OptimACS_Sensing.Motion.7.LastMotionAt"),
            Some(&String::new()),
            "never-seen must be empty, not a fabricated timestamp"
        );
        assert_eq!(
            m.get("Device.X_OptimACS_Sensing.Motion.4.LastMotionAt"),
            Some(&"2026-09-21T10:00:00+00:00".to_string())
        );
        assert_eq!(
            m.get("Device.X_OptimACS_Sensing.Motion.4.BaselineDbm"),
            Some(&"-55.0".to_string())
        );
    }

    // ── Lifecycle ────────────────────────────────────────────────────────────

    /// The live table outlives any one run of the USP agent, so the clock its
    /// timestamps are on has to as well.
    ///
    /// With a per-run clock every restart puts `now` back near zero: a link
    /// last seen at 4000 is then always "0.1 seconds old" and never ages out,
    /// while a link that started learning at 4000 has a NEGATIVE elapsed time
    /// and stays in Learning for as long as the device is up. Both failures
    /// are silent and neither shows up in a single-run test.
    #[test]
    fn the_table_and_the_clock_share_one_lifetime() {
        assert_eq!(
            clock_origin(),
            clock_origin(),
            "the sampler clock must have one origin per process, not per run"
        );

        let mac = "D6:F3:37:42:D3:CD".to_string();
        let mut table = Vec::new();

        // A run that saw the peer, a long way into the device's uptime.
        apply(
            &mut table,
            &[("phy0-ap0", LinkKind::Client, mac.clone(), -50)],
            4000.0,
            GAP,
        );
        assert_eq!(table.len(), 1);

        // A later run, on the same clock: the peer is long gone.
        let changes = apply(&mut table, &[], 4000.0 + STALE_SECS + 1.0, GAP);
        assert!(table.is_empty(), "a stale link must still age out after a restart");
        assert!(
            changes.is_empty(),
            "a link that never left Learning has no state to correct"
        );

        // And a link first seen after the restart must still be able to learn.
        let mut t = 5000.0;
        for _ in 0..300 {
            apply(
                &mut table,
                &[("phy0-ap0", LinkKind::Client, mac.clone(), -50)],
                t,
                GAP,
            );
            t += DT;
        }
        assert_eq!(table[0].detector.state(), State::Idle);
    }

    /// A peer that vanishes while its detector says Motion must be corrected
    /// before it is dropped.
    ///
    /// Otherwise the controller's last word on that instance is "Motion", and
    /// the instance is gone from the GET tree, so there is nothing left to
    /// correct it with. A phone that leaves the house would read as somebody
    /// moving in it, forever.
    #[test]
    fn an_evicted_link_that_was_in_motion_is_reported_idle_first() {
        let mac = "AE:5E:CA:CF:3F:1A".to_string();
        let mut table = Vec::new();
        let mut t = 0.0;
        let sample = |dbm: i32, mac: &String| {
            vec![("phy1-mesh0", LinkKind::Mesh, mac.clone(), dbm)]
        };

        for _ in 0..200 {
            apply(&mut table, &sample(-50, &mac), t, GAP);
            t += DT;
        }
        assert_eq!(table[0].detector.state(), State::Idle);
        for i in 0..40 {
            apply(
                &mut table,
                &sample(if i % 3 == 2 { -58 } else { -50 }, &mac),
                t,
                GAP,
            );
            t += DT;
        }
        assert_eq!(table[0].detector.state(), State::Motion);
        let instance = table[0].instance;

        let changes = apply(&mut table, &[], t + STALE_SECS + 1.0, GAP);
        assert!(table.is_empty(), "the stale link must be dropped");
        assert_eq!(
            changes,
            vec![(
                instance,
                "phy1-mesh0".to_string(),
                mac,
                "Idle"
            )],
            "an evicted link in Motion must be retracted, on its own instance"
        );
    }

    /// A phone that dozes for half a minute and comes back at a different
    /// level is not a person walking past.
    ///
    /// The gap puts an 8 dB step inside the short window, which is exactly
    /// the shape motion has. It fires on the fifth sample back with a score
    /// in the forties — a false positive that arrives precisely when a house
    /// is quiet enough for phones to sleep, which is to say at night.
    #[test]
    fn a_sampling_gap_does_not_look_like_motion() {
        let mac = "D6:F3:37:42:D3:CD".to_string();
        let mut table = Vec::new();
        let mut t = 0.0;
        for _ in 0..200 {
            apply(
                &mut table,
                &[("phy0-ap0", LinkKind::Client, mac.clone(), -50)],
                t,
                GAP,
            );
            t += DT;
        }
        assert_eq!(table[0].detector.state(), State::Idle);
        let baseline = table[0].detector.baseline_dbm().unwrap();

        // Asleep for thirty seconds, back on a different path.
        t += 30.0;
        let mut changes = Vec::new();
        for _ in 0..40 {
            changes.extend(apply(
                &mut table,
                &[("phy0-ap0", LinkKind::Client, mac.clone(), -58)],
                t,
                GAP,
            ));
            t += DT;
        }
        assert!(changes.is_empty(), "the gap fired: {changes:?}");
        assert_eq!(table[0].detector.state(), State::Idle);

        // The baseline is kept and left to creep toward the new level, not
        // thrown away: a relearn would sit on -58 within a sample or two and
        // cost the link a minute of Learning, when nothing about the room
        // changed while nobody was reporting from it.
        let after = table[0].detector.baseline_dbm().unwrap();
        assert!(
            after < baseline,
            "the baseline did not follow the new level at all: {baseline} -> {after}"
        );
        assert!(
            baseline - after < 4.0,
            "the baseline was relearnt rather than adapted: {baseline} -> {after}"
        );
    }

    /// The ambient variance is frozen once learnt, and stays frozen.
    ///
    /// When it followed the link as well, every hour of ordinary ±1 dB noise
    /// raised it, and raising the divisor lowers every future score: the
    /// detector grew steadily deafer the longer it ran, without a log line or
    /// a parameter to see it in. The two scores compared here are taken at
    /// the same phase of the same repeating jitter, so with a frozen variance
    /// they are the same number to the bit.
    #[test]
    fn the_baseline_variance_does_not_creep() {
        let mut d = tuned();
        let mut t = 0.0;
        let jitter = [0, 1, 0, -1, 1, 0, -1, 0];
        let mut early = 0.0;
        for i in 0..7200 {
            d.push(-50 + jitter[i % jitter.len()], t);
            t += DT;
            if i == 199 {
                early = d.score();
            }
        }
        assert!(early > 0.0, "the early score was never taken");
        assert!(
            (d.score() - early).abs() < 1e-9,
            "sensitivity drifted over an hour: {early} -> {}",
            d.score()
        );
        assert_eq!(d.state(), State::Idle);
    }

    /// Tuning fields are public, so a zero can reach the detector. It must
    /// clamp rather than index an empty ring or divide by zero: a panic takes
    /// the sampler task down, and a NaN reaches the controller as a parameter
    /// value that no comparison it makes is true for.
    #[test]
    fn degenerate_tuning_neither_panics_nor_poisons_the_data_model() {
        let mut d = LinkDetector {
            hampel_window: 0,
            short_window: 0,
            var_floor: 0.0,
            baseline_secs: 1.0,
            ..Default::default()
        };
        let mut t = 0.0;
        for i in 0..200 {
            d.push(-50 + i % 5, t);
            t += DT;
        }
        assert!(d.score().is_finite(), "score was {}", d.score());
        assert!(d.baseline_dbm().is_none_or(|b| b.is_finite()));

        let m = render(
            &[Link {
                instance: 1,
                iface: "phy0-ap0".into(),
                peer: "D6:F3:37:42:D3:CD".into(),
                kind: LinkKind::Client,
                detector: d,
                last_seen: t,
                last_motion_at: None,
            }],
            true,
        );
        assert!(
            !m.values().any(|v| v.contains("NaN") || v.contains("inf")),
            "a non-number reached the data model: {m:?}"
        );
    }

    /// One failed `iw dev` must not blank the interface list.
    ///
    /// Discovery is refreshed every thirty seconds, so a single transient
    /// failure that cleared the list would stop all sampling for thirty
    /// seconds, and two in a row would age out every link in the table —
    /// taking their baselines with them and putting the whole device back
    /// into Learning for a minute.
    #[test]
    fn a_failed_discovery_keeps_the_interfaces_it_had() {
        let had = vec![("phy0-ap0".to_string(), LinkKind::Client)];

        let (kept, retry) = merge_discovery(had.clone(), Vec::new());
        assert_eq!(kept, had, "a failed discovery discarded the interfaces");
        assert!(
            retry < REDISCOVER,
            "a failed discovery must be retried sooner than a successful one"
        );

        let found = vec![("phy1-mesh0".to_string(), LinkKind::Mesh)];
        let (fresh, next) = merge_discovery(had, found.clone());
        assert_eq!(fresh, found);
        assert_eq!(next, REDISCOVER);
    }

    /// One sampler per process, whatever the agent does.
    ///
    /// `agent::run` is re-entered after every connection failure, so `spawn`
    /// is called again and again over a device's uptime. Each extra sampler
    /// would fork `iw` on the same interfaces and push the same peers into the
    /// same shared table twice per tick, which reads as a link sampled at
    /// double rate — and nothing in the data model would show it.
    ///
    /// The task itself never wakes here: its first act is to sleep a period,
    /// and the test runtime is gone before then. That is deliberate, so the
    /// test forks nothing.
    #[tokio::test]
    async fn a_second_spawn_is_refused_and_the_slot_is_released() {
        let (tx, _rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
        let id = EndpointId::new("os::00005A::test");

        let off = Arc::new(ClientConfig::default());
        assert!(
            spawn(Arc::clone(&off), tx.clone(), id.clone()).is_none(),
            "sensing is off by default and must not start"
        );
        assert!(
            !RUNNING.load(Ordering::Acquire),
            "a refusal must not claim the slot"
        );

        let on = Arc::new(ClientConfig {
            motion_enabled: true,
            ..ClientConfig::default()
        });
        let first = spawn(Arc::clone(&on), tx.clone(), id.clone());
        assert!(first.is_some(), "the first sampler must start");
        assert!(
            spawn(Arc::clone(&on), tx.clone(), id).is_none(),
            "a second sampler must be refused while the first holds the slot"
        );

        // The slot is released by the task's guard, however it ends.
        drop(first);
        {
            let _guard = RunningGuard;
        }
        assert!(
            !RUNNING.load(Ordering::Acquire),
            "the slot must be released or sensing is off until the next reboot"
        );
    }

    /// A stalled connection must cost the notification, not the sampler.
    ///
    /// The status channel holds ten records and is shared with the heartbeat,
    /// so a stalled TCP connection fills it. Waiting there would stop the
    /// sampler mid-tick — the links it had not reached yet would miss samples,
    /// resync, and lose their short windows, so a stalled uplink would degrade
    /// the sensing itself rather than just its reporting.
    #[test]
    fn a_full_status_channel_drops_the_notification_instead_of_blocking() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
        tx.try_send(vec![0xAA]).expect("the channel starts empty");

        let id = EndpointId::new("os::00005A::test");
        notify(&tx, &id, "controller", 4, "Motion");

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

    /// A wedged radio fails on every tick. Logging each failure buries the
    /// first one — the only one that says when it started — under thousands of
    /// copies, in a log that is a ring buffer in RAM.
    #[test]
    fn a_repeated_interface_failure_is_logged_once_not_twice_a_second() {
        let mut warned = HashMap::new();
        warn_occasionally(&mut warned, "phy0-ap0", "wedged");
        let first = *warned.get("phy0-ap0").expect("the first failure is logged");

        warn_occasionally(&mut warned, "phy0-ap0", "wedged");
        assert_eq!(
            *warned.get("phy0-ap0").unwrap(),
            first,
            "the second failure within the window must be swallowed"
        );

        warn_occasionally(&mut warned, "phy1-ap0", "wedged");
        assert_eq!(
            warned.len(),
            2,
            "each interface gets its own window, or one noisy radio silences the rest"
        );
    }

    /// Consent is reported even with nothing to report: a controller must be
    /// able to tell "switched off" from "switched on and quiet".
    #[test]
    fn a_disabled_sensor_still_reports_its_switch() {
        let m = render(&[], false);
        assert_eq!(
            m.get("Device.X_OptimACS_Sensing.MotionEnable"),
            Some(&"0".to_string())
        );
        assert_eq!(
            m.get("Device.X_OptimACS_Sensing.MotionNumberOfEntries"),
            Some(&"0".to_string())
        );
    }

    #[test]
    fn a_slow_tick_does_not_resync_healthy_links() {
        // One wedged radio makes every tick take its whole dump budget. The
        // links on the other radios are still reporting every tick; judged
        // against the nominal period they would all look absent and be
        // resynced forever. `run` widens the gap to the real tick length.
        let mac = "D6:F3:37:42:D3:CD".to_string();
        let mut table = Vec::new();
        let mut t = 0.0;
        for _ in 0..200 {
            apply(
                &mut table,
                &[("phy0-ap0", LinkKind::Client, mac.clone(), -50)],
                t,
                GAP,
            );
            t += DT;
        }
        let full = table[0].detector.short_len();
        assert!(full > 1, "window never filled: {full}");

        // A tick that took five nominal periods, with the gap widened the
        // way `run` does it: twice the real tick.
        let slow = 5.0 * DT;
        t += slow;
        apply(
            &mut table,
            &[("phy0-ap0", LinkKind::Client, mac.clone(), -50)],
            t,
            2.0 * slow,
        );
        assert_eq!(
            table[0].detector.short_len(),
            full,
            "a slow tick was treated as a missing peer"
        );

        // And the same tick judged against the nominal gap does resync, which
        // is what makes the widening load-bearing rather than cosmetic.
        t += slow;
        apply(
            &mut table,
            &[("phy0-ap0", LinkKind::Client, mac.clone(), -50)],
            t,
            GAP,
        );
        // The Hampel ring empties too, so the short window cannot refill
        // until the ring has: the window is smaller, not merely restarted.
        assert!(
            table[0].detector.short_len() < full,
            "the nominal gap did not resync"
        );
    }

    #[test]
    fn a_saturated_link_is_named_and_never_fires() {
        // The -8 dBm link from the lab: flat, with the receiver's 7 dB gain
        // flicker clustered now and then. On a sensible link that cluster is
        // exactly what motion looks like, which is why it fired ten times in
        // eight minutes with nobody there.
        let mut d = LinkDetector::default();
        let mut t = 0.0;
        for _ in 0..200 {
            d.push(-8, t);
            t += DT;
        }
        assert_eq!(d.state(), State::Saturated);
        assert_eq!(d.baseline_dbm(), Some(-8.0));

        let mut fired = Vec::new();
        for i in 0..60 {
            let v = if (10..14).contains(&i) || (30..33).contains(&i) { -15 } else { -8 };
            fired.extend(d.push(v, t));
            t += DT;
        }
        assert!(fired.is_empty(), "a saturated link fired: {fired:?}");
        assert_eq!(d.state(), State::Saturated);
        assert_eq!(d.state().as_str(), "Saturated");

        // And the same shape on a link across a room is still motion: the
        // gate is about level, not about the pattern.
        let mut far = LinkDetector::default();
        let mut t = 0.0;
        for _ in 0..200 {
            far.push(-45, t);
            t += DT;
        }
        assert_eq!(far.state(), State::Idle);
        let mut fired = Vec::new();
        for i in 0..60 {
            let v = if (10..14).contains(&i) || (30..33).contains(&i) { -52 } else { -45 };
            fired.extend(far.push(v, t));
            t += DT;
        }
        assert!(fired.contains(&Transition::ToMotion));
    }
}
