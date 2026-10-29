//! Packed RGBA rows and exact rolling caches. All scratch remains caller-owned.
use super::{
    ColorCorrectionInput, MediaError, PreparedKeyer, decode_table, lut, sampled_bytes_with,
};
#[path = "obs_keyer_threads.rs"]
mod threads;

const WORDS_PER_COLUMN: usize = 12;
const STRIPE_WORDS_PER_COLUMN: usize = 5;
const PARALLEL_MIN_PIXELS: usize = 1_048_576;

#[derive(Clone, Copy)]
struct Context<'a> {
    width: usize,
    height: usize,
    encoding: ColorCorrectionInput,
    prepared: PreparedKeyer,
    opaque_identity: bool,
    lookup: &'a lut::Lookup,
    decoded: &'a [f32; 256],
}
impl Context<'_> {
    #[inline]
    fn pair(self, a: u32, b: u32) -> f32 {
        let a = a.to_le_bytes();
        let b = b.to_le_bytes();
        self.prepared.distance_nonlinear([
            self.lookup.pair(a[0], b[0]),
            self.lookup.pair(a[1], b[1]),
            self.lookup.pair(a[2], b[2]),
        ])
    }
    #[inline]
    fn sample(self, pixel: &[u8]) -> u32 {
        u32::from_le_bytes(sampled_bytes_with(
            [pixel[0], pixel[1], pixel[2], pixel[3]],
            self.encoding,
            self.decoded,
        ))
    }
    fn load(self, pixels: &[u8], y: usize, below: Option<&[u32]>, row: &mut [u32]) {
        if y >= self.height {
            if let Some(below) = below {
                row.copy_from_slice(below);
                return;
            }
        }
        let y = y.min(self.height - 1);
        let stride = self.width * 4;
        self.load_pixels(&pixels[y * stride..(y + 1) * stride], row);
    }
    /// Premultiplied RGBA is already the sampled byte representation. Select
    /// this once per row instead of rechecking encoding and alpha per pixel.
    /// This also applies to stripe halos and uses no additional scratch.
    #[inline]
    fn load_pixels(self, pixels: &[u8], row: &mut [u32]) {
        if self.encoding == ColorCorrectionInput::PremultipliedLinearSrgb {
            for (output, pixel) in row.iter_mut().zip(pixels.chunks_exact(4)) {
                *output = u32::from_le_bytes([pixel[0], pixel[1], pixel[2], pixel[3]]);
            }
        } else {
            for (output, pixel) in row.iter_mut().zip(pixels.chunks_exact(4)) {
                // Straight-alpha input is premultiplied when sampled into the
                // keyer's RGBA8 target. At zero alpha all three RGB channels
                // quantize to zero; fully opaque samples are unchanged. Avoid
                // transfer functions and multiplications in both common cases.
                *output = match pixel[3] {
                    0 => 0,
                    255 => u32::from_le_bytes([pixel[0], pixel[1], pixel[2], 255]),
                    _ => self.sample(pixel),
                };
            }
        }
    }
}

pub(super) fn apply(
    pixels: &mut [u8],
    width: usize,
    height: usize,
    encoding: ColorCorrectionInput,
    prepared: PreparedKeyer,
    workspace: &mut [u32],
) -> Result<(), MediaError> {
    let Some(context) = prepare(pixels, width, height, encoding, prepared, workspace)? else {
        return Ok(());
    };
    let execution = if height >= 2 && pixels.len() / 4 >= PARALLEL_MIN_PIXELS {
        threads::try_acquire().map(|permit| Execution::Scoped(permit, Spawn::Native))
    } else {
        None
    };
    dispatch(context, pixels, workspace, execution)
}

fn prepare(
    pixels: &mut [u8],
    width: usize,
    height: usize,
    encoding: ColorCorrectionInput,
    prepared: PreparedKeyer,
    workspace: &[u32],
) -> Result<Option<Context<'static>>, MediaError> {
    let expected = width
        .checked_mul(4)
        .and_then(|v| v.checked_mul(height))
        .ok_or(MediaError::Overflow)?;
    let words = width
        .checked_mul(WORDS_PER_COLUMN)
        .ok_or(MediaError::Overflow)?;
    if width == 0 || height == 0 || pixels.len() != expected || workspace.len() < words {
        return Err(MediaError::InvalidBuffer);
    }
    if prepared.opacity == 0.0 {
        pixels.fill(0);
        return Ok(None);
    }
    Ok(Some(Context {
        width,
        height,
        encoding,
        prepared,
        opaque_identity: prepared.identity_curve(),
        lookup: lut::get()?,
        decoded: decode_table(),
    }))
}

fn dispatch(
    context: Context<'_>,
    pixels: &mut [u8],
    workspace: &mut [u32],
    execution: Option<Execution<'_>>,
) -> Result<(), MediaError> {
    if let Some(execution) = execution {
        match two_stripes(context, pixels, workspace, execution)? {
            SplitOutcome::Complete => return Ok(()),
            SplitOutcome::NotStarted => {}
        }
    }
    stripe(context, pixels, workspace, None, None);
    Ok(())
}

fn stripe(
    context: Context<'_>,
    pixels: &mut [u8],
    scratch: &mut [u32],
    above: Option<&[u32]>,
    below: Option<&[u32]>,
) {
    let width = context.width;
    let (rows, caches) = scratch[..width * STRIPE_WORDS_PER_COLUMN].split_at_mut(2 * width);
    let (mut current, mut next) = rows.split_at_mut(width);
    context.load(pixels, 0, below, current);
    context.load(pixels, 1, below, next);
    for x in 0..width {
        let right = (x + 1).min(width - 1);
        let horizontal = context.pair(current[x], current[right]);
        let (vertical, previous_horizontal) = if let Some(above) = above {
            (
                context.pair(above[x], current[x]),
                context.pair(above[x], above[right]),
            )
        } else {
            (context.pair(current[x], current[x]), horizontal)
        };
        caches[3 * x] = vertical.to_bits();
        caches[3 * x + 1] = previous_horizontal.to_bits();
        caches[3 * x + 2] = horizontal.to_bits();
    }
    let mut previous_horizontal = 1;
    let mut current_horizontal = 2;
    for y in 0..context.height {
        let mut next_left_pair = context.pair(next[0], next[0]);
        let mut delayed_vertical = context.pair(current[0], next[0]);
        let mut last_vertical = delayed_vertical;
        for x in 0..width {
            let left = x.saturating_sub(1);
            let right = (x + 1).min(width - 1);
            let vertical_right = if right == x || (current[right] == current[x] && next[right] == next[x]) {
                last_vertical
            } else {
                context.pair(current[right], next[right])
            };
            let bytes = current[x].to_le_bytes();
            let raw = [
                f32::from(bytes[0]),
                f32::from(bytes[1]),
                f32::from(bytes[2]),
                f32::from(bytes[3]) / 255.0,
            ];
            let result = if bytes[3] == 0 {
                [0; 4]
            } else {
                let mut distance = f32::from_bits(caches[3 * left]);
                distance += vertical_right;
                distance += next_left_pair;
                distance += f32::from_bits(caches[3 * x + previous_horizontal]);
                distance *= 2.0;
                distance += if bytes[3] == 255 && current[x] == next[x] {
                    // last_vertical is pair(current[x], next[x]). For equal
                    // opaque pixels its diagonal lookup is exactly the same
                    // as the opaque center lookup below; reuse the distance.
                    last_vertical
                } else if bytes[3] == 255 {
                    context.prepared.distance_nonlinear([
                        context.lookup.opaque(bytes[0]),
                        context.lookup.opaque(bytes[1]),
                        context.lookup.opaque(bytes[2]),
                    ])
                } else {
                    context
                        .prepared
                        .distance(super::straight(super::row_lp_with(raw, context.decoded)))
                };
                distance /= 9.0;
                context.prepared.keyed_chroma(
                    raw,
                    distance,
                    context.decoded,
                    context.opaque_identity,
                )
            };
            pixels[(y * width + x) * 4..(y * width + x + 1) * 4].copy_from_slice(&result);
            // The cached horizontal pair belongs to the current row. If both
            // input texels match the next row, it is exactly the pair that
            // would be calculated there, including for translucent pixels.
            next_left_pair = if next[x] == current[x] && next[right] == current[right] {
                f32::from_bits(caches[3 * x + current_horizontal])
            } else {
                context.pair(next[x], next[right])
            };
            caches[3 * x + previous_horizontal] = next_left_pair.to_bits();
            // Old V[x-1] is consumed for the last time at this pixel (including
            // the clamped left edge). Two registers delay its replacement.
            if x > 0 {
                caches[3 * (x - 1)] = delayed_vertical.to_bits();
            }
            delayed_vertical = last_vertical;
            last_vertical = vertical_right;
        }
        caches[3 * (width - 1)] = last_vertical.to_bits();
        if y + 1 < context.height {
            context.load(pixels, y + 2, below, current);
            std::mem::swap(&mut current, &mut next);
            std::mem::swap(&mut previous_horizontal, &mut current_horizontal);
        }
    }
}

enum SplitOutcome {
    Complete,
    NotStarted,
}
enum Spawn {
    Native,
    #[cfg(test)]
    Reject,
}
enum Execution<'a> {
    Scoped(threads::Permit<'a>, Spawn),
    #[cfg(test)]
    Sequential(bool),
}

fn two_stripes(
    context: Context<'_>,
    pixels: &mut [u8],
    workspace: &mut [u32],
    execution: Execution<'_>,
) -> Result<SplitOutcome, MediaError> {
    let width = context.width;
    let boundary = context.height / 2;
    let stride = width * 4;
    let (halos, scratch) = workspace[..width * WORDS_PER_COLUMN].split_at_mut(2 * width);
    // Copy/sample both original boundary rows before pixels are partitioned or
    // either stripe can overwrite a byte. Shared halos are immutable thereafter.
    for (row, y) in halos.chunks_exact_mut(width).zip([boundary - 1, boundary]) {
        context.load_pixels(&pixels[y * stride..(y + 1) * stride], row);
    }
    let (above_lower, below_upper) = halos.split_at(width);
    let (upper_scratch, lower_scratch) = scratch.split_at_mut(width * STRIPE_WORDS_PER_COLUMN);
    let (upper, lower) = pixels.split_at_mut(boundary * stride);
    let upper_context = Context {
        height: boundary,
        ..context
    };
    let lower_context = Context {
        height: context.height - boundary,
        ..context
    };
    match execution {
        #[cfg(test)]
        Execution::Sequential(lower_first) => {
            if lower_first {
                stripe(lower_context, lower, lower_scratch, Some(above_lower), None);
                stripe(upper_context, upper, upper_scratch, None, Some(below_upper));
            } else {
                stripe(upper_context, upper, upper_scratch, None, Some(below_upper));
                stripe(lower_context, lower, lower_scratch, Some(above_lower), None);
            }
            Ok(SplitOutcome::Complete)
        }
        Execution::Scoped(_permit, spawn) => std::thread::scope(|scope| {
            // No pixel rewrite occurs until native spawn succeeded. Failure
            // releases all disjoint borrows on scope return, then caller retries
            // the whole frame serially using the same private pixel buffer.
            let child = match spawn {
                Spawn::Native => std::thread::Builder::new()
                    .name("SkyLiveChroma".into())
                    .stack_size(threads::STACK_BYTES)
                    .spawn_scoped(scope, move || {
                        stripe(lower_context, lower, lower_scratch, Some(above_lower), None);
                    })
                    .map_err(|_| ()),
                #[cfg(test)]
                Spawn::Reject => Err(()),
            };
            let Ok(child) = child else {
                return Ok(SplitOutcome::NotStarted);
            };
            stripe(upper_context, upper, upper_scratch, None, Some(below_upper));
            child.join().map_err(|_| MediaError::InvalidBuffer)?;
            Ok(SplitOutcome::Complete)
        }),
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(super) enum TestMode {
    Serial,
    SequentialStripes,
    ReverseStripes,
    Scoped,
    Busy,
    SpawnFailure,
}
#[cfg(test)]
pub(super) fn test_apply(
    pixels: &mut [u8],
    width: usize,
    height: usize,
    encoding: ColorCorrectionInput,
    prepared: PreparedKeyer,
    workspace: &mut [u32],
    mode: TestMode,
) -> Result<(), MediaError> {
    // Tests exercise thin/odd geometry below the production parallel threshold.
    // Keep the same validation/table preparation before invoking private modes.
    let Some(context) = prepare(pixels, width, height, encoding, prepared, workspace)? else {
        return Ok(());
    };
    match mode {
        TestMode::Serial => {}
        TestMode::Busy => {
            let admission = threads::Admission::new();
            let _first = admission.try_acquire().unwrap();
            let _second = admission.try_acquire().unwrap();
            let execution = admission
                .try_acquire()
                .map(|permit| Execution::Scoped(permit, Spawn::Native));
            assert!(execution.is_none());
            return dispatch(context, pixels, workspace, execution);
        }
        TestMode::SequentialStripes | TestMode::ReverseStripes if height >= 2 => {
            return dispatch(
                context,
                pixels,
                workspace,
                Some(Execution::Sequential(matches!(
                    mode,
                    TestMode::ReverseStripes
                ))),
            );
        }
        TestMode::Scoped | TestMode::SpawnFailure if height >= 2 => {
            // Real scoped tests use the same process-global admission as
            // production; no test bypass can create an unadmitted helper.
            let permit = threads::try_acquire().ok_or(MediaError::BudgetExceeded)?;
            let spawn = if matches!(mode, TestMode::SpawnFailure) {
                Spawn::Reject
            } else {
                Spawn::Native
            };
            return dispatch(
                context,
                pixels,
                workspace,
                Some(Execution::Scoped(permit, spawn)),
            );
        }
        _ => {}
    }
    dispatch(context, pixels, workspace, None)
}
