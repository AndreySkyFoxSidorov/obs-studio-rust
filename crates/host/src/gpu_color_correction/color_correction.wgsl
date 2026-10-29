// OBS Color Correction v2 SDR equations, GPL-2.0-or-later.
// Coefficients/byte-ingress quantization are prepared by the unchanged CPU math.
struct Pixels { values: array<u32>, }
struct DecodeBytes { values: array<f32, 256>, }
struct LinearIngress { values: array<u32, 65536>, }
struct Parameters {
    row0: vec4<f32>, row1: vec4<f32>, row2: vec4<f32>, row3: vec4<f32>,
    curve: vec4<f32>,
    dimensions: vec4<u32>, // pixel count, input policy, width, height
}
@group(0) @binding(0) var<storage, read> source_pixels: Pixels;
@group(0) @binding(1) var<storage, read_write> target_pixels: Pixels;
@group(0) @binding(2) var<uniform> parameters: Parameters;
@group(0) @binding(3) var<storage, read> decode_bytes: DecodeBytes;
@group(0) @binding(4) var<storage, read> linear_ingress: LinearIngress;

fn to_byte(value: f32) -> u32 {
    return u32(floor(clamp(value, 0.0, 1.0) * 255.0 + 0.5));
}
fn encode_byte(linear: f32) -> u32 {
    let value = clamp(linear, 0.0, 1.0);
    if value <= 0.0031308 { return to_byte(12.92 * value); }
    return to_byte(1.055 * pow(value, 1.0 / 2.4) - 0.055);
}
fn corrected_channel(channel: u32, alpha_byte: u32, alpha: f32) -> f32 {
    var premultiplied = decode_bytes.values[channel];
    if parameters.dimensions.y == 0u && alpha_byte != 255u {
        premultiplied = decode_bytes.values[linear_ingress.values[alpha_byte * 256u + channel]];
    } else if parameters.dimensions.y == 1u {
        premultiplied = decode_bytes.values[(channel * alpha_byte + 127u) / 255u];
    }
    let straight = premultiplied / alpha;
    if parameters.curve.x == 1.0 { return straight; }
    if straight == 0.0 { return 0.0; }
    return pow(straight, parameters.curve.x);
}
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) invocation: vec3<u32>) {
    let index = invocation.x;
    if index >= parameters.dimensions.x { return; }
    let raw = source_pixels.values[index];
    let alpha_byte = raw >> 24u;
    let alpha = f32(alpha_byte) / 255.0;
    var value = vec4<f32>(0.0, 0.0, 0.0, alpha);
    if alpha > 0.0 {
        value.x = corrected_channel(raw & 255u, alpha_byte, alpha);
        value.y = corrected_channel((raw >> 8u) & 255u, alpha_byte, alpha);
        value.z = corrected_channel((raw >> 16u) & 255u, alpha_byte, alpha);
    }
    var transformed = value.x * parameters.row0;
    transformed = transformed + value.y * parameters.row1;
    transformed = transformed + value.z * parameters.row2;
    transformed = transformed + value.w * parameters.row3;
    // RGB remains independent of the rounded output-alpha byte. In particular
    // nonzero PM emission at output alpha0 must survive readback.
    target_pixels.values[index] = encode_byte(transformed.x * transformed.w)
        | (encode_byte(transformed.y * transformed.w) << 8u)
        | (encode_byte(transformed.z * transformed.w) << 16u)
        | (to_byte(transformed.w) << 24u);
}
