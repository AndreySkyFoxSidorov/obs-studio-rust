//! Exact transfer values for ordered pairs of sampled RGBA8 texture bytes.
//! The process-lifetime payload is fixed at256KiB; no authored or native state.
use super::{MediaError, decode, nonlinear};
use std::sync::{Mutex, OnceLock};

pub(super) const PAIR_ENTRIES: usize = 256 * 256;
pub(super) const PAIR_BYTES: usize = PAIR_ENTRIES * std::mem::size_of::<f32>();
pub(super) struct Lookup {
    values: Box<[f32]>,
}
impl Lookup {
    fn build() -> Result<Self, MediaError> {
        let mut values = Vec::new();
        values
            .try_reserve_exact(PAIR_ENTRIES)
            .map_err(|_| MediaError::BudgetExceeded)?;
        if values
            .capacity()
            .checked_mul(std::mem::size_of::<f32>())
            .is_none_or(|bytes| bytes > PAIR_BYTES)
        {
            return Err(MediaError::BudgetExceeded);
        }
        for left in 0..=255u8 {
            let a = decode(left);
            for right in 0..=255u8 {
                values.push(nonlinear((a + decode(right)) * 0.5));
            }
        }
        Ok(Self {
            values: values.into_boxed_slice(),
        })
    }
    #[inline]
    pub(super) fn pair(&self, left: u8, right: u8) -> f32 {
        self.values[(usize::from(left) << 8) | usize::from(right)]
    }
    #[inline]
    pub(super) fn opaque(&self, byte: u8) -> f32 {
        // All decode outputs are zero or normal values in(0,1], making the
        // diagonal's exact (D+D)*0.5 identical to the original opaque D*1.
        self.pair(byte, byte)
    }
}
pub(super) fn get() -> Result<&'static Lookup, MediaError> {
    static TABLE: OnceLock<Lookup> = OnceLock::new();
    static INITIALIZE: Mutex<()> = Mutex::new(());
    if let Some(table) = TABLE.get() {
        return Ok(table);
    }
    // Serial construction bounds transient payload to one candidate. The lock
    // protects no mutable published data: interrupted construction drops its
    // local Vec, so recovering a poisoned unit guard can safely retry.
    let _guard = INITIALIZE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(table) = TABLE.get() {
        return Ok(table);
    }
    let table = Lookup::build()?;
    TABLE.set(table).map_err(|_| MediaError::InvalidBuffer)?;
    TABLE.get().ok_or(MediaError::InvalidBuffer)
}

#[cfg(test)]
#[path = "obs_keyer_lut_tests.rs"]
mod tests;
