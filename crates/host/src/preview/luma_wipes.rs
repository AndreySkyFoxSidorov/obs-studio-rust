//! Worker-only stock mask cache. Encoded assets are immutable build inputs.
use super::*;
use image::{DynamicImage,ImageDecoder};
use skylive_core::LumaWipePattern;
use skylive_media::LumaWipeMask;
use sha2::{Digest,Sha256};
use std::io::Cursor;
struct Spec{bytes:&'static[u8],width:u32,height:u32,sha:&'static str}
fn spec(pattern:LumaWipePattern)->Spec{match pattern{
 LumaWipePattern::BarndoorBottomLeft=>Spec{bytes:include_bytes!("../../../../assets/luma-wipes/barndoor-botleft.png"),width:720,height:480,sha:"92e16724c775e02dd172f0af9611ded7fb4b07a0bd94d75f8fa25078cea30f55"},
 LumaWipePattern::BarndoorHorizontal=>Spec{bytes:include_bytes!("../../../../assets/luma-wipes/barndoor-h.png"),width:720,height:576,sha:"06bb7ccdefec39b7ce77e5fe873fc53929243d94e5749c3043495082335f12be"},
 LumaWipePattern::BarndoorTopLeft=>Spec{bytes:include_bytes!("../../../../assets/luma-wipes/barndoor-topleft.png"),width:720,height:480,sha:"6f8653af6717f543ed9f83a2d735597cac91ff9f68b7f9cec99bcf48c8037f45"},
 LumaWipePattern::BarndoorVertical=>Spec{bytes:include_bytes!("../../../../assets/luma-wipes/barndoor-v.png"),width:720,height:576,sha:"d94cc264f3d25d44c9cf2e60b77cdc76b6c024c7e3a04b33ae764db5fc1cdef4"},
 LumaWipePattern::BlindsHorizontal=>Spec{bytes:include_bytes!("../../../../assets/luma-wipes/blinds-h.png"),width:720,height:560,sha:"fedc4d9fbbaf123fc0bc6585604cbc39bfaaa5baefc6a787287174c926a1ca77"},
 LumaWipePattern::BoxBottomLeft=>Spec{bytes:include_bytes!("../../../../assets/luma-wipes/box-botleft.png"),width:720,height:480,sha:"116b774fed1c8f58882f7d052c862103c9cec146dac323aa9df43a7e019cf3eb"},
 LumaWipePattern::BoxBottomRight=>Spec{bytes:include_bytes!("../../../../assets/luma-wipes/box-botright.png"),width:720,height:480,sha:"92ed6749d63aa7d9aa7fdbfa0506e828a6509ba27558d647f39096d173d3a5fa"},
 LumaWipePattern::BoxTopLeft=>Spec{bytes:include_bytes!("../../../../assets/luma-wipes/box-topleft.png"),width:720,height:480,sha:"c649e59fc6d3e28cd827ab787578ae158719881ee0c10006860faef81f0eef21"},
 LumaWipePattern::BoxTopRight=>Spec{bytes:include_bytes!("../../../../assets/luma-wipes/box-topright.png"),width:720,height:480,sha:"d743c298451e6ecd2813c87fab9cd58899a7869988e88ee0b24495a9723567d1"},
 LumaWipePattern::Burst=>Spec{bytes:include_bytes!("../../../../assets/luma-wipes/burst.png"),width:720,height:560,sha:"b46f17e4525b94b0a03de3eee82676524f6d02b20293138f897b28577581c56c"},
 LumaWipePattern::CheckerboardSmall=>Spec{bytes:include_bytes!("../../../../assets/luma-wipes/checkerboard-small.png"),width:720,height:560,sha:"8f6b70e157316beca2723b169ac21729bc71722c85d7a183b71133d868e4dc85"},
 LumaWipePattern::Circles=>Spec{bytes:include_bytes!("../../../../assets/luma-wipes/circles.png"),width:720,height:576,sha:"760ae2fea550e0de114d78c9fe1b794b6adba9727a99a61a4449ec4281bea025"},
 LumaWipePattern::Clock=>Spec{bytes:include_bytes!("../../../../assets/luma-wipes/clock.png"),width:720,height:576,sha:"6ebc4c8ae077243fa8003683e063ce6bf075d2cfd4de4d88c42278d1c0b607de"},
 LumaWipePattern::Cloud=>Spec{bytes:include_bytes!("../../../../assets/luma-wipes/cloud.png"),width:720,height:576,sha:"427765c738f2a206b177b49ae17337f1e139777ea6405cbfce423ca183e79e5b"},
 LumaWipePattern::Curtain=>Spec{bytes:include_bytes!("../../../../assets/luma-wipes/curtain.png"),width:720,height:576,sha:"03df4e705d06948a66ca47f1f8140a08d2ca02f079379b4bec0b4fb72b3982c8"},
 LumaWipePattern::Fan=>Spec{bytes:include_bytes!("../../../../assets/luma-wipes/fan.png"),width:720,height:560,sha:"709762b8078cd3056aa5b5ec1fb93b13af0ca5d17013234a5018f20cf66c130f"},
 LumaWipePattern::Fractal=>Spec{bytes:include_bytes!("../../../../assets/luma-wipes/fractal.png"),width:720,height:576,sha:"f004c1d893f04c83b732e522a5c73f3d719f3bf848a100b0d60d373cd1231690"},
 LumaWipePattern::Iris=>Spec{bytes:include_bytes!("../../../../assets/luma-wipes/iris.png"),width:720,height:576,sha:"59c0477aef7fb15b8c04891d4440d3365e7a1a13ebecb17de5d23d0610cdc2a2"},
 LumaWipePattern::LinearHorizontal=>Spec{bytes:include_bytes!("../../../../assets/luma-wipes/linear-h.png"),width:720,height:576,sha:"8d27430d6741e56ebb53e60e7ec0497ce263dd42b22394e0801cdbb80d47c33f"},
 LumaWipePattern::LinearTopLeft=>Spec{bytes:include_bytes!("../../../../assets/luma-wipes/linear-topleft.png"),width:720,height:480,sha:"333a332ca98955a9fcf6cebb6d3de1801b6cb705d6f589959b4d0d920265f063"},
 LumaWipePattern::LinearTopRight=>Spec{bytes:include_bytes!("../../../../assets/luma-wipes/linear-topright.png"),width:720,height:480,sha:"82ed930146a27493dd1ef97fcc25ac9aa0d51a59add9a4ecc8e8ac352f9a6a79"},
 LumaWipePattern::LinearVertical=>Spec{bytes:include_bytes!("../../../../assets/luma-wipes/linear-v.png"),width:720,height:576,sha:"4b53b3e8f18b837ef5fefc95ed9385f7f9bd63a0afabd638b1efd46b759400cc"},
 LumaWipePattern::ParallelZigzagHorizontal=>Spec{bytes:include_bytes!("../../../../assets/luma-wipes/parallel-zigzag-h.png"),width:720,height:480,sha:"0798e66f35b9d45accfb0159bbcd9884d766f6b8788294026c4810cac1907eca"},
 LumaWipePattern::ParallelZigzagVertical=>Spec{bytes:include_bytes!("../../../../assets/luma-wipes/parallel-zigzag-v.png"),width:720,height:480,sha:"d44960afec553563e72fc20c1b9021f063b685633926dcd581230c4bd1d11c34"},
 LumaWipePattern::Sinus9=>Spec{bytes:include_bytes!("../../../../assets/luma-wipes/sinus9.png"),width:720,height:576,sha:"9b631696e6e1d8e69548732512276c922c38b6cc1c9b311b3d51f0d999193643"},
 LumaWipePattern::Spiral=>Spec{bytes:include_bytes!("../../../../assets/luma-wipes/spiral.png"),width:720,height:576,sha:"f4980c559fc18f40e4f291648d27f595a817893cde61883d4ca6dd99c98bb40a"},
 LumaWipePattern::Square=>Spec{bytes:include_bytes!("../../../../assets/luma-wipes/square.png"),width:720,height:576,sha:"766527ccad0772bbeb2a3f8f123b81e3d2366a05bc1e7069ef185b5513de7095"},
 LumaWipePattern::Squares=>Spec{bytes:include_bytes!("../../../../assets/luma-wipes/squares.png"),width:720,height:576,sha:"a270090ba650cc1ed62f27dee2ccce93fcade14af8ede652ddea3a4c6cafd8b3"},
 LumaWipePattern::Stripes=>Spec{bytes:include_bytes!("../../../../assets/luma-wipes/stripes.png"),width:720,height:576,sha:"3903d1ecc9e9c72918cf9a84979b5bf98fe4cac9bc0a45bbb55e6f090ecfcbcf"},
 LumaWipePattern::StripsHorizontal=>Spec{bytes:include_bytes!("../../../../assets/luma-wipes/strips-h.png"),width:720,height:480,sha:"34e7f5d27789a2eb44894a6ea64a93861ec6f7235c25f49ac8a230fbf6d2ea2c"},
 LumaWipePattern::StripsVertical=>Spec{bytes:include_bytes!("../../../../assets/luma-wipes/strips-v.png"),width:720,height:480,sha:"a6c705ee03f3bb105cd7007efe826e0572fccdbf74d863ad4d80a0c5afcb73a8"},
 LumaWipePattern::Watercolor=>Spec{bytes:include_bytes!("../../../../assets/luma-wipes/watercolor.png"),width:800,height:600,sha:"a2341da7c22298a4cc84bf5087ba237a1eadf67b5a45a780ffb6dbebc741edc2"},
 LumaWipePattern::ZigzagHorizontal=>Spec{bytes:include_bytes!("../../../../assets/luma-wipes/zigzag-h.png"),width:720,height:480,sha:"72118528baa2e6d55af1f3a3efb46aaf8736549475c5916d478e459759e743f9"},
 LumaWipePattern::ZigzagVertical=>Spec{bytes:include_bytes!("../../../../assets/luma-wipes/zigzag-v.png"),width:720,height:480,sha:"dc58bc3a7cfa04846d792d3b10fc935e2280d6265a1b946732bba894b1b05e98"},
}}
#[derive(Default)]pub(super) struct Cache{loaded:Option<(LumaWipePattern,LumaWipeMask)>,error:Option<(LumaWipePattern,PreviewError)>}
impl Cache{
 pub(super) fn clear_error(&mut self){self.error=None;}
 pub(super) fn bytes(&self)->usize{self.loaded.as_ref().map_or(0,|(_,mask)|mask.byte_len())}
 fn matches(&self,pattern:LumaWipePattern)->bool{self.loaded.as_ref().is_some_and(|(p,_)|*p==pattern)}
 pub(super) fn get(&mut self,pattern:LumaWipePattern)->Result<&LumaWipeMask,PreviewError>{
  if let Some((old,error))=&self.error{if *old==pattern{return Err(error.clone());}}
  if !self.matches(pattern){
   self.error=None;
   let result=(||{
   // No reader escapes the synchronous owning renderer. Release old capacity
   // before the next PNG/RGBA/red allocations, never keep an unbounded catalog.
   self.loaded=None;let s=spec(pattern);
   let fail=|detail:String|PreviewError::LumaWipe{mask:pattern.filename(),detail};
   if s.bytes.len()>512*1024||s.width==0||s.height==0||s.width>800||s.height>600{return Err(fail("embedded mask exceeds recorded profile".into()));}
   if format!("{:x}",Sha256::digest(s.bytes))!=s.sha{return Err(fail("embedded mask content differs from source manifest".into()));}
   let pixels=s.width as u64*s.height as u64;
   let mut limits=image::Limits::default();limits.max_image_width=Some(s.width);limits.max_image_height=Some(s.height);limits.max_alloc=Some(pixels*8+65536);
   let mut decoder=image::codecs::png::PngDecoder::with_limits(Cursor::new(s.bytes),limits.clone()).map_err(|e|fail(e.to_string()))?;
   if decoder.dimensions()!=(s.width,s.height)||!matches!(decoder.color_type(),image::ColorType::L8|image::ColorType::La8|image::ColorType::Rgb8|image::ColorType::Rgba8){return Err(fail("embedded mask dimensions/SDR channels differ from source manifest".into()));}
   decoder.set_limits(limits).map_err(|e|fail(e.to_string()))?;
   let rgba=DynamicImage::from_decoder(decoder).map_err(|e|fail(e.to_string()))?.into_rgba8();
   let mut red=Vec::new();red.try_reserve_exact(pixels as usize).map_err(|_|fail("embedded mask plane allocation".into()))?;
   for pixel in rgba.as_raw().chunks_exact(4){red.push(pixel[0]);}drop(rgba);
   let mask=LumaWipeMask::new(s.width,s.height,red).map_err(PreviewError::Media)?;
   Ok(mask)
   })();
   match result{Ok(mask)=>self.loaded=Some((pattern,mask)),Err(error)=>{self.error=Some((pattern,error.clone()));return Err(error);}}
  }
  self.loaded.as_ref().map(|(_,mask)|mask).ok_or(PreviewError::Media(MediaError::InvalidBuffer))
 }
}
fn selected(output:&FrameComposition)->Option<(LumaWipePattern,bool)>{match output{
 FrameComposition::Configured{from,to,kind:TransitionKind::LumaWipe{mask,..},..}|FrameComposition::Manual{from,to,kind:TransitionKind::LumaWipe{mask,..},..}=>Some((*mask,from.is_none()||to.is_none())),_=>None,
}}
/// Metadata-only reservation before scene pixels, PNG decode or transition.
/// Both decoded scratch and the complete mask+mix work count in the same job.
pub(super) fn reserve(resources:&mut RenderResources<'_>,output:&FrameComposition,size:(u32,u32),mix_reserved:bool)->Result<(),PreviewError>{
 let Some((pattern,missing))=selected(output)else{return Ok(());};
 if resources.full_resolution{resources.base_work_limit=256*1024*1024;}
 if let Some((old,error))=&resources.cache.luma_wipe.error{if *old==pattern{return Err(error.clone());}}
 let s=spec(pattern);let mask=s.width as usize*s.height as usize;let pixels=size.0 as usize*size.1 as usize;
 // Decoder allocation allowance(8P) plus decoded RGBA(4P) bounds the
 // largest overlap; RGB->RGBA conversion(7P) and extraction(5P) are smaller.
 let decode=if resources.cache.luma_wipe.matches(pattern){0}else{mask.checked_mul(12).and_then(|n|n.checked_add(65536)).ok_or(PreviewError::InvalidSize)?};
 let mix=if mix_reserved{0}else{pixels.checked_mul(4*(1+usize::from(missing))).ok_or(PreviewError::InvalidSize)?};
 let work=(if decode==0{0}else{mask*5}).checked_add(if mix_reserved{0}else{pixels.checked_mul(7).ok_or(PreviewError::InvalidSize)?}).ok_or(PreviewError::InvalidSize)?;
 let bytes=decode.checked_add(mix).ok_or(PreviewError::InvalidSize)?;
 let used=resources.cache.bytes.saturating_add(resources.cache.text.bytes).saturating_add(resources.live_bytes);
 if bytes>MAX_RENDER_RESOURCE_BYTES.saturating_sub(used)||work>resources.work_remaining(){return Err(PreviewError::Media(MediaError::BudgetExceeded));}
 resources.live_bytes+=bytes;resources.pixel_visits+=work;Ok(())
}
