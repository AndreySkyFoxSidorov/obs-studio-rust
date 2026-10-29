//! OBS32 scale-filter.c SDR source target; alpha remains premultiplied linear.
use crate::{Affine2,AffineLayer,CompositeLimits,ItemBlendMode,ItemBlendingMethod,MediaError,RgbaFrame,SamplingFilter,SamplingWindow,SourceBlend,TexelRect,UvRect};
#[derive(Clone,Copy,Debug)]
pub struct ScaleFilterRequirements{pub width:u32,pub height:u32,pub input_bytes:usize,pub output_bytes:usize,pub pixel_visits:u64,pub texel_visits:u64}
pub struct ScaleFilterPlan{input:(u32,u32),output:(u32,u32),filter:SamplingFilter,transform:Affine2,sampler:Option<crate::source_scale_filtering::Prepared>,required:ScaleFilterRequirements}
impl ScaleFilterPlan {
    /// Width/height are already resolved by the canonical target owner. The
    /// optional warp factor is new_aspect/old_aspect, never a guessed canvas.
    pub fn prepare(input:(u32,u32),output:(u32,u32),filter:SamplingFilter,undistort_factor:Option<f64>)->Result<Self,MediaError>{
        if [input.0,input.1,output.0,output.1].into_iter().any(|n|n==0||n>8192)||u64::from(output.0)*u64::from(output.1)>32*1024*1024{return Err(MediaError::InvalidDimensions);}
        let input_bytes=crate::image_bytes(input.0,input.1)?;let output_bytes=crate::image_bytes(output.0,output.1)?;
        let transform=Affine2{xx:f64::from(output.0),xy:0.0,yx:0.0,yy:f64::from(output.1),tx:0.0,ty:0.0};
        let plan=crate::prepare_affine(transform,UvRect::FULL,output.0,output.1)?;
        let sampler=crate::source_scale_filtering::Prepared::source_filter(filter,input,output,transform,&plan,undistort_factor)?;
        let pixels=u64::from(output.0)*u64::from(output.1);let cost=sampler.as_ref().map_or(if filter==SamplingFilter::Bilinear{4}else{1},|p|p.cost);
        let required=ScaleFilterRequirements{width:output.0,height:output.1,input_bytes,output_bytes,pixel_visits:pixels,texel_visits:pixels.checked_mul(cost).ok_or(MediaError::Overflow)?};
        Ok(Self{input,output,filter,transform,sampler,required})
    }
    pub fn requirements(&self)->ScaleFilterRequirements{self.required}
    pub fn apply(&self,input:&RgbaFrame,ingress:SourceBlend,limits:CompositeLimits)->Result<RgbaFrame,MediaError>{
        if (input.width(),input.height())!=self.input{return Err(MediaError::InvalidDimensions);}
        let r=self.required;if limits.max_layers<1||r.input_bytes>limits.max_input_bytes||r.output_bytes>limits.max_output_bytes||r.texel_visits.checked_add(r.pixel_visits).is_none_or(|n|n>limits.max_pixel_visits){return Err(MediaError::BudgetExceeded);}
        let layer=AffineLayer::new(input,self.transform,UvRect::FULL,1.0).with_blend_mode(ingress).with_item_compositing(ItemBlendMode::Normal,ItemBlendingMethod::Default,self.filter);
        let window=SamplingWindow::Texels(TexelRect{x:0,y:0,width:input.width(),height:input.height()});
        let mut out=Vec::new();out.try_reserve_exact(r.output_bytes).map_err(|_|MediaError::BudgetExceeded)?;out.resize(r.output_bytes,0);
        for y in 0..self.output.1{for x in 0..self.output.0{
            let u=(f64::from(x)+0.5)/f64::from(self.output.0);let v=(f64::from(y)+0.5)/f64::from(self.output.1);
            let sample=match &self.sampler{Some(sampler)=>sampler.sample(&layer,u,v),None=>crate::item_compositing::sample(&layer,&window,u,v)};
            let at=(y as usize*self.output.0 as usize+x as usize)*4;
            for channel in 0..3{out[at+channel]=crate::source_color::encode(sample.rgb[channel]);}
            out[at+3]=(sample.alpha.clamp(0.0,1.0)*255.0).round() as u8;
        }}
        RgbaFrame::new(self.output.0,self.output.1,out)
    }
}
