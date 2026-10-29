//! Raw source pixels remain the scale/capture basis; enabled filters derive the displayed extent.
use crate::{Error,Source,StudioSnapshot,Transform,ItemGeometry,VideoEffect,UserPreferences};
impl VideoEffect {
    pub fn output_dimensions(self,input:(u32,u32))->Result<(u32,u32),Error>{
        self.output_dimensions_with_canvas(input,None)
    }
    pub fn output_dimensions_with_canvas(self,input:(u32,u32),canvas:Option<(u32,u32)>)->Result<(u32,u32),Error>{
        self.validate()?;
        if let Self::ScaleFilter {target,..}=self{return target.output(input,canvas);}
        let (width,height)=match self {
            Self::GaussianBlur{blurriness,repeat_edge,..}=>{let pad=if repeat_edge||blurriness==0.0{0}else{(f64::from(blurriness)*1.5).ceil() as i64};(i64::from(input.0)+2*pad,i64::from(input.1)+2*pad)},
            Self::Scroll {limit_width,limit_height,width,height,..}=>(i64::from(if limit_width{width}else{input.0}),i64::from(if limit_height{height}else{input.1})),
            Self::CropPad {relative,left,top,right,bottom,width,height}=>{
                if relative {((i64::from(input.0)-i64::from(left)-i64::from(right)).max(1),
                    (i64::from(input.1)-i64::from(top)-i64::from(bottom)).max(1))}
                else {(i64::from(width.max(1)),i64::from(height.max(1)))}
            }
            _=>(i64::from(input.0),i64::from(input.1)),
        };
        if matches!(self,Self::CropPad {..}|Self::Scroll{..}|Self::GaussianBlur{..}) && (width<1 || height<1 || width>8192 || height>8192 || width*height>32*1024*1024) {
            return Err(Error::Limit("filtered image exceeds 8192-edge/32M-pixel limits"));
        }
        Ok((width as u32,height as u32))
    }
}
/// Ordered raw source pixel edges to filtered pixel edges. Extents are useful
/// even when a nonlinear sampler makes affine editing unavailable.
#[derive(Clone,Copy,Debug,PartialEq)]
pub struct VideoFilterMapping{dimensions:(u32,u32),scale:[f64;2],translation:[f64;2],affine:bool}
impl VideoFilterMapping{
    pub fn identity(dimensions:(u32,u32))->Self{Self{dimensions,scale:[1.0;2],translation:[0.0;2],affine:true}}
    pub fn dimensions(self)->(u32,u32){self.dimensions}
    pub fn scale(self)->[f64;2]{self.scale}
    pub fn translation(self)->[f64;2]{self.translation}
    pub fn is_affine(self)->bool{self.affine}
    fn available(self)->Result<(),Error>{if self.affine{Ok(())}else{Err(Error::Unavailable("Root nonlinear scaling or animated Scroll cannot be edited with affine Preview handles"))}}
    pub fn map_point(self,point:[f64;2])->Result<[f64;2],Error>{self.available()?;Ok([point[0]*self.scale[0]+self.translation[0],point[1]*self.scale[1]+self.translation[1]])}
    pub fn inverse_point(self,point:[f64;2])->Result<[f64;2],Error>{self.available()?;Ok([(point[0]-self.translation[0])/self.scale[0],(point[1]-self.translation[1])/self.scale[1]])}
    pub fn map_vector(self,vector:[f64;2])->Result<[f64;2],Error>{self.available()?;Ok([vector[0]*self.scale[0],vector[1]*self.scale[1]])}
    pub fn inverse_vector(self,vector:[f64;2])->Result<[f64;2],Error>{self.available()?;Ok([vector[0]/self.scale[0],vector[1]/self.scale[1]])}
    pub fn map_geometry(self,mut geometry:ItemGeometry)->Result<ItemGeometry,Error>{geometry.origin=self.map_point(geometry.origin)?;geometry.axis_x=self.map_vector(geometry.axis_x)?;geometry.axis_y=self.map_vector(geometry.axis_y)?;Ok(geometry)}
}
impl Source {
    pub fn requires_video_canvas(&self)->bool{self.video_filters().iter().any(|f|f.enabled&&matches!(f.effect,VideoEffect::ScaleFilter{target:crate::ScaleFilterTarget::Canvas,..}))}
    pub fn video_filter_mapping(&self,raw:(u32,u32),canvas:Option<(u32,u32)>)->Result<VideoFilterMapping,Error>{
        self.video_filter_mapping_with_preferences(raw,canvas,None)
    }
    pub(crate) fn video_filter_mapping_with_preferences(&self,raw:(u32,u32),canvas:Option<(u32,u32)>,preferences:Option<&UserPreferences>)->Result<VideoFilterMapping,Error>{
        let mut mapping=VideoFilterMapping::identity(raw);let mut input=raw;
        for filter in self.video_filters().iter().filter(|filter|filter.enabled&&preferences.is_none_or(|prefs|prefs.plugin_enabled(filter.plugin_id()))){
            let output=filter.effect.output_dimensions_with_canvas(input,canvas)?;
            match filter.effect{
                VideoEffect::GaussianBlur{blurriness,repeat_edge,..}=>{let pad=if repeat_edge||blurriness==0.0{0.0}else{(f64::from(blurriness)*1.5).ceil()};mapping.translation[0]+=pad;mapping.translation[1]+=pad;},
                VideoEffect::CropPad{left,top,..}=>{mapping.translation[0]-=f64::from(left);mapping.translation[1]-=f64::from(top);},
                VideoEffect::ScaleFilter{target,sampling,undistort}=>{
                    if !target.skips(input){
                        let low=output.0<input.0/2||output.1<input.1/2;
                        let old=f64::from(input.0)/f64::from(input.1);
                        let desired=match target{crate::ScaleFilterTarget::Aspect{width,height}=>f64::from(width)/f64::from(height),_=>f64::from(output.0)/f64::from(output.1)};
                        if undistort&&sampling.allows_undistort()&&!low&&(desired/old-1.0).abs()>f64::EPSILON{mapping.affine=false;}
                        let ratio=[f64::from(output.0)/f64::from(input.0),f64::from(output.1)/f64::from(input.1)];
                        for i in 0..2{mapping.scale[i]*=ratio[i];mapping.translation[i]*=ratio[i];}
                    }
                },
                VideoEffect::Scroll{speed_x,speed_y,..}if speed_x!=0.0||speed_y!=0.0=>mapping.affine=false,
                _=>{},
            }
            input=output;
        }
        mapping.dimensions=input;Ok(mapping)
    }

    /// Legacy Crop/Pad-only integer offset. For mixed Crop/Scale coordinates
    /// use video_filter_mapping with the explicitly owned project canvas.
    pub fn video_filter_origin(&self)->[i64;2]{
        let mut origin=[0i64;2];
        for filter in self.video_filters().iter().filter(|f|f.enabled){
            match filter.effect {VideoEffect::CropPad {left,top,..}=>{origin[0]+=i64::from(left);origin[1]+=i64::from(top);},VideoEffect::GaussianBlur{blurriness,repeat_edge,..}=>{let pad=if repeat_edge||blurriness==0.0{0}else{(f64::from(blurriness)*1.5).ceil() as i64};origin[0]-=pad;origin[1]-=pad;},_=>{}}
        }
        origin
    }
    /// Apply the entire enabled, dimension-changing chain in authored order.
    pub fn filtered_video_dimensions(&self,raw:(u32,u32))->Result<(u32,u32),Error>{self.filtered_video_dimensions_with_canvas(raw,None)}
    pub fn filtered_video_dimensions_with_canvas(&self,raw:(u32,u32),canvas:Option<(u32,u32)>)->Result<(u32,u32),Error>{
        self.filtered_video_dimensions_with_preferences(raw,canvas,None)
    }
    fn filtered_video_dimensions_with_preferences(&self,mut raw:(u32,u32),canvas:Option<(u32,u32)>,preferences:Option<&UserPreferences>)->Result<(u32,u32),Error>{
        for filter in self.video_filters().iter().filter(|f|f.enabled&&preferences.is_none_or(|prefs|prefs.plugin_enabled(f.plugin_id()))) {
            raw=filter.effect.output_dimensions_with_canvas(raw,canvas)?;
        }
        Ok(raw)
    }
}
impl StudioSnapshot {
    /// Dimensions of the actual rendered chain; disabled providers bypass their filters.
    pub fn effective_filtered_video_dimensions(&self,source:&Source,raw:(u32,u32),canvas:Option<(u32,u32)>)->Result<(u32,u32),Error>{
        source.filtered_video_dimensions_with_preferences(raw,canvas,Some(self.preferences()))
    }
    /// Mapping of the actual rendered chain, using the same provider state as rendering.
    pub fn effective_video_filter_mapping(&self,source:&Source,raw:(u32,u32),canvas:Option<(u32,u32)>)->Result<VideoFilterMapping,Error>{
        source.video_filter_mapping_with_preferences(raw,canvas,Some(self.preferences()))
    }
    pub fn source_filtered_dimensions(&self,source:&Source)->Result<Option<(u32,u32)>,Error>{
        self.source_video_dimensions(source.kind())?.map(|raw|self.effective_filtered_video_dimensions(source,raw,Some((self.video().width(),self.video().height())))).transpose()
    }
    pub fn source_item_geometry(&self,source:&Source,transform:&Transform)->Result<ItemGeometry,Error>{
        let basis=self.source_video_dimensions(source.kind())?.ok_or(Error::Invalid("audio-only source has no geometry"))?;
        let frame=self.effective_filtered_video_dimensions(source,basis,Some((self.video().width(),self.video().height())))?;
        transform.geometry_for_frame(basis.0,basis.1,frame.0,frame.1)
    }
}
