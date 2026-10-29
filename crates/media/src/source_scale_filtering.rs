//! OBS32 SDR reconstruction kernels; CPU target sampling, not a GPU backend.
//! New choices retain the OBS eight-bilinear lowres branch below half scale.
use crate::{AffineLayer,AffinePlan,SamplingWindow,SamplingFilter,MediaError};
use crate::item_compositing::{Sample,tap};

#[derive(Clone,Copy)]
struct Window {left:f64,top:f64,width:f64,height:f64,min_x:i64,min_y:i64,max_x:i64,max_y:i64}
impl Window {
    fn new(window:&SamplingWindow,layer:&AffineLayer<'_>)->Self {
        match window {
            SamplingWindow::Texels(r)=>Self {left:f64::from(r.x),top:f64::from(r.y),width:f64::from(r.width),height:f64::from(r.height),min_x:i64::from(r.x),min_y:i64::from(r.y),max_x:i64::from(r.x+r.width-1),max_y:i64::from(r.y+r.height-1)},
            SamplingWindow::Uv {uv,last_x,last_y}=>{
                let w=f64::from(layer.image.width);let h=f64::from(layer.image.height);
                Self {left:uv.left*w,top:uv.top*h,width:(uv.right-uv.left)*w,height:(uv.bottom-uv.top)*h,
                    min_x:((uv.left*w).floor() as i64).min(*last_x as i64),min_y:((uv.top*h).floor() as i64).min(*last_y as i64),max_x:*last_x as i64,max_y:*last_y as i64}
            }
        }
    }
    fn at(self,layer:&AffineLayer<'_>,x:i64,y:i64)->Sample {
        let x=x.clamp(self.min_x,self.max_x) as usize;let y=y.clamp(self.min_y,self.max_y) as usize;
        let index=(y*layer.image.width as usize+x)*4;
        tap(&layer.image.pixels[index..index+4],layer.blend_mode,layer.item_blending_method)
    }
    fn bilinear(self,layer:&AffineLayer<'_>,x:f64,y:f64)->Sample {
        let x=x.clamp(self.min_x as f64,self.max_x as f64);let y=y.clamp(self.min_y as f64,self.max_y as f64);
        let x0=x.floor() as i64;let y0=y.floor() as i64;
        let fx=x-x0 as f64;let fy=y-y0 as f64;
        let mut result=zero();
        for (sx,sy,weight) in [(x0,y0,(1.0-fx)*(1.0-fy)),(x0+1,y0,fx*(1.0-fy)),(x0,y0+1,(1.0-fx)*fy),(x0+1,y0+1,fx*fy)] {add(&mut result,self.at(layer,sx,sy),weight);}
        result
    }
}
fn zero()->Sample{Sample {rgb:[0.0;3],alpha:0.0}}
fn add(out:&mut Sample,value:Sample,weight:f64){for (i,channel) in out.rgb.iter_mut().enumerate() {*channel+=value.rgb[i]*weight;}out.alpha+=value.alpha*weight;}
fn cubic(x:f64)->[f64;4]{[
    ((-0.75*x+1.5)*x-0.75)*x,
    (1.25*x-2.25)*x*x+1.0,
    ((-1.25*x+1.5)*x+0.75)*x,
    (0.75*x-0.75)*x*x,
]}
#[allow(clippy::approx_constant)] // Preserve the literal in the pinned OBS shader.
fn sinc3(x:f64)->f64{if x==0.0 {1.0}else{let p=x*3.141592654;3.0*p.sin()*(p*(1.0/3.0)).sin()/(p*p)}}
fn lanczos(f_neg:f64)->[f64;6]{
    let mut values=[sinc3(f_neg-2.0),sinc3(f_neg-1.0),sinc3(f_neg).min(1.0),sinc3(f_neg+1.0),sinc3(f_neg+2.0),sinc3(f_neg+3.0)];
    let sum=values.iter().copied().sum::<f64>();let inverse=1.0/sum;
    for value in &mut values {*value*=inverse;}values
}
#[derive(Clone,Copy,Debug)]
pub(super) enum Scope {SceneItem,GlobalOutput {width:u32,height:u32},SourceFilter {width:u32,height:u32}}
#[derive(Clone,Copy)]
enum Kernel {Bilinear,Bicubic,Lanczos,Lowres,Area}
pub(super) struct Prepared {undistort:Option<f64>,window:Window,kernel:Kernel,dx:[f64;2],dy:[f64;2],area:f64,pub cost:u64}
impl Prepared {
    /// Prove that this reconstruction evaluates exactly one input texel.
    /// Mixed or negative-weight samples cannot preserve a pixel witness without
    /// also tracking the magnitude and sign of its contribution.
    pub(super) fn exact_texel(&self,u:f64,v:f64,sx:i64,sy:i64)->bool {
        let x=self.window.left+u*self.window.width-0.5;
        let y=self.window.top+v*self.window.height-0.5;
        match self.kernel {
            Kernel::Bilinear=>x.clamp(self.window.min_x as f64,self.window.max_x as f64)==sx as f64
                && y.clamp(self.window.min_y as f64,self.window.max_y as f64)==sy as f64,
            Kernel::Bicubic if self.undistort.is_none()=>x==sx as f64&&y==sy as f64,
            Kernel::Area if self.dx[1]==0.0&&self.dy[0]==0.0=>{
                let cx=x+0.5;let cy=y+0.5;
                let hx=self.dx[0].abs()*0.5;let hy=self.dy[1].abs()*0.5;
                // A strict interior avoids floating-point boundary ambiguity.
                cx-hx>sx as f64 && cx+hx<(sx+1) as f64
                    && cy-hy>sy as f64 && cy+hy<(sy+1) as f64
            },
            _=>false,
        }
    }
    pub fn new(layer:&AffineLayer<'_>,window:&SamplingWindow,plan:&AffinePlan)->Result<Option<Self>,MediaError>{
        Self::prepare(layer.sampling_filter,Window::new(window,layer),layer.transform,plan,layer.sampling_scope)
    }
    pub(super) fn full_frame(filter:SamplingFilter,input:(u32,u32),transform:crate::Affine2,plan:&AffinePlan,scope:Scope)->Result<Option<Self>,MediaError>{
        Self::prepare(filter,Window {left:0.0,top:0.0,width:f64::from(input.0),height:f64::from(input.1),min_x:0,min_y:0,max_x:i64::from(input.0)-1,max_y:i64::from(input.1)-1},transform,plan,scope)
    }
    pub(super) fn source_filter(filter:SamplingFilter,input:(u32,u32),output:(u32,u32),transform:crate::Affine2,plan:&AffinePlan,undistort:Option<f64>)->Result<Option<Self>,MediaError>{
        if undistort.is_some_and(|factor|!factor.is_finite()||factor<=0.0){return Err(MediaError::InvalidTransform);}
        let mut result=Self::full_frame(filter,input,transform,plan,Scope::SourceFilter{width:output.0,height:output.1})?;
        if let Some(prepared)=&mut result{if matches!(prepared.kernel,Kernel::Bicubic|Kernel::Lanczos){prepared.undistort=undistort.filter(|f|*f!=1.0);if prepared.undistort.is_some(){prepared.cost=prepared.cost.checked_mul(4).ok_or(MediaError::Overflow)?;}}}
        Ok(result)
    }
    fn prepare(filter:SamplingFilter,window:Window,transform:crate::Affine2,plan:&AffinePlan,scope:Scope)->Result<Option<Self>,MediaError>{
        if filter==SamplingFilter::Nearest || (matches!(scope,Scope::SceneItem)&&filter==SamplingFilter::Bilinear){return Ok(None);}
        let sx=transform.xx.hypot(transform.yx)/window.width;
        let sy=transform.xy.hypot(transform.yy)/window.height;
        let dx=[plan.inverse_xx*window.width,plan.inverse_yx*window.height];
        let dy=[plan.inverse_xy*window.width,plan.inverse_yy*window.height];
        if ![sx,sy,dx[0],dx[1],dy[0],dy[1]].iter().all(|v|v.is_finite()) || sx<=0.0 || sy<=0.0 {return Err(MediaError::InvalidTransform);}
        let (near,lowres)=match scope {
            Scope::SceneItem=>((sx-1.0).abs()<=1e-4&&(sy-1.0).abs()<=1e-4,sx<0.5||sy<0.5),
            Scope::SourceFilter {width,height}=>(false,width<(window.width as u32/2)||height<(window.height as u32/2)),
            Scope::GlobalOutput {width,height}=>(
                (f64::from(width)-window.width).abs()<=16.0&&(f64::from(height)-window.height).abs()<=16.0,
                width<(window.width as u32/2)&&height<(window.height as u32/2)),
        };
        let (kernel,cost)=if near {(Kernel::Bilinear,4)} else if lowres {(Kernel::Lowres,32)}
            else {match filter {SamplingFilter::Bilinear=>(Kernel::Bilinear,4),SamplingFilter::Bicubic=>(Kernel::Bicubic,16),SamplingFilter::Lanczos=>(Kernel::Lanczos,36),SamplingFilter::Area=>(Kernel::Area,0),_=>unreachable!()}};
        let area=(dx[0]*dy[1]-dy[0]*dx[1]).abs();
        let cost=if matches!(kernel,Kernel::Area){
            let span_x=dx[0].abs()+dy[0].abs();let span_y=dx[1].abs()+dy[1].abs();
            // Scene items retain their32x32 bound. A global output can have
            // one strongly reduced axis while the other is enlarged: legal
            //4096-to16 anisotropy needs<=1032 conservative candidates. Its
            //2048-tap maximum is still charged to the same cumulative budget.
            let candidates_x=span_x.ceil()+2.0;let candidates_y=span_y.ceil()+2.0;
            if !area.is_finite() || area<f64::MIN_POSITIVE || !candidates_x.is_finite() || !candidates_y.is_finite(){return Err(MediaError::BudgetExceeded);}
            let maximum=match scope {Scope::SceneItem=>{if candidates_x>32.0||candidates_y>32.0{return Err(MediaError::BudgetExceeded);}1024},Scope::GlobalOutput {..}|Scope::SourceFilter {..}=>2048};
            if candidates_x*candidates_y>maximum as f64{return Err(MediaError::BudgetExceeded);}
            (candidates_x as u64).checked_mul(candidates_y as u64).ok_or(MediaError::Overflow)?
        }else{cost};
        Ok(Some(Self {undistort:None,window,kernel,dx,dy,area,cost}))
    }
    fn reconstruction_tap(&self,layer:&AffineLayer<'_>,x:i64,y:i64)->Sample{
        let Some(factor)=self.undistort else{return self.window.at(layer,x,y);};
        // OBS warps each reconstruction tap, not the output center alone.
        let u=(x as f64+0.5)/self.window.width;let normalized=(u-0.5)*2.0;
        let warped=((1.0-factor)*(normalized*normalized*normalized*normalized*normalized)+factor*normalized)*0.5+0.5;
        self.window.bilinear(layer,warped*self.window.width-0.5,y as f64)
    }
    pub fn sample(&self,layer:&AffineLayer<'_>,u:f64,v:f64)->Sample {
        let x=self.window.left+u*self.window.width-0.5;let y=self.window.top+v*self.window.height-0.5;
        match self.kernel {
            Kernel::Bilinear=>self.window.bilinear(layer,x,y),
            Kernel::Bicubic=>{
                let bx=x.floor() as i64;let by=y.floor() as i64;let wx=cubic(x-bx as f64);let wy=cubic(y-by as f64);let mut result=zero();
                for (row,weight_y) in wy.iter().copied().enumerate() {let mut line=zero();for (col,weight_x) in wx.iter().copied().enumerate() {add(&mut line,self.reconstruction_tap(layer,bx+col as i64-1,by+row as i64-1),weight_x);}add(&mut result,line,weight_y);}result
            }
            Kernel::Lanczos=>{
                let bx=x.floor() as i64;let by=y.floor() as i64;let wx=lanczos(bx as f64-x);let wy=lanczos(by as f64-y);let mut result=zero();
                for (row,weight_y) in wy.iter().copied().enumerate() {let mut line=zero();for (col,weight_x) in wx.iter().copied().enumerate() {add(&mut line,self.reconstruction_tap(layer,bx+col as i64-2,by+row as i64-2),weight_x);}add(&mut result,line,weight_y);}result
            }
            Kernel::Lowres=>{
                // The exact OBS D3D8 sample pattern and diagonal derivatives.
                let mut result=zero();for (a,b) in [(1.0,-3.0),(-1.0,3.0),(5.0,1.0),(-3.0,-5.0),(-5.0,5.0),(-7.0,-1.0),(3.0,7.0),(7.0,-7.0)] {
                    add(&mut result,self.window.bilinear(layer,x+self.dx[0]*a/16.0,y+self.dy[1]*b/16.0),0.125);
                }result
            }
            Kernel::Area=>self.area_sample(layer,x+0.5,y+0.5),
        }
    }
    fn area_sample(&self,layer:&AffineLayer<'_>,cx:f64,cy:f64)->Sample {
        // Work in texel-center-relative coordinates to avoid subtracting large
        // absolute coordinates when a footprint is smaller than one pixel.
        let bx=cx.floor() as i64;let by=cy.floor() as i64;let fx=cx-bx as f64;let fy=cy-by as f64;
        let span_x=self.dx[0].abs()+self.dy[0].abs();let span_y=self.dx[1].abs()+self.dy[1].abs();
        let begin_x=(fx-span_x*0.5).floor() as i64;let end_x=(fx+span_x*0.5).ceil() as i64;
        let begin_y=(fy-span_y*0.5).floor() as i64;let end_y=(fy+span_y*0.5).ceil() as i64;
        let rectangular=(self.dx[1]==0.0 && self.dy[0]==0.0)||(self.dx[0]==0.0 && self.dy[1]==0.0);
        let corners=[[-0.5*(self.dx[0]+self.dy[0]),-0.5*(self.dx[1]+self.dy[1])],
            [0.5*(self.dx[0]-self.dy[0]),0.5*(self.dx[1]-self.dy[1])],
            [0.5*(self.dx[0]+self.dy[0]),0.5*(self.dx[1]+self.dy[1])],
            [0.5*(-self.dx[0]+self.dy[0]),0.5*(-self.dx[1]+self.dy[1])]];
        let mut result=zero();
        for y in begin_y..end_y {for x in begin_x..end_x {
            let left=x as f64-fx;let top=y as f64-fy;
            let weight=if rectangular && span_x>=1e-6 && span_y>=1e-6 {
                let w=(left+1.0).min(span_x*0.5)-left.max(-span_x*0.5);
                let h=(top+1.0).min(span_y*0.5)-top.max(-span_y*0.5);
                w.max(0.0)*h.max(0.0)/self.area
            }else{intersection_area(corners,left,top)/self.area};
            if weight>0.0 {add(&mut result,self.window.at(layer,bx+x,by+y),weight);}
        }}result
    }
}
// A convex quadrilateral clipped to a rectangle has at most eight vertices.
// Twelve fixed slots avoid heap or hidden full-frame scratch on rotated Area.
fn clip(input:&[[f64;2];12],len:usize,axis:usize,bound:f64,greater:bool)->([[f64;2];12],usize){
    let mut output=[[0.0;2];12];let mut count=0usize;if len==0{return(output,0);}
    let inside=|p:[f64;2]|if greater {p[axis]>=bound}else{p[axis]<=bound};
    let mut previous=input[len-1];let mut previous_in=inside(previous);
    for current in input.iter().copied().take(len) {
        let current_in=inside(current);
        if current_in!=previous_in {
            let t=(bound-previous[axis])/(current[axis]-previous[axis]);
            output[count]=[previous[0]+t*(current[0]-previous[0]),previous[1]+t*(current[1]-previous[1])];count+=1;
        }
        if current_in {output[count]=current;count+=1;}
        previous=current;previous_in=current_in;
    }
    (output,count)
}
fn intersection_area(corners:[[f64;2];4],left:f64,top:f64)->f64 {
    let mut points=[[0.0;2];12];points[..4].copy_from_slice(&corners);let mut count=4usize;
    for (axis,bound,greater) in [(0,left,true),(0,left+1.0,false),(1,top,true),(1,top+1.0,false)] {(points,count)=clip(&points,count,axis,bound,greater);if count<3{return 0.0;}}
    let mut twice=0.0;let mut previous=points[count-1];for point in points.iter().copied().take(count) {twice+=previous[0]*point[1]-previous[1]*point[0];previous=point;}
    twice.abs()*0.5
}
