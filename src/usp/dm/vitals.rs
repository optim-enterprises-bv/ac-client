//! Vital signs and fall candidates — the second stage on top of [`super::csi`].
//!
//! [`super::csi`] reduces every sounding to one number, a motion energy in dB,
//! and asks [`super::motion::LinkDetector`] whether it moved. That question is
//! answered over two seconds, because a body crossing a path changes the
//! channel in two seconds. This module asks two questions that are only
//! answerable over half a minute of a link that is NOT moving:
//!
//!   * is the small, periodic residual a chest? (breathing, then heart)
//!   * did a large transient end in somebody not getting up? (a fall)
//!
//! # These are candidates, not measurements
//!
//! A breathing rate out of this module is a periodicity in a radio channel that
//! is consistent with a chest. It is not a medical measurement, nothing here is
//! a medical device, and there is no way for the agent to know whether the
//! periodicity it found is a person, a fan, a fish-tank pump, or a neighbour's
//! washing machine on a shared wall. It needs a STILL subject inside the link's
//! own path, which a mesh link between two rooms may never have. The numbers
//! are reported so an integrator can look at them; nothing should act on one.
//!
//! # What a confidence means here
//!
//! Not a band's share of the power, which for a flat spectrum is just the
//! band's share of the width — 0.10 for breathing and 0.32 for the heart,
//! measured and predicted alike. It is the EXCESS over that, rescaled to 0..1,
//! so noise scores zero in both bands and one floor means one thing for both.
//! See [`band`].
//!
//! # Why magnitude AND phase
//!
//! A chest wall moves millimetres. At 5 GHz a millimetre is about 6 degrees of
//! carrier phase and almost nothing at all of received power, so the magnitude
//! series is the wrong place to look for a heartbeat and roughly the right
//! place to look for a breath. Both series are transformed and, per band, the
//! one with the higher in-band power share is used. That is one FFT more per
//! estimate and it is the difference between finding a heartbeat and not.
//!
//! # Cost
//!
//! Per record, off the tokio runtime, the reader thread already paid `tones x
//! chains` square roots for the magnitudes; this stage adds the same count of
//! `atan2` calls for the phase and two O(tones x chains) passes for the means
//! — 1024 of each on an 80 MHz four-chain link, at 9 records a second, so
//! ~9 k `atan2`/s per link and at the eight-capture cap ~74 k/s across the
//! device. Everything else is a push onto a ring.
//!
//! The transform is NOT per record. A 30 s window at 9 records a second is 270
//! samples, zero-padded to a 512-point FFT: 512/2 x 9 = 2304 butterflies, twice
//! (magnitude and phase), at most once every 2 s per link. That is ~2.3 k
//! butterflies a second per link against the ~9 k `atan2` the same link spends
//! on the per-record scalars — the spectral stage is the cheap half, which is
//! why it is allowed to be a whole extra FFT rather than a running estimator.

use std::collections::VecDeque;
use std::f64::consts::TAU;

use log::warn;

use super::csi::wrap_pi;

// ── The transform ────────────────────────────────────────────────────────────

/// In-place iterative radix-2 Cooley-Tukey FFT. `re.len()` must be a power of
/// two, and `im` the same length.
///
/// Written out rather than depended on. The agent ships to a 16 MB-flash router
/// and its dependency list is deliberately short; an FFT is fifty lines and one
/// test against the definition, which is a smaller commitment than a crate.
///
/// The twiddle factors are advanced by a complex rotation rather than recomputed
/// with `sin_cos` per butterfly: that is ~10x fewer transcendental calls, and
/// the rotation is restarted at every block so the error accumulates over at
/// most `len/2` <= 1024 steps — about 1e-13 relative, twelve orders below
/// anything the radio resolves. `the_transform_matches_a_direct_discrete_fourier_transform`
/// is what holds that claim up.
fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    debug_assert!(n.is_power_of_two(), "radix-2 needs a power-of-two length");
    debug_assert_eq!(n, im.len());
    if n < 2 {
        return;
    }

    // Bit-reversal permutation, counted rather than computed: `j` is `i` with
    // its bits reversed, advanced by the same carry propagation an odometer
    // uses, running from the top bit down.
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }

    let mut len = 2usize;
    while len <= n {
        let half = len / 2;
        let ang = -TAU / len as f64;
        let (step_i, step_r) = ang.sin_cos();
        let mut start = 0usize;
        while start < n {
            let (mut wr, mut wi) = (1.0f64, 0.0f64);
            for k in 0..half {
                let (a, b) = (start + k, start + k + half);
                let (ur, ui) = (re[a], im[a]);
                let vr = re[b] * wr - im[b] * wi;
                let vi = re[b] * wi + im[b] * wr;
                re[a] = ur + vr;
                im[a] = ui + vi;
                re[b] = ur - vr;
                im[b] = ui - vi;
                let next_r = wr * step_r - wi * step_i;
                wi = wr * step_i + wi * step_r;
                wr = next_r;
            }
            start += len;
        }
        len <<= 1;
    }
}

/// The one-sided magnitude spectrum of a real series, and what a bin is worth.
///
/// Owns its buffer and is REFILLED rather than rebuilt, because it lives for
/// the life of a link and is rewritten every two seconds: a fresh `Vec` per
/// estimate is two allocations per series per link per two seconds on a router
/// whose allocator is musl's, for a buffer whose size never changes once the
/// window is full.
#[derive(Default)]
struct Spectrum {
    /// `|X[k]|` for k in `0..=nfft/2`.
    mag: Vec<f64>,
    /// Hz per bin.
    bin_hz: f64,
}

/// Everything an estimate needs to scribble on, owned by the window.
///
/// After the first estimate every one of these is at its final length and
/// `resize` is a no-op, so a steady-state estimate allocates nothing at all.
/// That matters here more than it usually would: this runs on the agent's
/// runtime, eight links at a time, and it used to run under the lock a
/// data-model GET contends for.
#[derive(Default)]
struct Scratch {
    t: Vec<f64>,
    y: Vec<f64>,
    re: Vec<f64>,
    im: Vec<f64>,
    mag: Spectrum,
    phase: Spectrum,
}

impl Spectrum {
    /// The allocating constructor. Tests only: the agent goes through
    /// [`fill`](Self::fill) so that it allocates nothing after the first pass.
    #[cfg(test)]
    fn of(t: &[f64], y: &[f64], fs: f64) -> Option<Self> {
        let mut s = Self::default();
        let (mut re, mut im) = (Vec::new(), Vec::new());
        s.fill(t, y, fs, &mut re, &mut im).then_some(s)
    }

    /// Transform `y`, sampled at `fs` Hz, having first removed its mean and its
    /// linear trend and then applied a Hann window.
    ///
    /// The detrend is not cosmetic. A link warming up, or a chair moved once an
    /// hour ago, leaves a ramp across the window, and a ramp is almost entirely
    /// low-frequency power that lands squarely inside the 0.1-0.5 Hz breathing
    /// band. Untouched it is reported as a breath — by a room with nobody in
    /// it, at a confidence that looks excellent. `a_linear_trend_is_not_a_slow_breath`
    /// is the test that says so.
    ///
    /// The Hann window is for the opposite failure: a 30 s rectangle of a 0.25 Hz
    /// sinusoid ends mid-cycle, and the discontinuity smears power across every
    /// bin — including, at a few percent, the heart band, where it would be read
    /// as a pulse at whatever rate the leakage peaked.
    ///
    /// `t` carries the real timestamps and is what the trend is fitted against,
    /// because the records genuinely jitter. The TRANSFORM still assumes a
    /// uniform grid — that is what a DFT is — so `fs` is the mean rate over the
    /// window and the residual jitter shows up as a slight broadening of the
    /// peak rather than as a wrong frequency.
    fn fill(&mut self, t: &[f64], y: &[f64], fs: f64, re: &mut Vec<f64>, im: &mut Vec<f64>) -> bool {
        let n = y.len();
        if n < 4 || fs <= 0.0 {
            return false;
        }

        // Least-squares line through (t, y), subtracted.
        let inv = 1.0 / n as f64;
        let mt = t.iter().sum::<f64>() * inv;
        let my = y.iter().sum::<f64>() * inv;
        let mut stt = 0.0f64;
        let mut sty = 0.0f64;
        for (&ti, &yi) in t.iter().zip(y.iter()) {
            let d = ti - mt;
            stt += d * d;
            sty += d * (yi - my);
        }
        // `stt` is zero only when every timestamp is identical, which is not a
        // window; the slope is then taken as flat and the mean removal stands.
        let slope = if stt > 0.0 { sty / stt } else { 0.0 };

        let nfft = n.next_power_of_two();
        // `clear` then `resize` rather than `resize` alone: the buffer must be
        // ZERO past `n` (that is the zero-padding) and the previous estimate
        // left a spectrum's worth of numbers there.
        re.clear();
        re.resize(nfft, 0.0);
        im.clear();
        im.resize(nfft, 0.0);
        let denom = (n - 1) as f64;
        for (i, (&ti, &yi)) in t.iter().zip(y.iter()).enumerate() {
            let hann = 0.5 - 0.5 * (TAU * i as f64 / denom).cos();
            re[i] = (yi - my - slope * (ti - mt)) * hann;
        }
        fft(re, im);

        self.mag.clear();
        self.mag
            .extend((0..=nfft / 2).map(|k| re[k].hypot(im[k])));
        self.bin_hz = fs / nfft as f64;
        true
    }

    /// The bins whose centre frequency lies in `lo..=hi`, clamped to the bins
    /// that exist. DC is never included: the mean was removed, so bin 0 holds
    /// rounding error and nothing else.
    fn bins(&self, lo: f64, hi: f64) -> std::ops::RangeInclusive<usize> {
        let top = self.mag.len().saturating_sub(1);
        let k_lo = ((lo / self.bin_hz).ceil().max(1.0) as usize).min(top);
        let k_hi = ((hi / self.bin_hz).floor().max(0.0) as usize).min(top);
        k_lo..=k_hi
    }

    /// Sum of `|X[k]|²` over a band.
    fn power(&self, lo: f64, hi: f64) -> f64 {
        self.bins(lo, hi).map(|k| self.mag[k] * self.mag[k]).sum()
    }

    /// How many bins a band actually holds.
    ///
    /// Counted rather than derived from `hi - lo` because the two ends are
    /// rounded to whole bins and the top of the analysis band is additionally
    /// clipped at Nyquist on a slow link. The confidence below divides one of
    /// these by another, and a nominal width against a counted one would leave
    /// a chance level that is slightly wrong in a direction nobody could see.
    fn width(&self, lo: f64, hi: f64) -> usize {
        let r = self.bins(lo, hi);
        if r.is_empty() {
            0
        } else {
            r.end() - r.start() + 1
        }
    }

    /// The largest bin in a band, or `None` if the band holds no bins.
    fn peak(&self, lo: f64, hi: f64) -> Option<usize> {
        let r = self.bins(lo, hi);
        if r.is_empty() {
            return None;
        }
        r.max_by(|&a, &b| self.mag[a].total_cmp(&self.mag[b]))
    }

    /// The frequency of a peak, refined by fitting a parabola through the peak
    /// bin and its two neighbours.
    ///
    /// Worth the six lines: at 30 s and a 512-point transform a bin is 0.02 Hz,
    /// which is 1.2 BPM — larger than the tolerance anybody would want on a
    /// breathing rate. The interpolation recovers the peak to well under a
    /// tenth of a bin because a Hann main lobe is close to parabolic near its
    /// top, and it is why `a_quarter_hertz_sinusoid_is_fifteen_breaths_a_minute`
    /// can ask for +-1 BPM rather than +-2.
    fn refine(&self, k: usize) -> f64 {
        let (Some(&lo), Some(&hi)) = (self.mag.get(k.wrapping_sub(1)), self.mag.get(k + 1)) else {
            return k as f64 * self.bin_hz;
        };
        let mid = self.mag[k];
        let denom = lo - 2.0 * mid + hi;
        // A flat or inverted triple has no parabola through it; the bin centre
        // is then the honest answer rather than a division that runs away.
        let delta = if denom.abs() > f64::EPSILON {
            (0.5 * (lo - hi) / denom).clamp(-0.5, 0.5)
        } else {
            0.0
        };
        (k as f64 + delta) * self.bin_hz
    }
}

// ── Vital signs ──────────────────────────────────────────────────────────────

/// Breathing: 6 to 30 breaths a minute.
const BREATH_LO_HZ: f64 = 0.1;
const BREATH_HI_HZ: f64 = 0.5;
/// Heart: 48 to 120 beats a minute.
const HEART_LO_HZ: f64 = 0.8;
const HEART_HI_HZ: f64 = 2.0;

/// The band a confidence is measured against.
///
/// Not 0 to Nyquist. The bottom is above the residual drift the detrend does
/// not quite remove, and the top is where a sounded channel has nothing left
/// but receiver noise — including it would divide every band by a constant and
/// make every confidence look better than it is.
const TOTAL_LO_HZ: f64 = 0.05;
const TOTAL_HI_HZ: f64 = 4.0;

/// Shortest window that can resolve a breath.
///
/// 0.1 Hz is one cycle in ten seconds. Fifteen gives one and a half, which is
/// the least that is not simply a trend; the shipped configuration clamps the
/// window to at least twenty for the same reason with more margin.
const MIN_SPAN_SECS: f64 = 15.0;

/// Fewest samples an estimate will be made from. At the slowest supported
/// sounding rate this is still several seconds of records.
const MIN_SAMPLES: usize = 64;

/// Slowest mean sample rate an estimate will be made from.
///
/// Set by the BANDS, not by the analysis span. `2 x HEART_HI_HZ` is 4 Hz and
/// this is 5, a quarter of margin: below it the top of the heart band is at or
/// past Nyquist and a pulse would alias into the breathing band.
///
/// Deliberately NOT `2 x TOTAL_HI_HZ` (8 Hz), which is what it was. The
/// analysis span is only the confidence's denominator, [`Spectrum::bins`]
/// already clips it at Nyquist, and the chance level is counted from the bins
/// that EXIST -- so a slower link gets a narrower denominator and a correctly
/// rescaled confidence rather than a wrong one. At 8 Hz the old floor meant a
/// link that dropped a single sounding an hour fell under it, and a
/// `csi_period_ms` above 125 disabled vitals outright with nothing said.
const MIN_RATE_HZ: f64 = 5.0;

/// Longest gap between two records the ring will absorb, as a multiple of the
/// nominal sounding period.
///
/// A hole is not a missing sample; it is a TIMEBASE error. The transform
/// assumes a uniform grid, so every sample after a 1.9 s hole sits 1.9 s from
/// where the transform thinks it is -- three radians of error at 0.25 Hz, which
/// is half the window at the wrong phase. That does not fabricate a peak, it
/// destroys the real one and spreads its power across every band, which is
/// worse: a smeared spectrum still has a largest bin in the breathing band.
///
/// The gap that matters is therefore far shorter than the 2 s one
/// [`super::csi::STALE_GAP_SECS`] uses to resync the energy window. Three
/// periods is four consecutive missed soundings; at the shipped 100 ms that is
/// 500 ms, five samples of a 300-sample window. The live links deliver 8-9 of
/// 10 soundings a second and drop them in ones and twos, which three periods
/// would have turned into a ring that emptied every few seconds and never
/// reported. Five absorbs that and still empties on anything that would put a
/// visible step inside a breath.
const MAX_GAP_PERIODS: f64 = 5.0;

/// How often the transform is allowed to run, per link.
///
/// A breathing rate does not change in a second and the FFT is the only part of
/// this module that is not O(1) per record. Deliberately decoupled from the
/// record rate: at 9 records a second an estimate per record would be 18 FFTs a
/// second per link for an answer that is the same every time.
const ESTIMATE_EVERY_SECS: f64 = 2.0;

/// One link's (time, mean magnitude, mean phase) sample.
#[derive(Debug, Clone, Copy)]
struct VitalsSample {
    at: f64,
    mag: f64,
    phase: f64,
}

/// What one estimate found. Every field is `None` when it could not be
/// established, and never a fabricated zero: a controller cannot tell an
/// invented rate from a measured one.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Vitals {
    /// Breaths per minute, reported only above the confidence floor.
    pub breathing_bpm: Option<f64>,
    /// Beats per minute, reported only above the confidence floor.
    pub heart_bpm: Option<f64>,
    /// The better of the two bands' confidences, whether or not it cleared the
    /// floor.
    ///
    /// Not a raw power share: it is how far the band's share of the analysed
    /// power exceeds the share it would have if the series were noise, rescaled
    /// to 0..1. See [`band`] for why the raw share could not carry one
    /// threshold. Zero means "exactly what an empty room gives"; one means a
    /// pure tone.
    ///
    /// Reported below the floor on purpose: an integrator looking at a link
    /// that reports no rates needs to be able to tell "0.34, nearly" from
    /// "0.02, there is nobody there".
    pub confidence: Option<f64>,
}

/// One band's answer, before the confidence floor is applied.
struct Band {
    /// Beats or breaths a minute.
    bpm: f64,
    confidence: f64,
}

/// Half a minute of one link's per-record scalars, and the vitals over them.
///
/// Held by the run loop beside the link, not by the shared table: the ring is
/// three `f64` per record and nothing outside the drain reads it.
pub struct VitalsWindow {
    /// Seconds of history kept, and the stillness required before estimating.
    span: f64,
    /// Confidence below which nothing is reported.
    min_confidence: f64,
    /// The configured sounding period, in seconds. Only [`MAX_GAP_PERIODS`]
    /// uses it: the transform takes its rate from the timestamps.
    period: f64,
    ring: VecDeque<VitalsSample>,
    last_estimate_at: Option<f64>,
    estimates: u64,
    current: Vitals,
    scratch: Scratch,
    /// Whether this link has already complained about its sounding rate.
    ///
    /// A link too slow for the bands is too slow on every record, so the
    /// complaint is once per link and not once per estimate -- the alternative
    /// is a line every two seconds for the life of the process.
    warned_rate: bool,
}

impl VitalsWindow {
    pub fn new(span_secs: f64, min_confidence: f64, period_secs: f64) -> Self {
        Self {
            span: span_secs.max(MIN_SPAN_SECS),
            min_confidence,
            period: if period_secs > 0.0 { period_secs } else { 0.1 },
            ring: VecDeque::new(),
            last_estimate_at: None,
            estimates: 0,
            current: Vitals::default(),
            scratch: Scratch::default(),
            warned_rate: false,
        }
    }

    /// Add one record's scalars, and drop everything older than the span.
    ///
    /// Non-finite samples are DROPPED rather than clamped. A dead link produces
    /// a magnitude of zero and a phase of nothing at all, and one NaN in the
    /// ring makes every bin of the transform NaN — a whole window of vitals
    /// lost to one bad record, silently.
    ///
    /// The phase is unwrapped ALONG TIME here, as well as along the tones it
    /// was already unwrapped along in [`super::csi::mean_phase`]. A link's
    /// common phase rotates steadily because the two ends' oscillators are not
    /// the same oscillator, so the per-record scalar crosses +-pi every few
    /// seconds; wrapped, that is a 2-pi step in the series, which is broadband
    /// and lands in every band at once. Unwrapped, a constant rotation rate is
    /// a straight line — and a straight line is precisely what
    /// [`Spectrum::of`] removes before it transforms anything.
    ///
    /// The assumption this rests on is that the rotation is less than half a
    /// turn between records. At 9 records a second that is a residual carrier
    /// offset under 4.5 Hz, which a mesh link's tracking loops should be far
    /// inside — but it is an assumption about live hardware, and a link that
    /// breaks it produces a phase series that is noise rather than a wrong
    /// answer.
    pub fn push(&mut self, at: f64, mag: f64, phase: f64) {
        if !at.is_finite() || !mag.is_finite() || !phase.is_finite() {
            return;
        }
        // A hole in the ring is a timebase error, not a missing sample. See
        // `MAX_GAP_PERIODS`. This is a SEPARATE and much shorter test than the
        // 2 s one that resyncs the energy window, because the energy window is
        // two seconds long and this one is thirty: a gap far too short to
        // matter to a variance is most of a cycle to a 0.25 Hz transform.
        if self
            .ring
            .back()
            .is_some_and(|prev| at - prev.at > MAX_GAP_PERIODS * self.period)
        {
            self.clear();
        }
        let phase = match self.ring.back() {
            Some(prev) => prev.phase + wrap_pi(phase - prev.phase),
            None => phase,
        };
        self.ring.push_back(VitalsSample { at, mag, phase });
        let cutoff = at - self.span;
        while self.ring.front().is_some_and(|s| s.at < cutoff) {
            self.ring.pop_front();
        }
    }

    /// Forget the history and the estimate.
    ///
    /// Called when the link resyncs after a gap, for the same reason
    /// [`super::csi::CsiWindow::clear`] is: joining the two sides of a hole puts
    /// a step in the middle of the series, and a step is broadband — it would
    /// raise every band's power at once and land wherever the leakage peaked.
    pub fn clear(&mut self) {
        self.ring.clear();
        self.last_estimate_at = None;
        self.current = Vitals::default();
    }

    /// Recompute if the link has been still long enough and the gate has
    /// expired.
    ///
    /// `idle_since` is when the link's detector entered `Idle`, or `None` while
    /// it is in any other state. The requirement is a WHOLE window of stillness,
    /// not merely stillness now: a link that stopped moving a second ago has a
    /// window still twenty-nine seconds full of the person who was walking
    /// through it, and the transform cannot tell that from a chest.
    /// Returns the new value only when it CHANGED, so a caller that has to take
    /// a lock to store it can skip the lock on the overwhelming majority of
    /// ticks: the estimate is gated to once every two seconds and the drain
    /// runs four times a second.
    pub fn update(&mut self, now: f64, idle_since: Option<f64>) -> Option<Vitals> {
        if !idle_since.is_some_and(|s| now - s >= self.span) {
            return self.set(Vitals::default());
        }
        if self
            .last_estimate_at
            .is_some_and(|t| now - t < ESTIMATE_EVERY_SECS)
        {
            return None;
        }
        self.last_estimate_at = Some(now);
        self.estimates += 1;
        let v = self.estimate().unwrap_or_default();
        self.set(v)
    }

    /// Store a new reported value, reporting it only if it is not the old one.
    fn set(&mut self, v: Vitals) -> Option<Vitals> {
        (self.current != v).then(|| {
            self.current = v;
            v
        })
    }

    /// The most recent estimate, all-`None` when there is not one.
    pub fn vitals(&self) -> Vitals {
        self.current
    }

    /// How many times the transform has run. Test instrumentation for the 2 s
    /// gate; a counter is the only way to assert that something did NOT happen.
    #[cfg(test)]
    fn estimates(&self) -> u64 {
        self.estimates
    }

    /// Transform the window and read the two bands out of it.
    ///
    /// `None` when the window cannot carry the question — too few samples, too
    /// short a span, or a sounding rate too slow for the heart band. Reporting
    /// from those would be reporting the largest bin of a spectrum that has no
    /// bins where the answer would be.
    ///
    /// `&mut` because the buffers it transforms into are the window's own and
    /// are reused; see [`Scratch`].
    fn estimate(&mut self) -> Option<Vitals> {
        let n = self.ring.len();
        if n < MIN_SAMPLES {
            return None;
        }
        let first = self.ring.front()?.at;
        let last = self.ring.back()?.at;
        let span = last - first;
        if span < MIN_SPAN_SECS {
            return None;
        }
        // The MEAN rate over the window, not the configured period: records
        // jitter, and a few of them are missed outright when a peer is asleep.
        let fs = (n - 1) as f64 / span;
        if fs < MIN_RATE_HZ {
            if !self.warned_rate {
                self.warned_rate = true;
                // Said once per link, at warn: a link sounded too slowly for
                // the heart band reports empty vitals for ever, and empty is
                // also what a link with nobody in front of it reports. Without
                // this line the two are indistinguishable from outside.
                warn!(
                    "csi: {fs:.1} records/s is below the {MIN_RATE_HZ:.0}/s vitals \
                     need; breathing and heart will stay empty on this link \
                     (lower csi_period_ms to 120 or less)"
                );
            }
            return None;
        }

        // Copied into the scratch rather than collected: after the first pass
        // these are at their final length and the copy allocates nothing.
        let sc = &mut self.scratch;
        sc.t.clear();
        sc.y.clear();
        sc.t.extend(self.ring.iter().map(|s| s.at));
        sc.y.extend(self.ring.iter().map(|s| s.mag));
        if !sc.mag.fill(&sc.t, &sc.y, fs, &mut sc.re, &mut sc.im) {
            return None;
        }
        sc.y.clear();
        sc.y.extend(self.ring.iter().map(|s| s.phase));
        if !sc.phase.fill(&sc.t, &sc.y, fs, &mut sc.re, &mut sc.im) {
            return None;
        }
        let (sm, sp) = (&sc.mag, &sc.phase);

        let breathing = best(sm, sp, BREATH_LO_HZ, BREATH_HI_HZ);
        let heart = best(sm, sp, HEART_LO_HZ, HEART_HI_HZ);
        let confidence = [&breathing, &heart]
            .into_iter()
            .flatten()
            .map(|b| b.confidence)
            .fold(f64::NEG_INFINITY, f64::max);

        let clear = |b: Option<Band>| {
            b.filter(|b| b.confidence >= self.min_confidence)
                .map(|b| b.bpm)
        };
        Some(Vitals {
            breathing_bpm: clear(breathing),
            heart_bpm: clear(heart),
            confidence: confidence.is_finite().then_some(confidence),
        })
    }
}

/// The better of the two series for one band.
///
/// "Better" is the larger CONFIDENCE, not the larger absolute power: the
/// magnitude series is in linear receiver units and the phase series is in
/// radians, so their powers are not comparable at all and a comparison of them
/// would simply always pick magnitude. The confidence is dimensionless and
/// already has the noise floor taken out of it, so the two are comparable.
fn best(mag: &Spectrum, phase: &Spectrum, lo: f64, hi: f64) -> Option<Band> {
    let a = band(mag, lo, hi);
    let b = band(phase, lo, hi);
    match (a, b) {
        (Some(a), Some(b)) => Some(if b.confidence > a.confidence { b } else { a }),
        (a, b) => a.or(b),
    }
}

/// One band of one spectrum: where its peak is, and how much more of the
/// analysis band's power sits in it than would if the series were noise.
///
/// # Why the raw share is not the confidence
///
/// The obvious measure is `band power / total power`, and it is unusable as a
/// single threshold because a band's share of a FLAT spectrum is simply its
/// share of the width. Measured on white noise, exactly as predicted: 0.10 in
/// the 0.4 Hz-wide breathing band and 0.32 in the 1.2 Hz-wide heart band,
/// against widths of 0.4/3.95 and 1.2/3.95 of the analysed span. One floor of
/// 0.35 therefore meant "three and a half times chance" for breathing and
/// "a tenth above chance" for the heart — the same number meaning two different
/// things, and an empty room reporting a pulse on a fair fraction of windows.
///
/// So the chance level is subtracted and what is left is rescaled to run from
/// zero to one: `(share - w) / (1 - w)`, where `w` is the band's share of the
/// bins. White noise now scores ~0 in both bands, a pure tone scores ~1 in the
/// band that holds it, and one floor means one thing everywhere.
///
/// Floored at zero rather than allowed negative. A band with LESS than its
/// share of the power is a band with nothing in it, and how far below chance it
/// is says nothing anybody wants; a negative confidence would also render as a
/// number a controller has no reading for.
fn band(s: &Spectrum, lo: f64, hi: f64) -> Option<Band> {
    let total = s.power(TOTAL_LO_HZ, TOTAL_HI_HZ);
    // A series that is exactly flat — a constant, or the zero left by a
    // detrended pure ramp — has no power to take a share of, and every ratio
    // over it is rounding noise. It has no opinion, which is not the same as an
    // opinion of zero. Written with `is_normal` rather than `> 0.0` so a NaN
    // total, which compares false against everything, takes this branch too.
    if !total.is_normal() {
        return None;
    }
    let analysed = s.width(TOTAL_LO_HZ, TOTAL_HI_HZ);
    let here = s.width(lo, hi);
    if analysed == 0 || here == 0 || here >= analysed {
        // A band that is the whole analysed span carries no information about
        // itself: its share is 1 whatever the series does, and the rescaling
        // below would divide by zero. Happens only on a link too slow to reach
        // the top of the analysis band, which `estimate` already refuses.
        return None;
    }
    let chance = here as f64 / analysed as f64;
    let share = s.power(lo, hi) / total;
    let k = s.peak(lo, hi)?;
    Some(Band {
        bpm: 60.0 * s.refine(k),
        confidence: ((share - chance) / (1.0 - chance)).clamp(0.0, 1.0),
    })
}

// ── Falls ────────────────────────────────────────────────────────────────────

/// How close to the learnt ambient counts as "back down", in dB.
///
/// The link detector's own resting spread on a CSI energy is about 0.6 dB, so 3
/// is five times the noise and still far below the 12 dB rise that opens a
/// candidate. It is used for three things at once — ending an impact, starting
/// the stillness, and deciding that motion has resumed — deliberately: three
/// separate thresholds here would let a link sit in a gap between two of them
/// forever.
const NEAR_DB: f64 = 3.0;

/// How quickly the energy must rise for the rise to be an impact.
///
/// A fall is over in a few hundred milliseconds. Somebody walking in raises the
/// same energy by the same amount over several seconds, and the ONLY thing that
/// separates the two at the leading edge is how long the link spent in between.
const RISE_SECS: f64 = 1.0;

/// How long the energy may stay up before it is a person doing something rather
/// than a person landing.
const RETURN_SECS: f64 = 3.0;

/// Records that must be elevated before a transient is believed.
///
/// A corrupt sounding, or a peer that answered a retry burst, produces one
/// record with the rise and the return of a fall and none of its duration.
/// Three is the fewest that cannot be one artefact, and at 9 records a second
/// it is a third of a second — shorter than any fall.
const MIN_ELEVATED: u32 = 3;

/// How long after one candidate before another may be raised.
const COOLDOWN_SECS: f64 = 5.0;

/// Longest gap between two records before a sequence in progress is abandoned.
///
/// The whole discriminator is TIMING -- a rise inside a second, a return inside
/// three, then stillness -- and a gap in the record stream is a stretch of time
/// the detector has no measurements for. A `Still` that began before a
/// twenty-second hole is not a person lying still for twenty seconds; it is a
/// link that stopped reporting, and the room behind it could have held a party.
/// Matching [`RETURN_SECS`] rather than being a separate number: past the point
/// where an impact could still be part of this sequence, no sequence survives.
const MAX_RECORD_GAP_SECS: f64 = RETURN_SECS;

/// How long a candidate stands before it retracts itself.
///
/// A flag nobody cleared is worse than no flag: a controller polling an hour
/// later would be told about a fall that happened before lunch, with no way to
/// tell it from one that happened just now.
const CLEAR_SECS: f64 = 60.0;

/// What the fall detector has to say, when it has anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallEvent {
    /// `FallCandidate` went 0 -> 1.
    Raised,
    /// `FallCandidate` went 1 -> 0.
    Cleared,
}

/// Where a link is in the fall sequence.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    /// At or near the ambient, with nothing in progress.
    Quiet,
    /// Above the rise threshold, having got there quickly.
    Impact { rose_at: f64, elevated: u32 },
    /// Came back down inside the return window; counting stillness.
    Still { since: f64 },
}

/// A fall candidate from the per-record motion energy and the link's own
/// learnt ambient.
///
/// The shape it looks for is: a fast rise well above the ambient, a return to
/// the ambient within a few seconds, and then nobody moving for a long time.
/// All three are needed. The rise alone is a door closing; the rise and the
/// return alone is somebody sitting down; the stillness alone is an empty room.
///
/// Every threshold is a field rather than a constant read at the use site, for
/// the same reason [`super::motion::LinkDetector`]'s are: a test must be able to
/// shrink the ten-second stillness without also shrinking the anti-glitch rule
/// it is not testing, and an operator chasing a false positive on one odd link
/// could in principle be given knobs without a recompile. The two the
/// configuration actually exposes are the constructor's arguments; the rest are
/// private because nothing outside this module has an opinion about them.
pub struct FallDetector {
    /// dB above the ambient that opens a candidate.
    rise_db: f64,
    /// Seconds of stillness after the return before a candidate is raised.
    still_secs: f64,
    rise_secs: f64,
    return_secs: f64,
    cooldown_secs: f64,
    clear_secs: f64,

    phase: Phase,
    /// The last time the energy was within [`NEAR_DB`] of the ambient. This is
    /// what makes a rise FAST or slow: a ramp that crawls up over five seconds
    /// leaves this five seconds stale by the time the threshold is crossed.
    near_at: Option<f64>,
    /// Consecutive records above the ambient. Serves the anti-glitch rule and,
    /// while a candidate stands, is what "motion resumed" means.
    elevated: u32,
    /// When the last record arrived, so a hole in the stream can be told from a
    /// quiet room. See [`MAX_RECORD_GAP_SECS`].
    last_at: Option<f64>,
    raised_at: Option<f64>,
    cooldown_until: f64,
}

impl FallDetector {
    pub fn new(rise_db: f64, still_secs: f64) -> Self {
        Self {
            rise_db,
            still_secs,
            rise_secs: RISE_SECS,
            return_secs: RETURN_SECS,
            cooldown_secs: COOLDOWN_SECS,
            clear_secs: CLEAR_SECS,
            phase: Phase::Quiet,
            near_at: None,
            elevated: 0,
            last_at: None,
            raised_at: None,
            cooldown_until: f64::NEG_INFINITY,
        }
    }

    /// Is a candidate standing?
    pub fn candidate(&self) -> bool {
        self.raised_at.is_some()
    }

    /// Abandon any sequence in progress, keeping what has already been
    /// reported.
    ///
    /// Called when the link resyncs after a gap. A sequence is a claim about
    /// what happened over the last few seconds, and after a hole in the record
    /// stream there is no such claim to make: a `Still` that began before a
    /// twenty-second gap would otherwise MATURE on the first record after it
    /// and report a fall that the detector watched none of.
    ///
    /// `raised_at` and `cooldown_until` deliberately survive. They are about
    /// what the controller has already been told, and a gap in the records is
    /// not a reason to tell it something different -- retracting a standing
    /// candidate because a relay hiccuped would be a 1 followed by a 0 with
    /// nothing behind either.
    pub fn reset(&mut self) {
        self.phase = Phase::Quiet;
        self.near_at = None;
        self.elevated = 0;
        self.last_at = None;
    }

    /// Feed one record's motion energy against the link's learnt ambient.
    ///
    /// `baseline_db` is `None` while the link detector is still learning, and
    /// then nothing is judged at all: without an ambient there is no such thing
    /// as a rise above it, and a detector that assumed one would fire on the
    /// first records of every link on every boot.
    pub fn push(&mut self, energy_db: f64, baseline_db: Option<f64>, at: f64) -> Option<FallEvent> {
        let Some(base) = baseline_db.filter(|b| b.is_finite()) else {
            self.reset();
            return None;
        };
        if !energy_db.is_finite() {
            return None;
        }
        // A hole in the record stream, whether or not anybody told this
        // detector about it. `reset` on the resync path covers the gaps the
        // reader notices; this covers the rest, and makes the guarantee a
        // property of `push` rather than of every caller remembering.
        if self
            .last_at
            .is_some_and(|prev| at - prev > MAX_RECORD_GAP_SECS)
        {
            self.reset();
        }
        self.last_at = Some(at);

        let near = energy_db <= base + NEAR_DB;
        let high = energy_db >= base + self.rise_db;
        self.elevated = if near { 0 } else { self.elevated + 1 };

        // A standing candidate retracts on its own after a minute, or as soon
        // as somebody is plainly moving again — which is the good outcome, and
        // the one the controller most needs to see promptly.
        let mut event = None;
        if let Some(raised) = self.raised_at {
            if at - raised >= self.clear_secs || self.elevated >= MIN_ELEVATED {
                self.raised_at = None;
                event = Some(FallEvent::Cleared);
            }
        }

        self.phase = match self.phase {
            Phase::Quiet => {
                // The rise must be FAST: the link has to have been near the
                // ambient within the last second. After a sustained elevation
                // this is stale, which is what stops a walk from re-arming
                // without first coming back down.
                if high && self.near_at.is_some_and(|n| at - n <= self.rise_secs) {
                    Phase::Impact {
                        rose_at: at,
                        elevated: 1,
                    }
                } else {
                    Phase::Quiet
                }
            }
            Phase::Impact { rose_at, elevated } => {
                if near {
                    // Came back down. Believed only if it was up for more than
                    // a glitch and came back inside the return window.
                    if at - rose_at <= self.return_secs && elevated >= MIN_ELEVATED {
                        Phase::Still { since: at }
                    } else {
                        Phase::Quiet
                    }
                } else if at - rose_at > self.return_secs {
                    // Still up after three seconds: a person doing something,
                    // not a person landing.
                    Phase::Quiet
                } else {
                    Phase::Impact {
                        rose_at,
                        elevated: elevated + 1,
                    }
                }
            }
            Phase::Still { since } => {
                if !near {
                    // Got up before the stillness completed. Nothing to report:
                    // that is the whole point of waiting.
                    Phase::Quiet
                } else if at - since >= self.still_secs {
                    if at >= self.cooldown_until && self.raised_at.is_none() {
                        self.raised_at = Some(at);
                        self.cooldown_until = at + self.cooldown_secs;
                        event = Some(FallEvent::Raised);
                    }
                    Phase::Quiet
                } else {
                    Phase::Still { since }
                }
            }
        };

        // Recorded AFTER the machine, so `near_at` is where the link was BEFORE
        // this record: a rise is measured from the last time it was down, and
        // updating first would make every rise look instantaneous.
        if near {
            self.near_at = Some(at);
        }
        event
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    /// A deterministic pseudo-random sequence, uniform in -1..1.
    ///
    /// A fixed LCG written out here rather than a crate: the agent takes no new
    /// dependencies, and a test whose noise is reseeded per run is a test that
    /// fails once a month for nobody's reason and cannot be reproduced when it
    /// does.
    struct Noise(u64);

    impl Noise {
        fn new(seed: u64) -> Self {
            Self(seed)
        }
        fn next(&mut self) -> f64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((self.0 >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
        }
    }

    /// A window holding `n` samples at `fs` Hz, built from the two series.
    ///
    /// The span is set past the series length so nothing is pruned: these
    /// tests are about the transform, not about the ring.
    fn filled(
        n: usize,
        fs: f64,
        mut mag: impl FnMut(f64) -> f64,
        mut phase: impl FnMut(f64) -> f64,
    ) -> VitalsWindow {
        let mut w = VitalsWindow::new(n as f64 / fs + 10.0, 0.35, 1.0 / fs);
        for i in 0..n {
            let t = i as f64 / fs;
            w.push(t, mag(t), phase(t));
        }
        w
    }

    // ── The transform ────────────────────────────────────────────────────────

    /// The hand-written radix-2 transform against the definition it claims to
    /// compute.
    ///
    /// Written out because there is no FFT in the dependency list and there is
    /// not going to be one: a wrong butterfly or a wrong bit-reversal produces
    /// a spectrum that still looks like a spectrum, and every number downstream
    /// of it — the BPM, the confidence — would be plausible and wrong.
    #[test]
    fn the_transform_matches_a_direct_discrete_fourier_transform() {
        let n = 16;
        let mut nz = Noise::new(7);
        let x: Vec<f64> = (0..n).map(|_| nz.next()).collect();

        let mut re = x.clone();
        let mut im = vec![0.0; n];
        fft(&mut re, &mut im);

        for (k, (&rk, &ik)) in re.iter().zip(im.iter()).enumerate() {
            let (mut dr, mut di) = (0.0f64, 0.0f64);
            for (j, &xj) in x.iter().enumerate() {
                let ang = -TAU * (k * j) as f64 / n as f64;
                dr += xj * ang.cos();
                di += xj * ang.sin();
            }
            assert!(
                (rk - dr).abs() < 1e-9 && (ik - di).abs() < 1e-9,
                "bin {k}: fft gave ({rk:.9}, {ik:.9}), the definition gives ({dr:.9}, {di:.9})"
            );
        }
    }

    // ── Breathing and heart ──────────────────────────────────────────────────

    /// 0.25 Hz is fifteen breaths a minute, and that is what must come out.
    ///
    /// 300 samples at 10 Hz is the shape the shipped 30 s window has on a link
    /// sounded every 100 ms.
    #[test]
    fn a_quarter_hertz_sinusoid_is_fifteen_breaths_a_minute() {
        let mut nz = Noise::new(1);
        let mut w = filled(
            300,
            10.0,
            |t| 100.0 + 2.0 * (TAU * 0.25 * t).sin() + 0.3 * nz.next(),
            |_| 0.0,
        );
        let v = w.estimate().expect("a full window must produce an estimate");
        let bpm = v.breathing_bpm.expect("0.25 Hz is inside the breathing band");
        assert!(
            (bpm - 15.0).abs() <= 1.0,
            "0.25 Hz reported as {bpm:.2} BPM, not 15"
        );
        // Measured 0.9900: a clean tone scores near one, so the 0.35 floor is
        // not a close-run thing for a signal this plain.
        let c = v.confidence.expect("a confidence is always reported");
        assert!(
            c >= 0.9,
            "a clean 0.25 Hz tone scored only {c:.4}; the floor is 0.35"
        );
        assert!(
            v.heart_bpm.is_none(),
            "a breathing-only series reported a heart rate of {:?}",
            v.heart_bpm
        );
    }

    /// 1.2 Hz is seventy-two beats a minute, carried on the PHASE series.
    ///
    /// Phase deliberately: the whole reason phase is unwrapped and detrended
    /// per record is that a chest wall displaces the path by millimetres, which
    /// moves the phase long before it moves the magnitude.
    #[test]
    fn a_one_point_two_hertz_sinusoid_is_seventy_two_beats_a_minute() {
        let mut nz = Noise::new(2);
        let mut w = filled(
            300,
            10.0,
            |_| 100.0,
            |t| 0.05 * (TAU * 1.2 * t).sin() + 0.004 * nz.next(),
        );
        let v = w.estimate().expect("a full window must produce an estimate");
        let bpm = v.heart_bpm.expect("1.2 Hz is inside the heart band");
        assert!(
            (bpm - 72.0).abs() <= 3.0,
            "1.2 Hz reported as {bpm:.2} BPM, not 72"
        );
        // Measured 0.9969. Before the chance level was taken out, a heart-band
        // tone and an empty room scored 0.997 and 0.315 — a factor of three
        // apart, with the floor sitting only 0.035 above the empty room.
        let c = v.confidence.expect("a confidence is always reported");
        assert!(
            c >= 0.9,
            "a clean 1.2 Hz tone scored only {c:.4}; the floor is 0.35"
        );
    }

    /// A chest does both at once, and the two bands must not shadow each other.
    ///
    /// Superposed the way a real one superposes them: the breath in the
    /// magnitude series, where a chest moving centimetres changes the received
    /// power, and the pulse in the phase series, where a chest wall moving
    /// millimetres is several degrees of carrier phase and no power at all.
    /// Each band then picks the series it belongs to, which is the whole reason
    /// both are transformed.
    #[test]
    fn breathing_and_heart_are_found_in_the_same_window() {
        let mut a = Noise::new(3);
        let mut b = Noise::new(11);
        let mut w = filled(
            300,
            10.0,
            |t| 100.0 + 2.0 * (TAU * 0.25 * t).sin() + 0.2 * a.next(),
            |t| 0.05 * (TAU * 1.2 * t).sin() + 0.004 * b.next(),
        );
        let v = w.estimate().expect("a full window must produce an estimate");
        let br = v.breathing_bpm.expect("the breathing component");
        let hr = v.heart_bpm.expect("the heart component");
        assert!((br - 15.0).abs() <= 1.0, "breathing came out {br:.2}");
        assert!((hr - 72.0).abs() <= 3.0, "heart came out {hr:.2}");
        // Measured 0.9971: the reported confidence is the better of the two,
        // and both bands are clean here.
        let c = v.confidence.expect("a confidence is always reported");
        assert!(c >= 0.9, "both bands clean but the confidence was {c:.4}");
    }

    /// The limitation the confidence definition carries, written down as a
    /// test so nobody rediscovers it on a live subject.
    ///
    /// Confidence is a band's share of the power in 0.05-4 Hz, so within ONE
    /// series the two bands compete: a breath four times the amplitude of a
    /// pulse takes sixteen times the power and leaves the pulse a share no
    /// floor worth having would pass. A heartbeat is therefore only findable in
    /// a series the breath does not dominate — in practice the phase series —
    /// and a link whose phase is too noisy to carry it will report breathing
    /// and nothing else however still the subject is.
    #[test]
    fn a_strong_breath_hides_a_weak_pulse_in_the_same_series() {
        let mut nz = Noise::new(12);
        let mut w = filled(
            300,
            10.0,
            |t| {
                100.0 + 2.0 * (TAU * 0.25 * t).sin() + 0.5 * (TAU * 1.2 * t).sin() + 0.1 * nz.next()
            },
            |_| 0.0,
        );
        let v = w.estimate().expect("a full window must produce an estimate");
        assert!(v.breathing_bpm.is_some(), "the breath must still be found");
        assert!(
            v.heart_bpm.is_none(),
            "the pulse cleared the floor at {:?} BPM inside a series the breath \
             dominates; if that is now possible this comment is wrong",
            v.heart_bpm
        );
    }

    /// An empty room is noise, and noise must report nothing at all.
    ///
    /// This is the test the feature lives or dies by. A spectrum always has a
    /// largest bin in every band, so an estimator that reports its peak
    /// unconditionally reports a breathing rate for a room with nobody in it —
    /// confidently, and forever.
    #[test]
    fn white_noise_alone_reports_nothing() {
        let mut a = Noise::new(4);
        let mut b = Noise::new(5);
        let mut w = filled(300, 10.0, |_| 100.0 + a.next(), |_| 0.01 * b.next());
        let v = w.estimate().expect("noise still produces a confidence");
        assert!(
            v.breathing_bpm.is_none() && v.heart_bpm.is_none(),
            "white noise was reported as vital signs: {v:?}"
        );
        // Measured 0.1085, against raw power shares of 0.1968 (breathing) and
        // 0.2588 (heart) — both within a draw's noise of those bands' width
        // shares, which is all an empty room ever is. The spread across
        // realisations is `white_noise_scores_near_zero_in_both_bands`; what
        // matters here is that it does not come near the 0.35 floor.
        let c = v.confidence.expect("a confidence is always reported");
        assert!(
            c < 0.2,
            "white noise scored {c:.4}, uncomfortably close to the 0.35 floor"
        );
    }

    /// The confidence must mean the same thing in both bands.
    ///
    /// It did not before. A band's share of a FLAT spectrum is simply its share
    /// of the WIDTH, so white noise scored 0.10 in the 0.4 Hz breathing band
    /// and 0.31 in the 1.2 Hz heart band. One floor of 0.35 was then three and
    /// a half times chance for breathing and a thirtieth above chance for the
    /// heart — the same number meaning two different things, and an empty room
    /// reporting a pulse on a fair fraction of windows.
    ///
    /// Twelve independent realisations rather than one, because what matters is
    /// the DISTRIBUTION and a single seed is one draw from it. Measured on
    /// these twelve: breathing mean 0.036, worst 0.109; heart mean 0.025, worst
    /// 0.169. Both bands now sit near zero and no realisation comes close to
    /// the 0.35 floor — which is the property the whole feature rests on,
    /// asserted here per realisation rather than on the average.
    #[test]
    fn white_noise_scores_near_zero_in_both_bands() {
        let fs = 10.0;
        let t: Vec<f64> = (0..300).map(|i| f64::from(i) / fs).collect();
        let runs = 12u64;

        let mut br_sum = 0.0f64;
        let mut hr_sum = 0.0f64;
        for seed in 1..=runs {
            let mut nz = Noise::new(seed);
            let y: Vec<f64> = t.iter().map(|_| nz.next()).collect();
            let s = Spectrum::of(&t, &y, fs).expect("a transformable series");
            let br = band(&s, BREATH_LO_HZ, BREATH_HI_HZ).expect("a populated band");
            let hr = band(&s, HEART_LO_HZ, HEART_HI_HZ).expect("a populated band");

            // The floor is what actually protects an empty room, so it is
            // asserted on every realisation and not on the average.
            assert!(
                br.confidence < 0.35 && hr.confidence < 0.35,
                "seed {seed}: white noise cleared the shipped floor at \
                 breathing {:.4} / heart {:.4}",
                br.confidence,
                hr.confidence
            );
            br_sum += br.confidence;
            hr_sum += hr.confidence;
        }

        let (br_mean, hr_mean) = (br_sum / runs as f64, hr_sum / runs as f64);
        assert!(
            br_mean < 0.1,
            "white noise averages {br_mean:.4} in the breathing band"
        );
        assert!(
            hr_mean < 0.1,
            "white noise averages {hr_mean:.4} in the heart band"
        );
        // The two must also be near each other: the whole point of taking the
        // chance level out is that one floor means one thing in both bands, and
        // a mean of 0.04 against a mean of 0.30 is exactly what it was before.
        assert!(
            (br_mean - hr_mean).abs() < 0.05,
            "the bands are still asymmetric: breathing {br_mean:.4}, \
             heart {hr_mean:.4}"
        );
    }

    /// The chance level the confidence subtracts is the band's share of the
    /// BINS, and that has to be what white noise actually puts there — or the
    /// wrong number is being subtracted and nothing downstream would show it.
    #[test]
    fn the_chance_level_is_the_bands_share_of_the_width() {
        let fs = 10.0;
        let t: Vec<f64> = (0..300).map(|i| f64::from(i) / fs).collect();
        let mut nz = Noise::new(5);
        let y: Vec<f64> = t.iter().map(|_| nz.next()).collect();
        let s = Spectrum::of(&t, &y, fs).expect("a transformable series");

        let analysed = s.width(TOTAL_LO_HZ, TOTAL_HI_HZ) as f64;
        let total = s.power(TOTAL_LO_HZ, TOTAL_HI_HZ);
        for (name, lo, hi, expect) in [
            ("breathing", BREATH_LO_HZ, BREATH_HI_HZ, 0.0990),
            ("heart", HEART_LO_HZ, HEART_HI_HZ, 0.3069),
        ] {
            let chance = s.width(lo, hi) as f64 / analysed;
            assert!(
                (chance - expect).abs() < 0.005,
                "the {name} band is {chance:.4} of the analysed bins, not {expect}"
            );
            // Measured on this seed: breathing 0.0779, heart 0.3153.
            let raw = s.power(lo, hi) / total;
            assert!(
                (raw - chance).abs() < 0.05,
                "white noise put {raw:.4} of the power in the {name} band \
                 against a width share of {chance:.4}; the chance level \
                 subtracted is the wrong one"
            );
        }
    }

    /// A slow drift is not a slow breath.
    ///
    /// A link warming up, or a neighbour's furniture moved once, puts a ramp
    /// through the window. Untouched, a ramp is almost all low-frequency power
    /// and lands squarely in the 0.1-0.5 Hz band — a breathing rate invented
    /// out of thermal drift. Removing the mean AND the linear trend before the
    /// transform is what stops it.
    #[test]
    fn a_linear_trend_is_not_a_slow_breath() {
        let mut a = Noise::new(6);
        let mut b = Noise::new(7);
        let mut w = filled(
            300,
            10.0,
            |t| 100.0 + 4.0 * t + a.next(),
            |_| 0.01 * b.next(),
        );
        let v = w.estimate().expect("a full window must produce an estimate");
        assert!(
            v.breathing_bpm.is_none(),
            "a linear drift was reported as breathing at {:?} BPM",
            v.breathing_bpm
        );
    }

    /// A phase series that crosses +-pi must not put a step in the window.
    ///
    /// The per-record scalar is an angle, so a link whose common phase rotates
    /// — and every real one does, because the two ends run on different
    /// crystals — wraps every few seconds. A wrapped series is a sawtooth, and
    /// a sawtooth is broadband: it raises every band at once and reports a
    /// heart rate wherever the leakage happened to peak.
    #[test]
    fn a_wrapping_phase_is_a_line_and_not_a_sawtooth() {
        let mut nz = Noise::new(13);
        // A steady 0.8 rad per record of drift — nearly two wraps a second —
        // with a real 1.2 Hz pulse riding on it.
        let mut w = VitalsWindow::new(40.0, 0.35, 0.1);
        for i in 0..300 {
            let t = i as f64 / 10.0;
            let drift = 0.8 * i as f64;
            let p = drift + 0.05 * (TAU * 1.2 * t).sin() + 0.004 * nz.next();
            w.push(t, 100.0, wrap_pi(p));
        }
        let v = w.estimate().expect("a full window must produce an estimate");
        let bpm = v.heart_bpm.expect("the pulse survives the wrapping");
        assert!(
            (bpm - 72.0).abs() <= 3.0,
            "a wrapping phase reported {bpm:.2} BPM, not 72"
        );
    }

    /// The radio does not sound on the beat. Records arrive 8-9 a second with
    /// the period jittering by tens of milliseconds, and the transform assumes
    /// a uniform grid — so the grid it assumes must come from the timestamps
    /// rather than from the configured period.
    #[test]
    fn jittered_sample_times_still_give_the_right_rate() {
        let mut j = Noise::new(8);
        let mut nz = Noise::new(9);
        let mut w = VitalsWindow::new(40.0, 0.35, 0.1);
        let mut t = 0.0f64;
        for i in 0..300 {
            // +-20 ms around a 100 ms period, and never backwards.
            t = (i as f64 * 0.1 + 0.02 * j.next()).max(t + 0.001);
            w.push(t, 100.0 + 2.0 * (TAU * 0.25 * t).sin() + 0.3 * nz.next(), 0.0);
        }
        let v = w.estimate().expect("a jittered window is still a window");
        let bpm = v.breathing_bpm.expect("0.25 Hz survives the jitter");
        assert!(
            (bpm - 15.0).abs() <= 1.0,
            "jittered sampling reported {bpm:.2} BPM, not 15"
        );
    }

    /// Vital signs mean nothing while somebody is walking through the path, so
    /// they are withheld until the link has been Idle for a WHOLE window — not
    /// merely Idle now, which would estimate from a window still half full of
    /// the motion that just stopped.
    #[test]
    fn vitals_are_withheld_until_the_link_has_been_idle_a_whole_window() {
        let mut nz = Noise::new(10);
        let mut w = VitalsWindow::new(30.0, 0.35, 0.1);
        for i in 0..300 {
            let t = i as f64 / 10.0;
            w.push(t, 100.0 + 2.0 * (TAU * 0.25 * t).sin() + 0.3 * nz.next(), 0.0);
        }

        let _ = w.update(30.0, None);
        assert_eq!(w.vitals(), Vitals::default(), "a moving link reported vitals");

        // Idle, but only for half a window.
        let _ = w.update(30.0, Some(15.0));
        assert_eq!(
            w.vitals(),
            Vitals::default(),
            "a link idle for half a window reported vitals"
        );

        let _ = w.update(30.0, Some(0.0));
        assert!(
            w.vitals().breathing_bpm.is_some(),
            "a full window of stillness reported nothing"
        );
    }

    /// The transform is the expensive part of this module and the answer moves
    /// on the scale of a breath, so it runs at most every two seconds.
    #[test]
    fn the_estimate_is_recomputed_at_most_every_two_seconds() {
        let mut w = VitalsWindow::new(30.0, 0.35, 0.1);
        for i in 0..300 {
            let t = i as f64 / 10.0;
            w.push(t, 100.0 + 2.0 * (TAU * 0.25 * t).sin(), 0.0);
        }
        let _ = w.update(30.0, Some(0.0));
        assert_eq!(w.estimates(), 1);
        let _ = w.update(31.0, Some(0.0));
        assert_eq!(w.estimates(), 1, "recomputed inside the two-second gate");
        let _ = w.update(32.0, Some(0.0));
        assert_eq!(w.estimates(), 2, "not recomputed after the gate expired");
    }

    /// A window too short to resolve a breath must say so rather than report
    /// the largest bin it happens to have.
    #[test]
    fn a_window_shorter_than_a_breath_estimates_nothing() {
        let mut w = filled(40, 10.0, |t| 100.0 + (TAU * 0.25 * t).sin(), |_| 0.0);
        assert!(
            w.estimate().is_none(),
            "four seconds of records produced a breathing rate"
        );
    }

    /// A hole in the ring is a TIMEBASE error, not a missing sample: the
    /// transform assumes a uniform grid, so everything after a 1.9 s hole sits
    /// 1.9 s from where the transform believes it is — three radians at
    /// 0.25 Hz, which destroys the real peak and spreads its power everywhere.
    ///
    /// Deliberately tested at 1.9 s, which is UNDER the 2 s gap that resyncs
    /// the energy window: the energy window is two seconds long and this one is
    /// thirty, so a gap far too short to matter to a variance is most of a
    /// cycle to this.
    #[test]
    fn a_hole_too_short_to_resync_the_energy_still_empties_the_ring() {
        let mut nz = Noise::new(20);
        let mut w = VitalsWindow::new(40.0, 0.35, 0.1);
        for i in 0..300 {
            let t = f64::from(i) / 10.0;
            w.push(t, 100.0 + 2.0 * (TAU * 0.25 * t).sin() + 0.3 * nz.next(), 0.0);
        }
        assert!(w.estimate().is_some(), "the fixture did not fill the ring");

        // Two missed soundings: under the threshold, and must be absorbed.
        w.push(29.9 + 0.25, 100.0, 0.0);
        assert!(
            w.estimate().is_some(),
            "an ordinary missed sounding emptied the ring"
        );

        // A 1.9 s hole.
        w.push(30.15 + 1.9, 100.0, 0.0);
        assert!(
            w.estimate().is_none(),
            "a 1.9 s hole was transformed as if it were not there"
        );
    }

    /// The rate floor is set by the BANDS, not by the analysis span.
    ///
    /// It used to be twice the top of the analysis band, 8 Hz, which a link
    /// sounded at the shipped 100 ms clears by a tenth — so a single dropped
    /// sounding took it under and vitals stopped, silently, and any
    /// `csi_period_ms` above 125 disabled them outright. What actually has to
    /// fit is the heart band: below 4 Hz a pulse aliases into the breathing
    /// band. The analysis span is only the confidence's denominator and is
    /// clipped at Nyquist, with the chance level counted from the bins that
    /// exist.
    #[test]
    fn the_rate_floor_is_the_heart_band_and_not_the_analysis_span() {
        let tone = |t: f64| 100.0 + 2.0 * (TAU * 0.25 * t).sin();
        // 6 Hz: under the old 8 Hz floor, comfortably over the real one.
        let mut ok = filled(180, 6.0, tone, |_| 0.0);
        let v = ok
            .estimate()
            .expect("6 records/s is enough for the heart band");
        let bpm = v.breathing_bpm.expect("the breath is still there at 6 Hz");
        assert!((bpm - 15.0).abs() <= 1.0, "6 Hz reported {bpm:.2} BPM");

        // 4 Hz: Nyquist is the top of the heart band itself.
        let mut slow = filled(120, 4.0, tone, |_| 0.0);
        assert!(
            slow.estimate().is_none(),
            "4 records/s cannot carry a 2 Hz pulse and must refuse"
        );
    }

    // ── Falls ────────────────────────────────────────────────────────────────

    /// A fall detector with the stillness shrunk so a test can drive one in a
    /// few seconds instead of ten. Every other threshold is the shipped value.
    fn faller(still_secs: f64) -> FallDetector {
        FallDetector::new(12.0, still_secs)
    }

    /// Drive `secs` seconds of records at 10 Hz whose energy comes from `f`,
    /// against a baseline of -22 dB, collecting what the detector said.
    fn drive(
        d: &mut FallDetector,
        from: f64,
        secs: f64,
        mut f: impl FnMut(f64) -> f64,
    ) -> Vec<(f64, FallEvent)> {
        let mut out = Vec::new();
        let n = (secs * 10.0).round() as i64;
        for i in 0..n {
            let t = from + i as f64 / 10.0;
            if let Some(e) = d.push(f(t), Some(-22.0), t) {
                out.push((t, e));
            }
        }
        out
    }

    /// The shape the detector exists for: a hard impact, an immediate return
    /// to the ambient, and then nobody moving.
    #[test]
    fn the_canonical_fall_fires_once() {
        let mut d = faller(2.0);
        // Two seconds of ambient, half a second of impact, then stillness.
        let mut events = drive(&mut d, 0.0, 2.0, |_| -22.0);
        events.extend(drive(&mut d, 2.0, 0.5, |_| -6.0));
        events.extend(drive(&mut d, 2.5, 4.0, |_| -22.0));

        assert_eq!(
            events.iter().filter(|(_, e)| *e == FallEvent::Raised).count(),
            1,
            "the canonical fall did not raise exactly one candidate: {events:?}"
        );
        assert!(d.candidate(), "the candidate must still be standing");
    }

    /// Somebody walking about raises the energy just as far and keeps it there.
    /// The whole discriminator is the RETURN: a fall stops, a walk does not.
    #[test]
    fn a_walk_never_fires() {
        let mut d = faller(2.0);
        let mut events = drive(&mut d, 0.0, 2.0, |_| -22.0);
        // Twelve seconds well above the baseline, then back down.
        events.extend(drive(&mut d, 2.0, 12.0, |t| -8.0 + (t * 3.0).sin()));
        events.extend(drive(&mut d, 14.0, 6.0, |_| -22.0));

        assert!(
            events.is_empty(),
            "a walk was reported as a fall: {events:?}"
        );
        assert!(!d.candidate());
    }

    /// One record of nonsense — a corrupt sounding, a retry burst — has the
    /// rise and the return of a fall and none of its duration.
    #[test]
    fn a_one_record_glitch_never_fires() {
        let mut d = faller(2.0);
        let mut events = drive(&mut d, 0.0, 2.0, |_| -22.0);
        // A single record at +16 dB, then straight back.
        events.extend(drive(&mut d, 2.0, 0.1, |_| -6.0));
        events.extend(drive(&mut d, 2.1, 6.0, |_| -22.0));

        assert!(
            events.is_empty(),
            "a one-record spike was reported as a fall: {events:?}"
        );
    }

    /// Somebody who falls, gets up, and falls again is two events, not one.
    ///
    /// Getting up is what clears the first candidate; without that clear the
    /// second fall has nothing to report, because the flag is already 1.
    #[test]
    fn two_falls_eight_seconds_apart_are_two_events() {
        let mut d = faller(1.0);
        let mut events = drive(&mut d, 0.0, 1.0, |_| -22.0);
        // Fall, stillness, candidate at ~t=2.5.
        events.extend(drive(&mut d, 1.0, 0.5, |_| -6.0));
        events.extend(drive(&mut d, 1.5, 6.0, |_| -22.0));
        // Getting up: sustained motion clears the candidate.
        events.extend(drive(&mut d, 7.5, 1.0, |_| -9.0));
        // The second fall, eight seconds after the first.
        events.extend(drive(&mut d, 8.5, 0.5, |_| -6.0));
        events.extend(drive(&mut d, 9.0, 4.0, |_| -22.0));

        let raised: Vec<f64> = events
            .iter()
            .filter(|(_, e)| *e == FallEvent::Raised)
            .map(|(t, _)| *t)
            .collect();
        assert_eq!(raised.len(), 2, "expected two falls, got {events:?}");
        assert!(
            raised[1] - raised[0] >= 5.0,
            "the two raises are {:.1} s apart, inside the cooldown",
            raised[1] - raised[0]
        );
        assert_eq!(
            events.iter().filter(|(_, e)| *e == FallEvent::Cleared).count(),
            1,
            "getting up must clear the first candidate: {events:?}"
        );
    }

    /// A second candidate inside the cooldown is the same fall seen twice.
    #[test]
    fn a_second_fall_inside_the_cooldown_is_not_reported() {
        let mut d = faller(0.5);
        let mut events = drive(&mut d, 0.0, 1.0, |_| -22.0);
        events.extend(drive(&mut d, 1.0, 0.5, |_| -6.0));
        events.extend(drive(&mut d, 1.5, 1.5, |_| -22.0));
        // Motion resumes, clearing the flag, then a second complete fall — all
        // inside five seconds of the first raise.
        events.extend(drive(&mut d, 3.0, 0.5, |_| -9.0));
        events.extend(drive(&mut d, 3.5, 0.5, |_| -6.0));
        events.extend(drive(&mut d, 4.0, 2.0, |_| -22.0));

        assert_eq!(
            events.iter().filter(|(_, e)| *e == FallEvent::Raised).count(),
            1,
            "the cooldown did not suppress the second raise: {events:?}"
        );
    }

    /// A candidate nobody acted on does not stand forever: it clears itself
    /// after a minute, so a controller polling an hour later is not told about
    /// a fall that happened before lunch.
    #[test]
    fn a_fall_clears_itself_after_a_minute() {
        let mut d = faller(1.0);
        let mut events = drive(&mut d, 0.0, 1.0, |_| -22.0);
        events.extend(drive(&mut d, 1.0, 0.5, |_| -6.0));
        events.extend(drive(&mut d, 1.5, 3.0, |_| -22.0));
        assert!(d.candidate(), "no candidate was raised at all: {events:?}");

        events.extend(drive(&mut d, 4.5, 60.0, |_| -22.0));
        assert!(!d.candidate(), "the candidate never cleared");
        assert_eq!(
            events.iter().filter(|(_, e)| *e == FallEvent::Cleared).count(),
            1,
            "expected exactly one clear: {events:?}"
        );
    }

    /// A sequence in progress must not MATURE across a hole in the records.
    ///
    /// The shape that used to fire: an impact, a return, the stillness clock
    /// started — and then the capture stops for twenty seconds. The first
    /// record after the hole is `still_secs` past the moment the stillness
    /// began, so the detector raised a candidate having watched none of the
    /// twenty seconds it was claiming to have watched. On the real device this
    /// is the ordinary consequence of a relay hiccup or a peer going to sleep,
    /// and the vitals ring and the energy window both reset around it while the
    /// fall detector did not.
    #[test]
    fn a_fall_does_not_mature_across_a_capture_gap() {
        let mut d = faller(2.0);
        // Ambient, impact, return: the stillness clock is now running.
        let mut events = drive(&mut d, 0.0, 2.0, |_| -22.0);
        events.extend(drive(&mut d, 2.0, 0.5, |_| -6.0));
        events.extend(drive(&mut d, 2.5, 0.3, |_| -22.0));
        assert!(events.is_empty(), "nothing should have fired yet: {events:?}");

        // Twenty seconds of nothing, then the records come back, still quiet.
        events.extend(drive(&mut d, 22.8, 5.0, |_| -22.0));
        assert!(
            events.is_empty(),
            "a fall matured across a twenty-second hole: {events:?}"
        );
        assert!(!d.candidate());
    }

    /// `reset` abandons the sequence and keeps what was already reported.
    ///
    /// A standing candidate is something the controller has been told. A gap in
    /// the records is not a reason to tell it something different — retracting
    /// on a relay hiccup would be a 1 followed by a 0 with nothing behind
    /// either.
    #[test]
    fn a_reset_abandons_the_sequence_and_keeps_the_candidate() {
        let mut d = faller(1.0);
        let mut events = drive(&mut d, 0.0, 1.0, |_| -22.0);
        events.extend(drive(&mut d, 1.0, 0.5, |_| -6.0));
        events.extend(drive(&mut d, 1.5, 3.0, |_| -22.0));
        assert!(d.candidate(), "the fixture did not raise one: {events:?}");

        d.reset();
        assert!(d.candidate(), "a reset retracted a standing candidate");
        // And nothing new matures out of the abandoned sequence.
        let after = drive(&mut d, 4.5, 5.0, |_| -22.0);
        assert!(after.is_empty(), "the abandoned sequence fired: {after:?}");
    }

    /// A link still learning its ambient has no baseline to rise above, and a
    /// detector that guessed one would fire on the first record of every link.
    #[test]
    fn nothing_fires_while_the_baseline_is_unknown() {
        let mut d = faller(1.0);
        for i in 0..200 {
            let t = i as f64 / 10.0;
            let db = if (20..25).contains(&i) { -6.0 } else { -22.0 };
            assert!(d.push(db, None, t).is_none(), "fired without a baseline");
        }
        assert!(!d.candidate());
    }
}
