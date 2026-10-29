// SPDX-License-Identifier: MIT
// Copyright (c) 2026 ArtCraft Team and the EffectCraft/PhotoCraft contributors.
// Equations mirror media/craft_video.rs. No byte rounding between Craft stages.
struct Pixels {values:array<u32>,}
struct DecodeBytes {values:array<f32,256>,}
struct Payload {values:array<u32>,}
struct Parameters {dimensions:vec4<u32>,run:vec4<u32>,a:vec4<u32>,b:vec4<u32>,c:vec4<u32>,d:vec4<u32>,}
@group(0) @binding(0) var<storage,read> source_pixels:Pixels;
@group(0) @binding(1) var<storage,read_write> target_pixels:Pixels;
@group(0) @binding(2) var<uniform> parameters:Parameters;
@group(0) @binding(3) var<storage,read> decode_bytes:DecodeBytes;
@group(0) @binding(4) var<storage,read> payload:Payload;
fn value(base:u32,index:u32)->f32{return bitcast<f32>(payload.values[base+index]);}
fn to_byte(v:f32)->u32{if v!=v{return 0u;}return u32(floor(clamp(v,0.0,1.0)*255.0+0.5));}
fn decode(v:f32)->f32{if v<=0.04045{return v/12.92;}return pow((v+0.055)/1.055,2.4);}
fn encode(v:f32)->f32{if v<=0.0031308{return v*12.92;}return 1.055*pow(v,1.0/2.4)-0.055;}
fn signed_power(v:f32,e:f32)->f32{let p=pow(abs(v),e);if v<0.0{return -p;}return p;}
fn hue_saturation(c:vec3<f32>)->vec2<f32>{
    let hi=max(max(c.r,c.g),c.b);let lo=min(min(c.r,c.g),c.b);let l=(hi+lo)*0.5;
    if abs(hi-lo)<0.0000001{return vec2<f32>(0.0);}
    let d=hi-lo;var s=d/(hi+lo);if l>0.5{s=d/(2.0-hi-lo);}
    var h=0.0;
    if hi==c.r{var wrap=0.0;if c.g<c.b{wrap=6.0;}h=((c.g-c.b)/d+wrap)/6.0;}
    else if hi==c.g{h=((c.b-c.r)/d+2.0)/6.0;}
    else{h=((c.r-c.g)/d+4.0)/6.0;}
    h=h-floor(h);return vec2<f32>(h,s);
}
fn hue_to_rgb(p:f32,q:f32,input:f32)->f32{
    let t=input-floor(input);
    if t<1.0/6.0{return p+(q-p)*6.0*t;}
    if t<0.5{return q;}
    if t<2.0/3.0{return p+(q-p)*(2.0/3.0-t)*6.0;}
    return p;
}
fn apply_vibrance(c:vec3<f32>,vibrance:f32,saturation:f32)->vec3<f32>{
    // Identical stage ordering to the EffectCraft CPU path: compute HSL
    // before either control modifies saturation; alpha is not touched.
    let hi=max(max(c.r,c.g),c.b);let lo=min(min(c.r,c.g),c.b);
    let l=(hi+lo)*0.5;let hs=hue_saturation(c);
    let s=clamp(hs.y*(1.0+saturation)+vibrance*(1.0-hs.y)*min(hs.y,0.5),0.0,1.0);
    if s<=0.0{return vec3<f32>(l);}
    var q=l+s-l*s;if l<0.5{q=l*(1.0+s);}
    let p=2.0*l-q;
    return vec3<f32>(hue_to_rgb(p,q,hs.x+1.0/3.0),hue_to_rgb(p,q,hs.x),hue_to_rgb(p,q,hs.x-1.0/3.0));
}
fn chroma(c:vec3<f32>)->vec2<f32>{let s=c.r+c.g+c.b;if s<=0.000001{return vec2<f32>(1.0/3.0);}return c.rg/s;}
fn key_distance(c:vec3<f32>,key:vec3<f32>,method:u32)->f32{
    if method==0u{let d=c-key;return sqrt((d.r*d.r+d.g*d.g)+d.b*d.b)/sqrt(3.0);}
    if method==1u{let a=hue_saturation(c);let b=hue_saturation(key);if a.y<0.02{return 1.0;}let d=abs(a.x-b.x);return min(d,1.0-d)*2.0;}
    let d=chroma(c)-chroma(key);return sqrt(d.x*d.x+d.y*d.y)/sqrt(2.0);
}
fn bc_modern(input:f32,b:f32,c:f32)->f32{
    if input>=1.0{return input;}var v=max(input,0.0);
    if b!=0.0{let e=pow(2.0,1.24*abs(b));var power=e;if b<=0.0{power=1.0/e;}v=1.0-pow(1.0-v,power);}
    if c!=0.0{var g=max(1.0+0.53*abs(c),0.05);if c<=0.0{g=1.0/g;}
        if v<0.5{v=0.5*pow(2.0*v,g);}else{v=1.0-0.5*pow(max(2.0-2.0*v,0.0),g);}}
    return v;
}
fn stage(input:vec4<f32>,base:u32)->vec4<f32>{
    var c=input.rgb;var alpha=input.a;let kind=payload.values[base];let flags=payload.values[base+1u];
    let p=value(base,4u);let q=value(base,5u);
    switch kind {
        case 0u:{c=c+(vec3<f32>(1.0)-2.0*c)*p;}
        case 1u:{let y=clamp((0.299*c.r+0.587*c.g)+0.114*c.b,0.0,1.0);c=c+(vec3<f32>(y)-c)*p;}
        case 2u:{let x=floor(clamp(c,vec3<f32>(0.0),vec3<f32>(1.0))*255.0+0.5);let bin=min(floor(x*p/256.0),vec3<f32>(p-1.0));c=floor(bin*255.0/(p-1.0))/255.0;}
        case 3u:{var exponent=value(base,6u);if flags==0u{c=vec3<f32>(signed_power(c.r,2.2),signed_power(c.g,2.2),signed_power(c.b,2.2));exponent/=2.2;}
            c=c*p+vec3<f32>(q);c=clamp(vec3<f32>(signed_power(c.r,exponent),signed_power(c.g,exponent),signed_power(c.b,exponent)),vec3<f32>(0.0),vec3<f32>(1.0));}
        case 4u:{c=vec3<f32>(c.r*(1.0+0.25*p),c.g*(1.0-0.25*q),c.b*(1.0-0.25*p));}
        case 5u:{let bytes=floor(clamp(c,vec3<f32>(0.0),vec3<f32>(1.0))*255.0+0.5);let white=(30.0*bytes.r+59.0*bytes.g)+11.0*bytes.b>100.0*p-50.0;c=vec3<f32>(select(0.0,1.0,white));}
        case 6u:{let view=payload.values[base+3u];if view!=1u{
            let key=vec3<f32>(p,q,value(base,6u));let distance=key_distance(c,key,flags);let tolerance=value(base,7u);let softness=value(base,8u);
            var matte=select(0.0,1.0,distance>tolerance);if softness>0.000001{matte=clamp((distance-tolerance)/softness,0.0,1.0);}
            if payload.values[base+2u]!=0u{matte=1.0-matte;}alpha*=matte;if view==2u{c=vec3<f32>(alpha);alpha=1.0;}}}
        case 7u:{if flags!=0u{var coefficient=1.0+q;if q>=0.0{coefficient=1.0/(1.0-q*0.99);}c=max(((c+vec3<f32>(p))-vec3<f32>(0.5))*coefficient+vec3<f32>(0.5),vec3<f32>(0.0));}
            else{c=vec3<f32>(bc_modern(c.r,p,q),bc_modern(c.g,p,q),bc_modern(c.b,p,q));}}
        case 8u:{var primary=1u;if flags!=0u{primary=2u;}let a=(primary+1u)%3u;let b=(primary+2u)%3u;
            let limit=c[a]*p+c[b]*(1.0-p);c[primary]-=max(c[primary]-limit,0.0)*q;}
        case 9u:{c=apply_vibrance(c,p,q);}
        case 10u:{let l=clamp((0.299*c.r+0.587*c.g)+0.114*c.b,0.0,1.0);let black=vec3<f32>(q,value(base,6u),value(base,7u));let white=vec3<f32>(value(base,8u),value(base,9u),value(base,10u));let target=black+(white-black)*l;c=c+(target-c)*p;}
        case 11u:{let l=clamp((0.299*c.r+0.587*c.g)+0.114*c.b,0.0,1.0);let shadows=vec3<f32>(q,value(base,6u),value(base,7u));let midtones=vec3<f32>(value(base,8u),value(base,9u),value(base,10u));let highlights=vec3<f32>(value(base,11u),value(base,12u),value(base,13u));var target=shadows+(midtones-shadows)*(l*2.0);if l>=0.5{target=midtones+(highlights-midtones)*((l-0.5)*2.0);}c=c+(target-c)*p;}
        case 12u:{
            let original=c;
            let luminance=(0.2126*c.r+0.7152*c.g)+0.0722*c.b;
            let l=clamp(luminance,0.0,1.0);
            let ws=clamp(1.0-l*2.0,0.0,1.0);let wh=clamp(l*2.0-1.0,0.0,1.0);let wm=1.0-ws-wh;
            let shadows=vec3<f32>(p,q,value(base,6u));let midtones=vec3<f32>(value(base,7u),value(base,8u),value(base,9u));let highlights=vec3<f32>(value(base,10u),value(base,11u),value(base,12u));
            c=max(c+(shadows*ws+midtones*wm+highlights*wh)*0.5,vec3<f32>(0.0));
            if flags!=0u{
                let before=(0.2126*original.r+0.7152*original.g)+0.0722*original.b;
                let after=(0.2126*c.r+0.7152*c.g)+0.0722*c.b;
                c=max(c+vec3<f32>(before-after),vec3<f32>(0.0));
            }
        }
        case 13u:{
            // Keep the CPU row expression's summation order. Each output
            // is derived from the original input, including in monochrome.
            let original=c;
            let red=max(((original.r*value(base,4u)+original.g*value(base,5u))+original.b*value(base,6u))+value(base,7u),0.0);
            if flags!=0u {c=vec3<f32>(red);} else {
                let green=max(((original.r*value(base,8u)+original.g*value(base,9u))+original.b*value(base,10u))+value(base,11u),0.0);
                let blue=max(((original.r*value(base,12u)+original.g*value(base,13u))+original.b*value(base,14u))+value(base,15u),0.0);
                c=vec3<f32>(red,green,blue);
            }
        }
        case 14u:{
            // EffectCraft Photo Filter (including the fixed Sepia preset).
            // Preserve the straight-RGB operation order; alpha is unchanged.
            if p!=0.0 {
                let original=c;
                let tint=vec3<f32>(q,value(base,6u),value(base,7u));
                let filtered=c*tint;
                c=c+(filtered-c)*p;
                if flags!=0u {
                    let l0=(0.2126*original.r+0.7152*original.g)+0.0722*original.b;
                    let l1=max((0.2126*c.r+0.7152*c.g)+0.0722*c.b,0.000001);
                    c=(c*l0)/l1;
                }
            }
        }
        case 15u:{
            let gamma=vec3<f32>(q,value(base,6u),value(base,7u));
            let pedestal=vec3<f32>(value(base,8u),value(base,9u),value(base,10u));
            let gain=vec3<f32>(value(base,11u),value(base,12u),value(base,13u));
            for(var i=0u;i<3u;i+=1u){
                var v=c[i];
                if p>0.0 && v>0.0 && v<1.0 {let x=1.0-v;v+=p*0.25*v*x*x*x*x;}
                c[i]=max(pedestal[i]+(gain[i]-pedestal[i])*pow(max(v,0.0),gamma[i]),0.0);
            }
        }
        default:{}
    }
    return vec4<f32>(c,alpha);
}
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) invocation:vec3<u32>){
    let index=invocation.x;if index>=parameters.dimensions.x{return;}
    let raw=source_pixels.values[index];let alpha=f32(raw>>24u)/255.0;
    var c=vec3<f32>(f32(raw&255u),f32((raw>>8u)&255u),f32((raw>>16u)&255u))/255.0;
    if parameters.dimensions.y==1u{c=vec3<f32>(0.0);if alpha>0.0{
        c=vec3<f32>(encode(decode_bytes.values[raw&255u]/alpha),encode(decode_bytes.values[(raw>>8u)&255u]/alpha),encode(decode_bytes.values[(raw>>16u)&255u]/alpha));}}
    var color=vec4<f32>(c,alpha);
    for(var i=0u;i<parameters.run.x;i++){color=stage(color,i*16u);}
    c=color.rgb;
    if parameters.dimensions.y==1u{if color.a==0.0{c=vec3<f32>(0.0);}else{c=vec3<f32>(encode(clamp(decode(c.r)*color.a,0.0,1.0)),encode(clamp(decode(c.g)*color.a,0.0,1.0)),encode(clamp(decode(c.b)*color.a,0.0,1.0)));}}
    else if color.a==0.0{c=vec3<f32>(0.0);}
    target_pixels.values[index]=to_byte(c.r)|(to_byte(c.g)<<8u)|(to_byte(c.b)<<16u)|(to_byte(color.a)<<24u);
}
