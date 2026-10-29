//! Explicit, bounded correlation between a Windows QPC instant and media time.
//!
//! The producer must bracket the very same QPC observation with MediaTime
//! readings from a monotonic media clock. Merely sampling two clocks at
//! unrelated call sites is not a valid calibration. An owner must limit the
//! validity interval to the period in which their rates are known to agree.
//! This type never fabricates a correlation when a device timestamp or clock
//! epoch is unavailable.

use crate::MediaTime;

/// An inclusive interval of possible media timestamps, in nanoseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MediaTimeBounds {
    pub earliest: MediaTime,
    pub latest: MediaTime,
}

/// A bounded estimate on the audio sample grid. The bounds are inclusive;
/// `uncertainty_ns` is the maximum error before sample-grid rounding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SampleTimeEstimate {
    pub sample: u64,
    pub uncertainty_ns: u64,
    pub exact: bool,
}

/// Immutable proof supplied by the clock owner for one session and epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QpcMediaCorrelation {
    session: u64,
    clock_epoch: u64,
    qpc_ns: u64,
    media: MediaTimeBounds,
    valid_from_qpc_ns: u64,
    valid_until_qpc_ns: u64,
}

impl QpcMediaCorrelation {
    /// `earliest` and `latest` must bracket the same QPC reading. The caller
    /// supplies an uncertainty ceiling and an explicit validity interval;
    /// neither freshness nor a shared clock domain is inferred here.
    pub fn new(session: u64, clock_epoch: u64, qpc_ns: u64,
        media: MediaTimeBounds, valid_from_qpc_ns: u64, valid_until_qpc_ns: u64,
        max_uncertainty_ns: u64) -> Option<Self> {
        if session == 0 || clock_epoch == 0 || media.earliest.0 > media.latest.0
            || media.latest.0.checked_sub(media.earliest.0)? > max_uncertainty_ns
            || valid_from_qpc_ns > qpc_ns || qpc_ns > valid_until_qpc_ns {
            return None;
        }
        Some(Self {session, clock_epoch, qpc_ns, media, valid_from_qpc_ns, valid_until_qpc_ns})
    }

    /// Maps only observations from this exact session/epoch and validity
    /// interval. Both bounds remain visible to the caller; no midpoint is used.
    pub fn media_bounds(&self, session: u64, clock_epoch: u64, qpc_ns: u64) -> Option<MediaTimeBounds> {
        if session != self.session || clock_epoch != self.clock_epoch
            || !(self.valid_from_qpc_ns..=self.valid_until_qpc_ns).contains(&qpc_ns) {
            return None;
        }
        let delta = i128::from(qpc_ns) - i128::from(self.qpc_ns);
        let earliest = u64::try_from(i128::from(self.media.earliest.0).checked_add(delta)?).ok()?;
        let latest = u64::try_from(i128::from(self.media.latest.0).checked_add(delta)?).ok()?;
        Some(MediaTimeBounds { earliest: MediaTime(earliest), latest: MediaTime(latest) })
    }

    /// Estimate a graph-clock interval only within the caller's explicit error
    /// budget. This maps the entire interval including calibration uncertainty;
    /// it does not assert anything about camera exposure or hardware accuracy.
    pub fn estimate_interval(&self, session: u64, clock_epoch: u64,
        qpc_bounds: (u64, u64), sample_rate: u32, max_error_ns: u64) -> Option<SampleTimeEstimate> {
        if !(8000..=192000).contains(&sample_rate) || qpc_bounds.0 > qpc_bounds.1 { return None; }
        let first = self.media_bounds(session, clock_epoch, qpc_bounds.0)?.earliest.0;
        let last = self.media_bounds(session, clock_epoch, qpc_bounds.1)?.latest.0;
        let width = last.checked_sub(first)?;
        let error = width / 2 + width % 2;
        if error > max_error_ns { return None; }
        let sample_for = |ns: u64| u64::try_from(
            (u128::from(ns) * u128::from(sample_rate)).div_ceil(1_000_000_000)).ok();
        let first_sample = sample_for(first)?;
        let last_sample = sample_for(last)?;
        let middle = first.checked_add(width / 2)?;
        Some(SampleTimeEstimate { sample: sample_for(middle)?, uncertainty_ns: error,
            exact: first_sample == last_sample })
    }

    /// Return a sample-grid event only if every timestamp in the uncertainty
    /// interval maps to the same ceiling-aligned sample. Ambiguous bounds are
    /// deliberately unavailable instead of silently rounding a midpoint.
    pub fn exact_sample(&self, session: u64, clock_epoch: u64, qpc_ns: u64, sample_rate: u32) -> Option<u64> {
        if !(8000..=192000).contains(&sample_rate) { return None; }
        let bounds = self.media_bounds(session, clock_epoch, qpc_ns)?;
        let convert = |ns: u64| u64::try_from(
            (u128::from(ns) * u128::from(sample_rate)).div_ceil(1_000_000_000)).ok();
        let first = convert(bounds.earliest.0)?;
        (convert(bounds.latest.0)? == first).then_some(first)
    }
}
