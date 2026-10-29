// SPDX-License-Identifier: GPL-2.0-or-later
// OBS32 color_grade_filter.effect SDR equations, same branches as media LUT.
// Native pixel/rounding/performance equivalence has not been executed.
struct Pixels { values: array<u32>, }
struct DecodeBytes { values: array<f32,256>, }
struct LinearIngress { values: array<u32,65536>, }
struct LutValues { values: array<f32>, }
struct Parameters {
    dimensions: vec4<u32>, // count, ingress, width, height
    mode: vec4<u32>, // size, 3D, passthrough-alpha, reserved
    lower_amount: vec4<f32>, upper: vec4<f32>,
    scale: vec4<f32>, offset: vec4<f32>,
}
@group(0) @binding(0) var<storage,read> source_pixels: Pixels;
@group(0) @binding(1) var<storage,read_write> target_pixels: Pixels;
@group(0) @binding(2) var<uniform> parameters: Parameters;
@group(0) @binding(3) var<storage,read> decode_bytes: DecodeBytes;
@group(0) @binding(4) var<storage,read> linear_ingress: LinearIngress;
@group(0) @binding(5) var<storage,read> lut: LutValues;
fn to_byte(value:f32)->u32{return u32(floor(clamp(value,0.0,1.0)*255.0+0.5));}
fn encode_byte(linear:f32)->u32{
    let value=clamp(linear,0.0,1.0);
    if value<=0.0031308{return to_byte(12.92*value);}
    return to_byte(1.055*pow(value,1.0/2.4)-0.055);
}
fn nonlinear(value:f32)->f32{
    if value<=0.0031308{return 12.92*value;}
    return 1.055*pow(value,1.0/2.4)-0.055;
}
fn sampled_channel(channel:u32,alpha:u32)->f32{
    var byte=channel;
    if parameters.dimensions.y==0u&&alpha!=255u{byte=linear_ingress.values[alpha*256u+channel];}
    else if parameters.dimensions.y==1u{byte=(channel*alpha+127u)/255u;}
    return decode_bytes.values[byte];
}
fn axis(value:f32,channel:u32)->vec3<f32>{
    let n=parameters.mode.x;
    let uv=value*parameters.scale[channel]+parameters.offset[channel];
    let point=clamp(uv*f32(n)-0.5,0.0,f32(n-1u));
    let low=floor(point);
    return vec3<f32>(low,min(low+1.0,f32(n-1u)),point-low);
}
fn channel_lookup(value:f32,channel:u32)->f32{
    let p=axis(value,channel);let a=lut.values[u32(p.x)*3u+channel];let b=lut.values[u32(p.y)*3u+channel];
    return a*(1.0-p.z)+b*p.z;
}
fn read_lut(x:u32,y:u32,z:u32)->vec3<f32>{
    let n=parameters.mode.x;let i=((z*n+y)*n+x)*3u;
    return vec3<f32>(lut.values[i],lut.values[i+1u],lut.values[i+2u]);
}
fn sample_lut(rgb:vec3<f32>)->vec3<f32>{
    let x=axis(rgb.x,0u);let y=axis(rgb.y,1u);let z=axis(rgb.z,2u);
    let a=read_lut(u32(x.x),u32(y.x),u32(z.x));let b=read_lut(u32(x.y),u32(y.x),u32(z.x));
    let c=read_lut(u32(x.x),u32(y.y),u32(z.x));let d=read_lut(u32(x.y),u32(y.y),u32(z.x));
    let e=read_lut(u32(x.x),u32(y.x),u32(z.y));let f=read_lut(u32(x.y),u32(y.x),u32(z.y));
    let g=read_lut(u32(x.x),u32(y.y),u32(z.y));let h=read_lut(u32(x.y),u32(y.y),u32(z.y));
    let near=(a*(1.0-x.z)+b*x.z)*(1.0-y.z)+(c*(1.0-x.z)+d*x.z)*y.z;
    let far=(e*(1.0-x.z)+f*x.z)*(1.0-y.z)+(g*(1.0-x.z)+h*x.z)*y.z;
    return near*(1.0-z.z)+far*z.z;
}
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) invocation:vec3<u32>){
    let index=invocation.x;if index>=parameters.dimensions.x{return;}
    let raw=source_pixels.values[index];let alpha_byte=raw>>24u;let alpha=f32(alpha_byte)/255.0;
    var inverse=0.0;if alpha>0.0{inverse=1.0/alpha;}
    var rgb=vec3<f32>(sampled_channel(raw&255u,alpha_byte),sampled_channel((raw>>8u)&255u,alpha_byte),sampled_channel((raw>>16u)&255u,alpha_byte));
    let amount=parameters.lower_amount.w;let lower=parameters.lower_amount.xyz;let upper=parameters.upper.xyz;
    let domain=any(lower>vec3<f32>(0.0))||any(upper<vec3<f32>(1.0));
    if parameters.mode.y==0u{
        // Draw1D deliberately returns straight RGB without re-premultiplying.
        rgb=rgb*inverse;
        for(var i=0u;i<3u;i=i+1u){let value=nonlinear(rgb[i]);if value>=lower[i]&&value<=upper[i]{rgb[i]=rgb[i]*(1.0-amount)+channel_lookup(value,i)*amount;}}
    }else{
        let straight=domain||amount<1.0||parameters.mode.z==0u;
        if straight{rgb=rgb*inverse;}
        let non=vec3<f32>(nonlinear(rgb.x),nonlinear(rgb.y),nonlinear(rgb.z));
        if !domain||(all(non>=lower)&&all(non<=upper)){
            let mapped=sample_lut(non);
            if domain||amount<1.0{rgb=rgb*(1.0-amount)+mapped*amount;}else{rgb=mapped;}
        }
        if straight{rgb=rgb*alpha;}
    }
    target_pixels.values[index]=encode_byte(rgb.x)|(encode_byte(rgb.y)<<8u)|(encode_byte(rgb.z)<<16u)|(raw&0xff000000u);
}
