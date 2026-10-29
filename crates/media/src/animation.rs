//! Immutable, bounded full-canvas frames and direct elapsed-time sampling.
//!
//! Timing follows the supplied OBS image-file.c get_time/calculate_new_frame
//! endpoint policy: a frame remains selected at elapsed == its end; transition
//! occurs one nanosecond later. Duration normalization (e.g. GIF zero ->100ms)
//! belongs to the decoder, never this UI-independent clip.
use crate::{MediaError, MediaTime, RgbaFrame};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct AnimationFrame {
    pub image: Arc<RgbaFrame>,
    pub duration_ns: u64,
}

/// Finite counts TOTAL cycles, not repeats after the initial cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimationLoop {
    Infinite,
    Finite(u32),
}

#[derive(Debug, Clone, Copy)]
pub struct AnimationLimits {
    pub max_frames: usize,
    pub max_bytes: usize,
}
impl Default for AnimationLimits {
    fn default() -> Self {
        Self {
            max_frames: 512,
            max_bytes: 128 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AnimationSample {
    pub image: Arc<RgbaFrame>,
    pub index: usize,
    /// Delay until the next frame/finished-state boundary. None for a static or
    /// finished clip, or when the absolute boundary exceeds MediaTime's u64.
    pub next_after_ns: Option<u64>,
    pub finished: bool,
}

/// Frames and duration indexes cannot mutate after validation. Each sample
/// clones one Arc and uses binary search; elapsed time never controls loop work.
#[derive(Debug)]
pub struct AnimationClip {
    frames: Box<[AnimationFrame]>,
    ends_ns: Box<[u64]>,
    looping: AnimationLoop,
    cycle_ns: u64,
    bytes: usize,
}

impl AnimationClip {
    pub fn new(
        frames: Vec<AnimationFrame>,
        looping: AnimationLoop,
        limits: AnimationLimits,
    ) -> Result<Self, MediaError> {
        if frames.is_empty() || looping == AnimationLoop::Finite(0) {
            return Err(MediaError::InvalidAnimation);
        }
        if frames.len() > limits.max_frames {
            return Err(MediaError::BudgetExceeded);
        }
        let width = frames[0].image.width();
        let height = frames[0].image.height();
        let mut ends_ns = Vec::with_capacity(frames.len());
        let mut bytes = 0usize;
        let mut cycle_ns = 0u64;
        for frame in &frames {
            if frame.image.width() != width
                || frame.image.height() != height
                || frame.duration_ns == 0
            {
                return Err(MediaError::InvalidAnimation);
            }
            // Conservative by design: repeat references count once per retained
            // animation frame, even if the caller reused the same Arc.
            bytes = bytes
                .checked_add(frame.image.byte_len())
                .ok_or(MediaError::Overflow)?;
            if bytes > limits.max_bytes {
                return Err(MediaError::BudgetExceeded);
            }
            cycle_ns = cycle_ns
                .checked_add(frame.duration_ns)
                .ok_or(MediaError::Overflow)?;
            ends_ns.push(cycle_ns);
        }
        Ok(Self {
            frames: frames.into_boxed_slice(),
            ends_ns: ends_ns.into_boxed_slice(),
            looping,
            cycle_ns,
            bytes,
        })
    }

    /// Wrap an already validated raster without duplicating its pixels. The
    /// provider's frame/resource admission owns this infallible static case.
    pub fn static_frame(image: Arc<RgbaFrame>) -> Self {
        let bytes = image.byte_len();
        Self {
            frames: vec![AnimationFrame {
                image,
                duration_ns: 0,
            }]
            .into_boxed_slice(),
            ends_ns: vec![0].into_boxed_slice(),
            looping: AnimationLoop::Finite(1),
            cycle_ns: 0,
            bytes,
        }
    }

    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Consume freshly decoded, exclusively owned pixels without a raster copy.
    /// Output RGB is sRGB-encoded premultiplied linear light. Callers must use
    /// SourceBlend::PremultipliedLinearSrgb rather than the raw-image blend mode.
    /// Every frame is checked before any conversion; shared ownership rejects.
    pub fn into_linear_alpha(mut self)->Result<Self,MediaError>{
        if self.frames.iter_mut().any(|frame|Arc::get_mut(&mut frame.image).is_none()){
            return Err(MediaError::SharedResource);
        }
        for frame in &mut self.frames {
            let image=Arc::get_mut(&mut frame.image).ok_or(MediaError::SharedResource)?;
            crate::image_alpha::premultiply(image);
        }
        Ok(self)
    }
    pub fn byte_len(&self) -> usize {
        self.bytes
    }
    pub fn width(&self) -> u32 {
        self.frames[0].image.width()
    }
    pub fn height(&self) -> u32 {
        self.frames[0].image.height()
    }
    pub fn is_animated(&self) -> bool {
        self.frames.len() > 1
    }

    pub fn sample(&self, elapsed: MediaTime) -> AnimationSample {
        if !self.is_animated() {
            return self.at(0, None, true);
        }
        if let AnimationLoop::Finite(cycles) = self.looping {
            // u64 cycle * u32 count fits u128, even when the full clip end is
            // beyond MediaTime's representable range. Equality is NOT finished.
            let end = u128::from(self.cycle_ns) * u128::from(cycles);
            if u128::from(elapsed.0) > end {
                return self.at(self.frames.len() - 1, None, true);
            }
        }
        let (cycle, position) = if elapsed.0 == 0 {
            (0, 0)
        } else {
            (
                (elapsed.0 - 1) / self.cycle_ns,
                (elapsed.0 - 1) % self.cycle_ns + 1,
            )
        };
        // First inclusive end >= position. Positive durations guarantee the
        // index exists, including position==cycle_ns at an exact loop endpoint.
        let index = self.ends_ns.partition_point(|end| *end < position);
        let next =
            u128::from(cycle) * u128::from(self.cycle_ns) + u128::from(self.ends_ns[index]) + 1;
        let next_after_ns = u64::try_from(next)
            .ok()
            .and_then(|next| next.checked_sub(elapsed.0));
        self.at(index, next_after_ns, false)
    }

    fn at(&self, index: usize, next_after_ns: Option<u64>, finished: bool) -> AnimationSample {
        AnimationSample {
            image: self.frames[index].image.clone(),
            index,
            next_after_ns,
            finished,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(label: u8, duration_ns: u64) -> AnimationFrame {
        AnimationFrame {
            image: Arc::new(RgbaFrame::solid(1, 1, [label, 0, 0, 255]).unwrap()),
            duration_ns,
        }
    }

    fn clip(looping: AnimationLoop) -> AnimationClip {
        AnimationClip::new(
            vec![frame(1, 10), frame(2, 20), frame(3, 30)],
            looping,
            AnimationLimits::default(),
        )
        .unwrap()
    }

    #[test]
    fn inclusive_frame_and_cycle_ends_match_obs_nanosecond_policy() {
        let clip = clip(AnimationLoop::Infinite);
        for (elapsed, index, delay) in [
            (0, 0, 11),
            (10, 0, 1),
            (11, 1, 20),
            (30, 1, 1),
            (31, 2, 30),
            (60, 2, 1),
            (61, 0, 10),
            (70, 0, 1),
            (71, 1, 20),
            (120, 2, 1),
            (121, 0, 10),
        ] {
            let sample = clip.sample(MediaTime(elapsed));
            assert_eq!(
                (sample.index, sample.next_after_ns, sample.finished),
                (index, Some(delay), false),
                "elapsed {elapsed}"
            );
            assert_eq!(sample.image.pixels()[0], index as u8 + 1);
        }
    }

    #[test]
    fn finite_total_cycles_finish_only_after_the_inclusive_final_end() {
        for cycles in [1, 2, 3] {
            let clip = clip(AnimationLoop::Finite(cycles));
            let end = 60 * u64::from(cycles);
            let at_end = clip.sample(MediaTime(end));
            assert_eq!(
                (at_end.index, at_end.finished, at_end.next_after_ns),
                (2, false, Some(1))
            );
            for elapsed in [end + 1, u64::MAX] {
                let sample = clip.sample(MediaTime(elapsed));
                assert_eq!(
                    (sample.index, sample.finished, sample.next_after_ns),
                    (2, true, None)
                );
            }
        }
    }

    #[test]
    fn one_nanosecond_delays_are_not_rounded_or_clamped() {
        let clip = AnimationClip::new(
            vec![frame(1, 1), frame(2, 1)],
            AnimationLoop::Infinite,
            AnimationLimits::default(),
        )
        .unwrap();
        let expected = [0, 0, 1, 0, 1, 0, 1];
        for (elapsed, index) in expected.into_iter().enumerate() {
            let sample = clip.sample(MediaTime(elapsed as u64));
            assert_eq!(sample.index, index);
            assert!(sample.next_after_ns.is_some_and(|delay| delay >= 1));
        }
    }

    #[test]
    fn huge_elapsed_and_unrepresentable_next_endpoint_use_bounded_lookup() {
        let clip = clip(AnimationLoop::Infinite);
        let elapsed = u64::MAX;
        let phase = (elapsed - 1) % 60 + 1;
        let expected = if phase <= 10 {
            0
        } else if phase <= 30 {
            1
        } else {
            2
        };
        let sample = clip.sample(MediaTime(elapsed));
        assert_eq!(sample.index, expected);
        assert_eq!(sample.next_after_ns, None);
        assert!(!sample.finished);
        let long = AnimationClip::new(
            vec![frame(1, u64::MAX - 1), frame(2, 1)],
            AnimationLoop::Finite(u32::MAX),
            AnimationLimits::default(),
        )
        .unwrap();
        let sample = long.sample(MediaTime(u64::MAX));
        assert_eq!(
            (sample.index, sample.next_after_ns, sample.finished),
            (1, None, false)
        );
    }

    #[test]
    fn static_and_single_frame_clips_reuse_pixels_and_never_request_a_tick() {
        let image = frame(7, 1).image;
        let static_clip = AnimationClip::static_frame(image.clone());
        let single = AnimationClip::new(
            vec![AnimationFrame {
                image: image.clone(),
                duration_ns: 42,
            }],
            AnimationLoop::Infinite,
            AnimationLimits::default(),
        )
        .unwrap();
        for clip in [&static_clip, &single] {
            assert_eq!(
                (
                    clip.frame_count(),
                    clip.width(),
                    clip.height(),
                    clip.byte_len(),
                    clip.is_animated()
                ),
                (1, 1, 1, 4, false)
            );
            for elapsed in [0, 42, u64::MAX] {
                let sample = clip.sample(MediaTime(elapsed));
                assert_eq!(
                    (sample.index, sample.next_after_ns, sample.finished),
                    (0, None, true)
                );
                assert!(Arc::ptr_eq(&sample.image, &image));
            }
        }
    }

    #[test]
    fn malformed_frames_timing_and_resource_limits_fail_before_publication() {
        assert_eq!(
            AnimationClip::new(vec![], AnimationLoop::Infinite, AnimationLimits::default())
                .unwrap_err(),
            MediaError::InvalidAnimation
        );
        assert_eq!(
            AnimationClip::new(
                vec![frame(1, 1)],
                AnimationLoop::Finite(0),
                AnimationLimits::default()
            )
            .unwrap_err(),
            MediaError::InvalidAnimation
        );
        assert_eq!(
            AnimationClip::new(
                vec![frame(1, 0), frame(2, 1)],
                AnimationLoop::Infinite,
                AnimationLimits::default()
            )
            .unwrap_err(),
            MediaError::InvalidAnimation
        );
        let different = AnimationFrame {
            image: Arc::new(RgbaFrame::solid(2, 1, [0; 4]).unwrap()),
            duration_ns: 1,
        };
        assert_eq!(
            AnimationClip::new(
                vec![frame(1, 1), different],
                AnimationLoop::Infinite,
                AnimationLimits::default()
            )
            .unwrap_err(),
            MediaError::InvalidAnimation
        );
        assert_eq!(
            AnimationClip::new(
                vec![frame(1, u64::MAX), frame(2, 1)],
                AnimationLoop::Infinite,
                AnimationLimits::default()
            )
            .unwrap_err(),
            MediaError::Overflow
        );
        let limits = AnimationLimits {
            max_frames: 1,
            max_bytes: 8,
        };
        assert_eq!(
            AnimationClip::new(
                vec![frame(1, 1), frame(2, 1)],
                AnimationLoop::Infinite,
                limits
            )
            .unwrap_err(),
            MediaError::BudgetExceeded
        );
        let shared = frame(1, 1);
        let limits = AnimationLimits {
            max_frames: 2,
            max_bytes: 7,
        };
        assert_eq!(
            AnimationClip::new(
                vec![shared.clone(), shared],
                AnimationLoop::Infinite,
                limits
            )
            .unwrap_err(),
            MediaError::BudgetExceeded
        );
    }
}
