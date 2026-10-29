// SPDX-License-Identifier: GPL-2.0-or-later
// OBS32.2.2 SDR keyer equations; no device/rounding equivalence is asserted.
// Raw input follows SourceBlend, NOT a universally premultiplied byte format.
struct Pixels { values: array<u32>, }
struct DecodeBytes { values: array<f32, 256>, }
struct LinearIngress { values: array<u32, 65536>, }
struct Parameters {
    dimensions: vec4<u32>, // count, ingress policy, width, height
    key: vec4<f32>,
    chroma: vec4<f32>, // key Cb, key Cr, similarity, smoothness
    curve: vec4<f32>, // opacity, contrast, brightness, gamma
    spill: vec4<f32>, reserved: vec4<u32>,
}
@group(0) @binding(0) var<storage, read> source_pixels: Pixels;
@group(0) @binding(1) var<storage, read_write> target_pixels: Pixels;
@group(0) @binding(2) var<uniform> parameters: Parameters;
@group(0) @binding(3) var<storage, read> decode_bytes: DecodeBytes;
@group(0) @binding(4) var<storage, read> linear_ingress: LinearIngress;
fn to_byte(value:f32)->u32 {return u32(floor(clamp(value,0.0,1.0)*255.0+0.5));}
fn encode_byte(value:f32)->u32 {
    let v=clamp(value,0.0,1.0);
    if v<=0.0031308 {return to_byte(v*12.92);}
    return to_byte(1.055*pow(v,1.0/2.4)-0.055);
}
fn nonlinear(v:f32)->f32 {
    if v<=0.0031308 {return v*12.92;}
    return 1.055*pow(v,1.0/2.4)-0.055;
}
fn nonlinear_rgb(v:vec3<f32>)->vec3<f32> {return vec3<f32>(nonlinear(v.x),nonlinear(v.y),nonlinear(v.z));}
fn sampled_channel(channel:u32,alpha:u32)->f32 {
    var byte=channel;
    if parameters.dimensions.y==0u && alpha!=255u {
        // Straight encoded RGB -> decode, alpha, encode/UNORM texture round.
        byte=linear_ingress.values[alpha*256u+channel];
    } else if parameters.dimensions.y==1u {
        // Both Encoded and EncodedPremultipliedSrgb take STRAIGHT input.
        // Round encoded premultiplication once before the sRGB texture decode.
        byte=(channel*alpha+127u)/255u;
    }
    // Policy2 is already an encoded premultiplied-linear framebuffer.
    return decode_bytes.values[byte];
}
fn sampled_rgb(raw:u32)->vec3<f32> {
    let a=raw>>24u;
    return vec3<f32>(sampled_channel(raw&255u,a),sampled_channel((raw>>8u)&255u,a),sampled_channel((raw>>16u)&255u,a));
}
// Parameters.key = lower edge, lower smooth end, upper smooth begin, upper edge.
fn hlsl_smoothstep(low:f32, high:f32, value:f32)->f32 {
    // Explicit HLSL saturate(NaN)=0 for zero-smoothing threshold equality.
    // WGSL's builtin smoothstep with equal edges is not used.
    var t:f32;
    if high==low {t=select(1.0,0.0,value<=low);} else {t=clamp((value-low)/(high-low),0.0,1.0);}
    return t*t*(3.0-2.0*t);
}
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) invocation:vec3<u32>) {
    let index=invocation.x;if index>=parameters.dimensions.x {return;}
    let raw=source_pixels.values[index];let a=f32(raw>>24u)/255.0;
    var inverse=0.0;if a>0.0 {inverse=1.0/a;}
    let rgb=sampled_rgb(raw)*inverse;
    let luminance=rgb.x*0.2126+rgb.y*0.7152+rgb.z*0.0722;
    let low=hlsl_smoothstep(parameters.key.x,parameters.key.y,luminance);
    let high=1.0-hlsl_smoothstep(parameters.key.z,parameters.key.w,luminance);
    let alpha=a*(low*high);let color=rgb*alpha;
    target_pixels.values[index]=encode_byte(color.x)|(encode_byte(color.y)<<8u)|(encode_byte(color.z)<<16u)|(to_byte(alpha)<<24u);
}
