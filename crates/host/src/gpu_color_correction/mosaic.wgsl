// Spatial Mosaic matching the CPU stage's aligned, alpha-weighted cell average.
// Each workgroup owns one disjoint cell; input and output are separate buffers.
struct Pixels { values: array<u32>, }
struct DecodeBytes { values: array<f32, 256>, }
struct Parameters {
    dimensions: vec4<u32>, // count, ingress policy, width, height
    key: vec4<f32>,
    unused1: vec4<f32>, unused2: vec4<f32>, unused3: vec4<f32>, unused4: vec4<u32>,
}
@group(0) @binding(0) var<storage, read> source_pixels: Pixels;
@group(0) @binding(1) var<storage, read_write> target_pixels: Pixels;
@group(0) @binding(2) var<uniform> parameters: Parameters;
@group(0) @binding(3) var<storage, read> decode_bytes: DecodeBytes;
var<workgroup> partial: array<vec4<f32>, 64>;
fn byte(v:f32)->u32 {return u32(floor(clamp(v,0.0,1.0)*255.0+0.5));}
fn encode(v:f32)->f32 {
    if v<=0.0031308 {return v*12.92;}
    return 1.055*pow(v,1.0/2.4)-0.055;
}
@compute @workgroup_size(64)
fn main(@builtin(workgroup_id) group:vec3<u32>, @builtin(local_invocation_index) lane:u32) {
    let cell=u32(parameters.key.x);
    let width=parameters.dimensions.z; let height=parameters.dimensions.w;
    if cell==1u {
        let index=group.x*64u+lane;
        if index<parameters.dimensions.x {target_pixels.values[index]=source_pixels.values[index];}
        return;
    }
    let left=group.x*cell; let top=group.y*cell;
    let cw=min(cell,width-left); let ch=min(cell,height-top); let count=cw*ch;
    var sum=vec4<f32>(0.0);
    for(var i=lane; i<count; i+=64u) {
        let x=left+i%cw; let y=top+i/cw;
        let raw=source_pixels.values[y*width+x];
        let a=f32(raw>>24u)/255.0;
        let r=raw&255u; let g=(raw>>8u)&255u; let b=(raw>>16u)&255u;
        var rgb=vec3<f32>(f32(r),f32(g),f32(b))/255.0*a;
        if parameters.dimensions.y==2u {rgb=vec3<f32>(decode_bytes.values[r],decode_bytes.values[g],decode_bytes.values[b]);}
        sum+=vec4<f32>(rgb,a);
    }
    partial[lane]=sum; workgroupBarrier();
    var stride=32u;
    loop {
        if lane<stride {partial[lane]+=partial[lane+stride];}
        workgroupBarrier();
        if stride==1u {break;}
        stride/=2u;
    }
    let total=partial[0]; let average_alpha=total.a/f32(count);
    var rgb=vec3<f32>(0.0);
    if parameters.dimensions.y==2u {
        let v=total.rgb/f32(count); rgb=vec3<f32>(encode(v.r),encode(v.g),encode(v.b));
    } else if total.a>0.0 {rgb=total.rgb/total.a;}
    let packed=byte(rgb.r)|(byte(rgb.g)<<8u)|(byte(rgb.b)<<16u)|(byte(average_alpha)<<24u);
    for(var i=lane; i<count; i+=64u) {
        let x=left+i%cw; let y=top+i/cw;
        target_pixels.values[y*width+x]=packed;
    }
}
