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
    // For an opaque texel all ingress policies address the same bytes:
    // (channel*255+127)/255 == channel for channel in 0..255.
    if a==255u {
        return vec3<f32>(decode_bytes.values[raw&255u],decode_bytes.values[(raw>>8u)&255u],decode_bytes.values[(raw>>16u)&255u]);
    }
    return vec3<f32>(sampled_channel(raw&255u,a),sampled_channel((raw>>8u)&255u,a),sampled_channel((raw>>16u)&255u,a));
}
fn straight(lp:vec3<f32>,alpha:f32)->vec3<f32> {
    if alpha>0.0 {let reciprocal=1.0/alpha;return lp*reciprocal;}
    return vec3<f32>(0.0);
}
fn finish(rgb:vec3<f32>,alpha:f32)->u32 {
    if alpha==0.0 {return 0u;}
    var c=rgb;
    if parameters.curve.w!=1.0 {
        c=vec3<f32>(pow(c.x,parameters.curve.w),pow(c.y,parameters.curve.w),pow(c.z,parameters.curve.w));
    }
    c=(c*parameters.curve.y+vec3<f32>(parameters.curve.z))*alpha;
    return encode_byte(c.x)|(encode_byte(c.y)<<8u)|(encode_byte(c.z)<<16u)|(to_byte(alpha)<<24u);
}
fn source_chroma(c:vec3<f32>)->vec2<f32> {
    // Keep the source's left-associative expression, not the key-uniform grouping.
    return vec2<f32>(((c.x*-0.100644+c.y*-0.338572)+c.z*0.439216)+0.501961,
        ((c.x*0.439216+c.y*-0.398942)+c.z*-0.040274)+0.501961);
}
fn distance_encoded(c:vec3<f32>)->f32 {
    let d=source_chroma(c)-parameters.chroma.xy;return sqrt(d.x*d.x+d.y*d.y);
}
fn clamped_raw(x:i32,y:i32)->u32 {
    let px=u32(clamp(x,0,i32(parameters.dimensions.z)-1));
    let py=u32(clamp(y,0,i32(parameters.dimensions.w)-1));
    return source_pixels.values[py*parameters.dimensions.z+px];
}
fn pair_distance(ax:i32,ay:i32,bx:i32,by:i32)->f32 {
    let a=sampled_rgb(clamped_raw(ax,ay));let b=sampled_rgb(clamped_raw(bx,by));
    // OBS linear/clamp sampler at exact half-texel positions. Neighbors sample
    // premultiplied texture RGB; they are NOT divided by neighbor alpha.
    return distance_encoded(nonlinear_rgb((a+b)*0.5));
}
fn shape(v:f32)->f32 {if v==0.0 {return 0.0;}if v==1.0 {return 1.0;}return pow(v,1.5);}
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) invocation:vec3<u32>) {
    let index=invocation.x;if index>=parameters.dimensions.x {return;}
    let raw=source_pixels.values[index];let alpha_byte=raw>>24u;let alpha=f32(alpha_byte)/255.0;
    if alpha==0.0 || parameters.curve.x==0.0 {target_pixels.values[index]=0u;return;}
    let x=i32(index%parameters.dimensions.z);let y=i32(index/parameters.dimensions.z);
    let rgb=straight(sampled_rgb(raw),alpha);
    // Exact original stagger: (-1,-.5),(+1,+.5),(-.5,+1),(+.5,-1).
    var distance=pair_distance(x-1,y-1,x-1,y);
    distance=distance+pair_distance(x+1,y,x+1,y+1);
    distance=distance+pair_distance(x-1,y+1,x,y+1);
    distance=distance+pair_distance(x,y-1,x+1,y-1);
    distance=distance*2.0;
    distance=distance+distance_encoded(nonlinear_rgb(rgb));
    distance=distance/9.0;
    let base=distance-parameters.chroma.z;
    if base<=0.0 {target_pixels.values[index]=0u;return;}
    let mask=shape(clamp(base/parameters.chroma.w,0.0,1.0));
    let spill=shape(clamp(base/parameters.spill.x,0.0,1.0));
    if alpha_byte==255u && mask==1.0 && spill==1.0 && parameters.curve.x==1.0
        && parameters.curve.y==1.0 && parameters.curve.z==0.0 && parameters.curve.w==1.0 {
        target_pixels.values[index]=raw;return;
    }
    let gray=(rgb.x*0.2126+rgb.y*0.7152)+rgb.z*0.0722;
    let gray_rgb=vec3<f32>(gray);
    let suppressed=gray_rgb+(rgb-gray_rgb)*spill;
    target_pixels.values[index]=finish(suppressed,(alpha*parameters.curve.x)*mask);
}
