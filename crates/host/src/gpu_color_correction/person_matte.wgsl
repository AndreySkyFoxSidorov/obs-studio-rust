// Person-matte SDR postprocess. CPU NN and CPU f64 axis preparation remain
// authoritative. GPU f32/pow rounding requires device acceptance; no bitwise
// equivalence is implied. Input bytes and SourceBlend are preserved at matte1.
struct Pixels { values: array<u32>, }
struct DecodeBytes { values: array<f32, 256>, }
struct Payload { values: array<u32>, }
struct Parameters {
    dimensions: vec4<u32>, // pixel count, PM-linear policy, width, height
    curve: vec4<f32>, // threshold, softness
    reserved0: vec4<u32>, reserved1: vec4<u32>,
    reserved2: vec4<u32>, reserved3: vec4<u32>,
}
@group(0) @binding(0) var<storage, read> source_pixels: Pixels;
@group(0) @binding(1) var<storage, read_write> target_pixels: Pixels;
@group(0) @binding(2) var<uniform> parameters: Parameters;
@group(0) @binding(3) var<storage, read> decode_bytes: DecodeBytes;
@group(0) @binding(4) var<storage, read> payload: Payload;

fn to_byte(value: f32) -> u32 {
    return u32(floor(clamp(value, 0.0, 1.0) * 255.0 + 0.5));
}
fn encode_byte(linear: f32) -> u32 {
    let value = clamp(linear, 0.0, 1.0);
    if value <= 0.0031308 { return to_byte(12.92 * value); }
    return to_byte(1.055 * pow(value, 1.0 / 2.4) - 0.055);
}
fn probability(index: u32) -> f32 { return bitcast<f32>(payload.values[index]); }
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) invocation: vec3<u32>) {
    let index = invocation.x;
    if index >= parameters.dimensions.x { return; }
    let raw = source_pixels.values[index];
    let width = parameters.dimensions.z;
    let x_axis = 65536u + (index % width) * 4u;
    let y_axis = 65536u + (width + index / width) * 4u;
    let first_x = payload.values[x_axis];
    let second_x = payload.values[x_axis + 1u];
    let weight_x = bitcast<f32>(payload.values[x_axis + 2u]);
    let first_y = payload.values[y_axis] * 256u;
    let second_y = payload.values[y_axis + 1u] * 256u;
    let weight_y = bitcast<f32>(payload.values[y_axis + 2u]);
    let a = probability(first_y + first_x);
    let b = probability(first_y + second_x);
    let c = probability(second_y + first_x);
    let d = probability(second_y + second_x);
    // Keep the CPU operation order explicit; do not substitute hardware texture
    // sampling, a different resize kernel or an approximate neural mask.
    let top = a + (b - a) * weight_x;
    let bottom = c + (d - c) * weight_x;
    let sampled = top + (bottom - top) * weight_y;
    let matte = clamp((sampled - parameters.curve.x) / parameters.curve.y + 0.5, 0.0, 1.0);
    if matte == 1.0 { target_pixels.values[index] = raw; return; }
    if matte == 0.0 {
        if parameters.dimensions.y == 1u { target_pixels.values[index] = 0u; }
        else { target_pixels.values[index] = raw & 0x00ffffffu; }
        return;
    }
    var rgb = raw & 0x00ffffffu;
    if parameters.dimensions.y == 1u {
        // RGB energy is independent of alpha, including emission at alpha0.
        rgb = encode_byte(decode_bytes.values[raw & 255u] * matte)
            | (encode_byte(decode_bytes.values[(raw >> 8u) & 255u] * matte) << 8u)
            | (encode_byte(decode_bytes.values[(raw >> 16u) & 255u] * matte) << 16u);
    }
    let alpha = to_byte((f32(raw >> 24u) / 255.0) * matte);
    target_pixels.values[index] = rgb | (alpha << 24u);
}
