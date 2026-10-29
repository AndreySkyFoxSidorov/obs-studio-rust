//! Portable approximation of OBS's GDI+ angle-scalable linear text brush.
//! Colors are interpolated in encoded RGBA; no extra raster is allocated.
pub(super) struct Brush{first:[u8;4],second:[u8;4],x:f64,y:f64,origin:f64}
impl Brush{
    pub(super) fn new(first:[u8;4],gradient:skylive_core::TextGradient,width:u32,height:u32)->Self{
        let angle=f64::from(gradient.direction).to_radians();let (sin,cos)=angle.sin_cos();
        let denominator=cos.abs()+sin.abs();
        let x=cos/(f64::from(width)*denominator);let y=sin/(f64::from(height)*denominator);
        Self{first,second:[gradient.color[0],gradient.color[1],gradient.color[2],(u16::from(gradient.opacity)*255/100) as u8],x,y,origin:0.5-(cos+sin)/(2.0*denominator)}
    }
    pub(super) fn sample(&self,x:i32,y:i32)->[u8;4]{
        let t=(self.origin+(f64::from(x)+0.5)*self.x+(f64::from(y)+0.5)*self.y).clamp(0.0,1.0);
        std::array::from_fn(|i|(f64::from(self.first[i])+(f64::from(self.second[i])-f64::from(self.first[i]))*t).round().clamp(0.0,255.0) as u8)
    }
}
