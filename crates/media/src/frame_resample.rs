//! Dimension-only admission for final opaque Program/output frame resampling.
use crate::{Affine2,AffineLayer,AffineSampleRequirements,CompositeLimits,ItemBlendMode,ItemBlendingMethod,MediaError,RgbaFrame,SamplingFilter,SourceBlend,UvRect};
#[derive(Clone,Copy,Debug)]
pub struct FrameResampleRequirements {pub input_bytes:usize,pub output_bytes:usize,pub samples:AffineSampleRequirements}
#[derive(Clone,Copy,Debug)]
pub struct FrameResamplePlan {input:(u32,u32),output:(u32,u32),transform:Affine2,filter:SamplingFilter,global:bool,required:FrameResampleRequirements}
impl FrameResamplePlan {
    pub fn stretch(input:(u32,u32),output:(u32,u32),filter:SamplingFilter)->Result<Self,MediaError>{Self::new(input,output,filter,false)}
    pub fn fit(input:(u32,u32),output:(u32,u32),filter:SamplingFilter)->Result<Self,MediaError>{Self::new(input,output,filter,true)}
    fn new(input:(u32,u32),output:(u32,u32),filter:SamplingFilter,fit:bool)->Result<Self,MediaError>{
        let input_bytes=crate::image_bytes(input.0,input.1)?;let output_bytes=crate::image_bytes(output.0,output.1)?;
        let (width,height)=if fit {let factor=(f64::from(output.0)/f64::from(input.0)).min(f64::from(output.1)/f64::from(input.1));(f64::from(input.0)*factor,f64::from(input.1)*factor)}else{(f64::from(output.0),f64::from(output.1))};
        let transform=Affine2 {xx:width,xy:0.0,yx:0.0,yy:height,tx:(f64::from(output.0)-width)*0.5,ty:(f64::from(output.1)-height)*0.5};
        let plan=crate::prepare_affine(transform,UvRect::FULL,output.0,output.1)?;
        let sampler=crate::source_scale_filtering::Prepared::full_frame(filter,input,transform,&plan,if fit {crate::source_scale_filtering::Scope::SceneItem}else{crate::source_scale_filtering::Scope::GlobalOutput {width:output.0,height:output.1}})?;
        let cost=sampler.map_or(if filter==SamplingFilter::Bilinear {4}else{1},|v|v.cost);
        let (x0,y0,x1,y1)=plan.bounds;let covered=u64::from(x1-x0)*u64::from(y1-y0);
        let base=u64::from(output.0)*u64::from(output.1)+covered;
        let extra=covered.checked_mul(cost-1).ok_or(MediaError::Overflow)?;
        let required=FrameResampleRequirements {input_bytes,output_bytes,samples:AffineSampleRequirements {base_pixel_visits:base,extra_sample_visits:extra,total_sample_visits:base.checked_add(extra).ok_or(MediaError::Overflow)?}};
        Ok(Self {input,output,transform,filter,global:!fit,required})
    }
    pub fn requirements(&self)->FrameResampleRequirements{self.required}
    pub fn output_dimensions(&self)->(u32,u32){self.output}
    /// Return an output pixel centre which selects this exact input pixel.
    /// Other filters mix several texels and require a different contribution
    /// proof; callers must treat `None` as unknown, not as an invisible input.
    pub fn nearest_witness(&self, point:[f64;2])->Option<[f64;2]> {
        if self.filter!=SamplingFilter::Nearest || !point[0].is_finite() || !point[1].is_finite() {return None;}
        let (sx,sy)=(point[0].floor(),point[1].floor());
        if sx<0.0 || sy<0.0 || sx>=f64::from(self.input.0) || sy>=f64::from(self.input.1) {return None;}
        let transform=self.transform;
        let x=(transform.tx+(sx+0.5)*transform.xx/f64::from(self.input.0)).floor()+0.5;
        let y=(transform.ty+(sy+0.5)*transform.yy/f64::from(self.input.1)).floor()+0.5;
        if x<0.0 || y<0.0 || x>=f64::from(self.output.0) || y>=f64::from(self.output.1) {return None;}
        let u=(x-transform.tx)/transform.xx;let v=(y-transform.ty)/transform.yy;
        if !(0.0..1.0).contains(&u) || !(0.0..1.0).contains(&v) {return None;}
        let fx=u*f64::from(self.input.0);let fy=v*f64::from(self.input.1);
        // Reject coordinates near a texel boundary rather than assume the
        // proof and rasterizer round floating point operations identically.
        if fx.fract()<1e-9 || fy.fract()<1e-9 || fx.fract()>1.0-1e-9 || fy.fract()>1.0-1e-9 {return None;}
        if fx.floor()!=sx || fy.floor()!=sy {return None;}
        Some([x,y])
    }
    /// Preserve a witness through an exact single-texel reconstruction only.
    /// A mixed kernel needs contribution magnitudes, which a coordinate alone
    /// does not prove. In particular Lanczos may have negative/nonzero lobes.
    pub fn exact_witness(&self,point:[f64;2])->Option<[f64;2]> {
        if self.filter==SamplingFilter::Nearest{return self.nearest_witness(point);}
        if !point[0].is_finite()||!point[1].is_finite(){return None;}
        let (sx,sy)=(point[0].floor() as i64,point[1].floor() as i64);
        if sx<0||sy<0||sx>=i64::from(self.input.0)||sy>=i64::from(self.input.1){return None;}
        let t=self.transform;
        let x=(t.tx+(sx as f64+0.5)*t.xx/f64::from(self.input.0)).floor()+0.5;
        let y=(t.ty+(sy as f64+0.5)*t.yy/f64::from(self.input.1)).floor()+0.5;
        if x<0.0||y<0.0||x>=f64::from(self.output.0)||y>=f64::from(self.output.1){return None;}
        let plan=crate::prepare_affine(t,UvRect::FULL,self.output.0,self.output.1).ok()?;
        let (u,v)=plan.local(x-t.tx,y-t.ty);
        if !(0.0..1.0).contains(&u)||!(0.0..1.0).contains(&v){return None;}
        let scope=if self.global {crate::source_scale_filtering::Scope::GlobalOutput{width:self.output.0,height:self.output.1}}else{crate::source_scale_filtering::Scope::SceneItem};
        let sampler=crate::source_scale_filtering::Prepared::full_frame(self.filter,self.input,t,&plan,scope).ok()?;
        let exact=match sampler {
            Some(sampler)=>sampler.exact_texel(u,v,sx,sy),
            // Scene-item bilinear uses the default scalar sampler.
            None if self.filter==SamplingFilter::Bilinear=>{
                (u*f64::from(self.input.0)-0.5).clamp(0.0,f64::from(self.input.0-1))==sx as f64
                    && (v*f64::from(self.input.1)-0.5).clamp(0.0,f64::from(self.input.1-1))==sy as f64
            },
            None=>false,
        };
        exact.then_some([x,y])
    }
    pub fn apply(&self,input:&RgbaFrame,blend:SourceBlend,limits:CompositeLimits)->Result<RgbaFrame,MediaError>{
        if (input.width(),input.height())!=self.input{return Err(MediaError::InvalidBuffer);}
        let mut layer=AffineLayer::new(input,self.transform,UvRect::FULL,1.0).with_blend_mode(blend)
            .with_item_compositing(ItemBlendMode::Normal,ItemBlendingMethod::Default,self.filter);
        if self.global {layer=layer.with_global_output_sampling(self.output.0,self.output.1);}
        crate::composite_affine(self.output.0,self.output.1,[0,0,0,255],&[layer],limits)
    }
}
