//! OBS32 gs_premultiply_xyza_srgb byte policy, before texture sampling.
//! Behavioral reference: libobs/graphics/srgb.h, GPL-2.0-or-later.
use crate::RgbaFrame;
use std::sync::OnceLock;

fn table()->&'static [u8;65536]{
    static TABLE:OnceLock<[u8;65536]>=OnceLock::new();
    TABLE.get_or_init(||std::array::from_fn(|index|{
        let color=(index&255)as f32/255.0;
        let alpha=(index>>8)as f32/255.0;
        let linear=if color<=0.04045{color/12.92}else{((color+0.055)/1.055).powf(2.4)};
        let value=linear*alpha;
        let encoded=if value<=0.0031308{12.92*value}else{1.055*value.powf(1.0/2.4)-0.055};
        (encoded*255.0+0.5)as u8
    }))
}
pub(crate) fn premultiply(frame:&mut RgbaFrame){
    let values=table();
    for pixel in frame.pixels.chunks_exact_mut(4){
        let row=usize::from(pixel[3])<<8;
        for color in &mut pixel[..3]{*color=values[row|usize::from(*color)];}
    }
}
