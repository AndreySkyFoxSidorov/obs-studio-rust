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
// Sharpen equations from OBS32.2.2 sharpen_filter.effect/libretro common-shaders.
// Parameters.key.x = sharpness. Source is immutable; all nine taps clamp to edge.
fn sampled_at(x:i32,y:i32)->vec4<f32> {
    let px=u32(clamp(x,0,i32(parameters.dimensions.z)-1));
    let py=u32(clamp(y,0,i32(parameters.dimensions.w)-1));
    let raw=source_pixels.values[py*parameters.dimensions.z+px];
    return vec4<f32>(sampled_rgb(raw),f32(raw>>24u)/255.0);
}
fn pack_lp(color:vec4<f32>)->u32 {
    // Alpha-zero RGB energy is valid for the typed premultiplied-linear result.
    return encode_byte(color.x)|(encode_byte(color.y)<<8u)|(encode_byte(color.z)<<16u)|(to_byte(color.w)<<24u);
}
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) invocation:vec3<u32>) {
    let index=invocation.x;if index>=parameters.dimensions.x {return;}
    let x=i32(index%parameters.dimensions.z);let y=i32(index/parameters.dimensions.z);
    let e=sampled_at(x,y);
    if parameters.key.x==0.0 {target_pixels.values[index]=pack_lp(e);return;}
    let b=sampled_at(x,y-1);let d=sampled_at(x-1,y);let f=sampled_at(x+1,y);let h=sampled_at(x,y+1);
    if !((any(e!=f)&&any(e!=d))||(any(e!=b)&&any(e!=h))) {target_pixels.values[index]=pack_lp(e);return;}
    let a=sampled_at(x-1,y-1);let c=sampled_at(x+1,y-1);let g=sampled_at(x-1,y+1);let last=sampled_at(x+1,y+1);
    var color=8.0*e;color-=a;color-=b;color-=c;color-=d;color-=f;color-=g;color-=h;color-=last;
    let result=clamp(e+color*parameters.key.x,vec4<f32>(0.0),vec4<f32>(1.0));
    target_pixels.values[index]=pack_lp(result);
}
