//! Deterministic profile video ticks for OBS's manual torque integrator.
use super::{MediaTime, StudioSnapshot, TransitionError};

const MAX_STEPS: usize = 8192;

#[derive(Clone, Debug)]
pub(super) struct ManualClock {
    pub current: f32,
    pub target: f32,
    anchor: MediaTime,
    index: u128,
    numerator: u32,
    denominator: u32,
}
impl ManualClock {
    pub fn new(current: &StudioSnapshot, now: MediaTime) -> Result<Self, TransitionError> {
        let (numerator, denominator) = rate(current)?;
        Ok(Self {
            current: 0.0,
            target: 0.0,
            anchor: now,
            index: 0,
            numerator,
            denominator,
        })
    }
    fn offset(&self, index: u128) -> u128 {
        index * u128::from(self.denominator) * 1_000_000_000 / u128::from(self.numerator)
    }
    fn due(&self, now: MediaTime) -> u128 {
        ((u128::from(now.0 - self.anchor.0) + 1) * u128::from(self.numerator) - 1)
            / (u128::from(self.denominator) * 1_000_000_000)
    }
    pub fn advance(
        &mut self,
        current: &StudioSnapshot,
        previous: MediaTime,
        now: MediaTime,
    ) -> Result<(), TransitionError> {
        let (numerator, denominator) = rate(current)?;
        if (numerator, denominator) != (self.numerator, self.denominator) {
            self.anchor = previous;
            self.index = 0;
            self.numerator = numerator;
            self.denominator = denominator;
        }
        let due = self.due(now);
        let mut steps = 0;
        while self.index < due && self.current != self.target {
            if steps == MAX_STEPS {
                return Err(TransitionError::Limit(
                    "manual transition integration limit exceeded",
                ));
            }
            let next = self.index + 1;
            let delta = u64::try_from(self.offset(next) - self.offset(self.index))
                .map_err(|_| TransitionError::TimeOverflow)?;
            self.current = skylive_media::manual_torque_step(self.current, self.target, delta)
                .map_err(TransitionError::Media)?;
            self.index = next;
            steps += 1;
        }
        // At the target, arbitrarily long idle seeks consume no integration work.
        self.index = due;
        Ok(())
    }
    pub fn next_after(&self, now: MediaTime) -> Result<Option<u64>, TransitionError> {
        if self.current == self.target {
            return Ok(None);
        }
        let next = self.offset(self.due(now) + 1) + u128::from(self.anchor.0);
        let next = u64::try_from(next).map_err(|_| TransitionError::TimeOverflow)?;
        Ok(Some(
            next.checked_sub(now.0)
                .filter(|value| *value > 0)
                .ok_or(TransitionError::TimeOverflow)?,
        ))
    }
    pub fn rational(&self) -> Result<(u64, u64), TransitionError> {
        skylive_media::progress_rational(self.current).map_err(TransitionError::Media)
    }
}
fn rate(current: &StudioSnapshot) -> Result<(u32, u32), TransitionError> {
    let numerator = current.video().fps_num();
    let denominator = current.video().fps_den();
    if numerator == 0 || denominator == 0 || u64::from(numerator) > 240 * u64::from(denominator) {
        return Err(TransitionError::Limit(
            "manual transition profile rate must be positive and at most 240 fps",
        ));
    }
    Ok((numerator, denominator))
}

#[cfg(test)]
mod tests {
    use super::*;
    use skylive_core::{Command, Session};
    #[test]
    fn floor_ticks_and_chunked_seeks_use_identical_f32_steps() {
        let mut session = Session::new();
        session
            .apply(Command::SetVideo {
                width: 16,
                height: 16,
                fps_num: 30000,
                fps_den: 1001,
            })
            .unwrap();
        let current = session.snapshot();
        let mut whole = ManualClock::new(&current, MediaTime(9)).unwrap();
        whole.target = 0.5;
        let mut chunked = whole.clone();
        chunked
            .advance(&current, MediaTime(9), MediaTime(33_366_674))
            .unwrap();
        assert_eq!(chunked.current, 0.0);
        chunked
            .advance(&current, MediaTime(33_366_674), MediaTime(33_366_675))
            .unwrap();
        assert_eq!(
            chunked.current,
            skylive_media::manual_torque_step(0.0, 0.5, 33_366_666).unwrap()
        );
        chunked
            .advance(&current, MediaTime(33_366_675), MediaTime(1_000_000_009))
            .unwrap();
        whole
            .advance(&current, MediaTime(9), MediaTime(1_000_000_009))
            .unwrap();
        assert_eq!(whole.current.to_bits(), chunked.current.to_bits());
        assert_eq!(
            whole.next_after(MediaTime(1_000_000_009)).unwrap(),
            None,
            "a settled hold must stop periodic work"
        );
        whole.target = 0.25;
        assert_eq!(
            whole.next_after(MediaTime(1_000_000_009)).unwrap(),
            Some(1_000_000),
            "moving a held target resumes at the next profile tick"
        );
    }
    #[test]
    fn huge_seek_settles_with_bounded_work_and_rate_change_reanchors() {
        let mut session = Session::new();
        session
            .apply(Command::SetVideo {
                width: 16,
                height: 16,
                fps_num: 240,
                fps_den: 1,
            })
            .unwrap();
        let mut clock = ManualClock::new(&session.snapshot(), MediaTime(0)).unwrap();
        clock.target = 1.0;
        clock
            .advance(&session.snapshot(), MediaTime(0), MediaTime(u64::MAX))
            .unwrap();
        assert_eq!(clock.current, 1.0);
        assert_eq!(clock.next_after(MediaTime(u64::MAX)).unwrap(), None);
        clock.target = 0.5;
        session
            .apply(Command::SetVideo {
                width: 16,
                height: 16,
                fps_num: 30,
                fps_den: 1,
            })
            .unwrap();
        clock
            .advance(&session.snapshot(), MediaTime(100), MediaTime(33_333_432))
            .unwrap();
        assert_eq!(clock.current, 1.0);
        clock
            .advance(
                &session.snapshot(),
                MediaTime(33_333_432),
                MediaTime(33_333_433),
            )
            .unwrap();
        assert!(clock.current < 1.0);
    }
}
