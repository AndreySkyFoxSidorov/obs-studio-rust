use super::{AudioFormat, MediaError};
const FL: usize = 0;
const FR: usize = 1;
const FC: usize = 2;
const BL: usize = 4;
const BR: usize = 5;
const BC: usize = 8;
const SL: usize = 9;
const SR: usize = 10;
fn bit(channel: usize) -> u32 {
    1 << channel
}
fn has(mask: u32, channel: usize) -> bool {
    mask & bit(channel) != 0
}
fn normalize(format: AudioFormat) -> u32 {
    match (format.channel_mask(), format.channels()) {
        (0, 1) => 4,
        (0, 2) => 3,
        (mask, _) => mask,
    }
}
fn sane(mask: u32) -> bool {
    let pairs = [(FL, FR), (BL, BR), (SL, SR)];
    mask & !0x73f == 0
        && (has(mask, FC) || has(mask, FL))
        && pairs
            .into_iter()
            .all(|(left, right)| has(mask, left) == has(mask, right))
}
fn roles(mask: u32) -> ([usize; 8], usize) {
    let mut roles = [0; 8];
    let mut count = 0;
    for role in 0..11 {
        if has(mask, role) {
            roles[count] = role;
            count += 1;
        }
    }
    (roles, count)
}
pub(super) fn build(input: AudioFormat, output: AudioFormat) -> Result<[[f64; 8]; 8], MediaError> {
    let mut result = [[0.0; 8]; 8];
    let input_mask = normalize(input);
    let output_mask = normalize(output);
    if input.channels() == output.channels() && input_mask == output_mask {
        // This includes explicitly unknown→unknown same-count identity. It
        // does not assign those channels any invented speaker roles.
        for (channel, row) in result
            .iter_mut()
            .enumerate()
            .take(usize::from(input.channels()))
        {
            row[channel] = 1.0;
        }
        return Ok(result);
    }
    if input_mask == 0 || output_mask == 0 || !sane(input_mask) || !sane(output_mask) {
        return Err(MediaError::InvalidAudio);
    }
    let (input_roles, input_count) = roles(input_mask);
    let (output_roles, output_count) = roles(output_mask);
    let input_roles = &input_roles[..input_count];
    let output_roles = &output_roles[..output_count];
    if input_mask == bit(FC) {
        // OBS's explicit mono matrix overrides libswresample's sqrt(.5)
        // expansion. The six-channel row really does feed LFE at unity.
        let muted_index = match output.channels() {
            3 => Some(2),
            5 | 7 | 8 => Some(3),
            _ => None,
        };
        for (index, row) in result.iter_mut().enumerate().take(output_count) {
            row[0] = if Some(index) == muted_index { 0.0 } else { 1.0 };
        }
        return Ok(result);
    }
    let q = std::f64::consts::FRAC_1_SQRT_2;
    // FFmpeg's AV_OPT_TYPE_FLOAT options store center/surround coefficients
    // as f32 before widening them to the double-precision matrix builder.
    let center = f64::from(std::f32::consts::FRAC_1_SQRT_2);
    let surround = center;
    let h = surround * q;
    let mut matrix = [[0.0; 11]; 11];
    for (role, row) in matrix.iter_mut().enumerate() {
        if has(input_mask & output_mask, role) {
            row[role] = 1.0;
        }
    }
    let missing = input_mask & !output_mask;
    if has(missing, FC) {
        let coefficient = if has(input_mask, FL) { center } else { q };
        matrix[FL][FC] += coefficient;
        matrix[FR][FC] += coefficient;
    }
    if has(missing, FL) {
        matrix[FC][FL] += q;
        matrix[FC][FR] += q;
        if has(input_mask, FC) {
            matrix[FC][FC] = center * std::f64::consts::SQRT_2;
        }
    }
    if has(missing, BC) {
        if has(output_mask, BL) {
            matrix[BL][BC] += q;
            matrix[BR][BC] += q;
        } else if has(output_mask, SL) {
            matrix[SL][BC] += q;
            matrix[SR][BC] += q;
        } else if has(output_mask, FL) {
            matrix[FL][BC] += h;
            matrix[FR][BC] += h;
        } else {
            matrix[FC][BC] += h;
        }
    }
    if has(missing, BL) {
        if has(output_mask, BC) {
            matrix[BC][BL] += q;
            matrix[BC][BR] += q;
        } else if has(output_mask, SL) {
            let coefficient = if has(input_mask, SL) { q } else { 1.0 };
            matrix[SL][BL] += coefficient;
            matrix[SR][BR] += coefficient;
        } else if has(output_mask, FL) {
            matrix[FL][BL] += surround;
            matrix[FR][BR] += surround;
        } else {
            matrix[FC][BL] += h;
            matrix[FC][BR] += h;
        }
    }
    if has(missing, SL) {
        if has(output_mask, BL) {
            let coefficient = if has(input_mask, BL) { q } else { 1.0 };
            matrix[BL][SL] += coefficient;
            matrix[BR][SR] += coefficient;
        } else if has(output_mask, BC) {
            matrix[BC][SL] += q;
            matrix[BC][SR] += q;
        } else if has(output_mask, FL) {
            matrix[FL][SL] += surround;
            matrix[FR][SR] += surround;
        } else {
            matrix[FC][SL] += h;
            matrix[FC][SR] += h;
        }
    }
    // Missing LFE is deliberately silent: FFmpeg's default lfe_mix_level=0.
    // Floating output has maxval=INT_MAX, so no peak normalization is applied.
    for (out_index, out_role) in output_roles.iter().enumerate() {
        for (in_index, in_role) in input_roles.iter().enumerate() {
            result[out_index][in_index] = matrix[*out_role][*in_role];
        }
    }
    Ok(result)
}
