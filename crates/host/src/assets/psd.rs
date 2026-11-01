//! Bounded flattened PSD composite decoder. Layers remain in the source file;
//! only the merged RGB, CMYK, grayscale, bitmap or indexed image is displayed.
use super::{dimensions, AssetError, DecodeBudget, MAX_IMAGE_DECODE_BYTES};
use skylive_media::RgbaFrame;
use std::io::Read;

fn invalid(message: &'static str) -> AssetError {
    AssetError::rejected("image_psd", message)
}

struct Reader<'a> { data: &'a [u8], pos: usize }
impl<'a> Reader<'a> {
    fn take(&mut self, size: usize) -> Result<&'a [u8], AssetError> {
        let end = self.pos.checked_add(size).ok_or_else(|| invalid("PSD offset overflow"))?;
        let value = self.data.get(self.pos..end).ok_or_else(|| invalid("Truncated PSD file"))?;
        self.pos = end;
        Ok(value)
    }
    fn u16(&mut self) -> Result<u16, AssetError> { let b=self.take(2)?; Ok(u16::from_be_bytes([b[0],b[1]])) }
    fn u32(&mut self) -> Result<u32, AssetError> { let b=self.take(4)?; Ok(u32::from_be_bytes([b[0],b[1],b[2],b[3]])) }
    fn block(&mut self) -> Result<&'a [u8], AssetError> { let size=self.u32()? as usize; self.take(size) }
}

fn unpack_row(encoded: &[u8], output: &mut [u8]) -> Result<(), AssetError> {
    let mut source=Reader { data:encoded, pos:0 };
    let mut written=0usize;
    while source.pos < encoded.len() {
        let control=source.take(1)?[0] as i8;
        if control >= 0 {
            let count=control as usize+1;
            let end=written.checked_add(count).filter(|end|*end<=output.len()).ok_or_else(||invalid("PSD RLE row exceeds its width"))?;
            output[written..end].copy_from_slice(source.take(count)?);
            written=end;
        } else if control != -128 {
            let count=(1i16-i16::from(control)) as usize;
            let end=written.checked_add(count).filter(|end|*end<=output.len()).ok_or_else(||invalid("PSD RLE row exceeds its width"))?;
            output[written..end].fill(source.take(1)?[0]);
            written=end;
        }
    }
    if written!=output.len() { return Err(invalid("PSD RLE row is incomplete")); }
    Ok(())
}

// Photoshop predicts each row independently. 32-bit data is additionally
// byte-planar within the row; restore its native big-endian sample order.
fn undo_prediction(row:&mut [u8], depth:u16, width:usize, scratch:&mut Vec<u8>) {
    match depth {
        1|8 => for i in 1..row.len() {row[i]=row[i].wrapping_add(row[i-1]);},
        16 => for i in 1..width {
            let a=u16::from_be_bytes([row[2*i],row[2*i+1]]);
            let b=u16::from_be_bytes([row[2*i-2],row[2*i-1]]);
            row[2*i..2*i+2].copy_from_slice(&a.wrapping_add(b).to_be_bytes());
        },
        32 => {
            for i in 1..row.len() {row[i]=row[i].wrapping_add(row[i-1]);}
            scratch.clear();scratch.extend_from_slice(row);
            for x in 0..width {for byte in 0..4 {row[4*x+byte]=scratch[byte*width+x];}}
        },
        _=>unreachable!("validated PSD depth"),
    }
}

pub(super) fn decode(bytes:&[u8], expected:Option<(u32,u32)>, max_bytes:usize, budget:&mut DecodeBudget) -> Result<RgbaFrame,AssetError> {
    let mut input=Reader { data:bytes, pos:0 };
    if input.take(4)?!=b"8BPS" || input.u16()?!=1 { return Err(invalid("Only PSD version 1 is supported; PSB is not supported")); }
    if input.take(6)?.iter().any(|value|*value!=0) { return Err(invalid("Invalid PSD reserved header")); }
    let channels=usize::from(input.u16()?);
    let height=input.u32()?;
    let width=input.u32()?;
    let depth=input.u16()?;
    let mode=input.u16()?;
    dimensions(width,height)?;
    if expected.is_some_and(|value|value!=(width,height)) { return Err(AssetError::rejected("image_dimensions_changed","PSD dimensions disagree with the recorded asset")); }
    // FFmpeg/OBS also display duotone as grayscale and convert inverted CMYK
    // composite planes to RGB. Spot channels are skipped below.
    let colors=match mode { 0|1|2|8=>1usize, 3=>3usize, 4=>4usize, _=>return Err(invalid("Unsupported PSD composite color mode")) };
    let valid_depth=match mode {0=>depth==1,2=>depth==8,1=>matches!(depth,8|16|32),_=>matches!(depth,8|16)};
    if channels<colors || channels>56 || !valid_depth { return Err(invalid("Unsupported PSD channel count or bit depth")); }
    let pixels=width as usize*height as usize;
    let rgba_bytes=pixels*4;
    let row_bytes=if mode==0 {(width as usize+7)/8} else {width as usize*usize::from(depth/8)};
    let planar_bytes=row_bytes.checked_mul(height as usize).and_then(|v|v.checked_mul(channels)).ok_or_else(||invalid("PSD raster size overflow"))?;
    if rgba_bytes>max_bytes || planar_bytes>MAX_IMAGE_DECODE_BYTES as usize { return Err(AssetError::decoded_capacity("PSD composite exceeds decoded byte budget")); }
    // Validate all channel work, including spot channels that are ignored.
    budget.reserve(rgba_bytes)?;
    let color_data=input.block()?;
    if mode==2 && color_data.len()!=768 {return Err(invalid("Indexed PSD requires a 256-color palette"));}
    input.block()?; // image resources
    let layer_mask=input.block()?;
    let has_transparency=if !matches!(mode,0|2) && layer_mask.len()>=6 {
        let mut layer=Reader {data:layer_mask,pos:0};
        let layer_info=layer.block()?;
        layer_info.len()>=2 && i16::from_be_bytes([layer_info[0],layer_info[1]])<0
    } else {false};
    if has_transparency && channels<=colors { return Err(invalid("PSD declares transparency without an alpha channel")); }
    let compression=input.u16()?;
    if compression>3 { return Err(invalid("Unsupported PSD composite compression")); }
    let rows=channels.checked_mul(height as usize).ok_or_else(||invalid("PSD row count overflow"))?;
    let row_lengths=if compression==1 { Some(input.take(rows.checked_mul(2).ok_or_else(||invalid("PSD row table overflow"))?)?) } else { None };
    let mut zip=if compression>=2 {Some(flate2::read::ZlibDecoder::new(&bytes[input.pos..]))} else {None};
    let mut rgba=vec![255u8;rgba_bytes];
    let mut scratch=vec![0u8;row_bytes];
    let mut predictor_scratch=Vec::new();
    for channel in 0..channels {
        for y in 0..height as usize {
            let row=if let Some(decoder)=zip.as_mut() {
                decoder.read_exact(&mut scratch).map_err(|_|invalid("Truncated or invalid PSD ZIP image data"))?;
                if compression==3 {undo_prediction(&mut scratch,depth,width as usize,&mut predictor_scratch);}
                &scratch[..]
            } else if let Some(lengths)=row_lengths {
                let i=(channel*height as usize+y)*2;
                let size=usize::from(u16::from_be_bytes([lengths[i],lengths[i+1]]));
                unpack_row(input.take(size)?, &mut scratch)?;
                &scratch[..]
            } else { input.take(row_bytes)? };
            if channel<colors || (has_transparency && channel==colors) {
                for x in 0..width as usize {
                    let sample=if mode==0 {
                        if row[x/8]&(0x80>>(x%8))!=0 {0} else {255}
                    } else if depth==32 {
                        let i=x*4;
                        let value=f32::from_be_bytes([row[i],row[i+1],row[i+2],row[i+3]]);
                        if value.is_nan() {0} else {(value.clamp(0.0,1.0)*255.0).round() as u8}
                    } else {row[x*usize::from(depth/8)]};
                    let pixel=(y*width as usize+x)*4;
                    if mode==2 && channel==0 {
                        let index=usize::from(sample);
                        rgba[pixel]=color_data[index];rgba[pixel+1]=color_data[256+index];rgba[pixel+2]=color_data[512+index];
                    }
                    else if matches!(mode,0|1|8) && channel==0 {rgba[pixel..pixel+3].fill(sample);}
                    else if mode==4 && channel==3 {
                        // PSD's CMYK composite channels are inverted: C/M/Y
                        // times K yields RGB, as in the OBS FFmpeg decoder.
                        for component in &mut rgba[pixel..pixel+3] {
                            *component=((u32::from(*component)*u32::from(sample)*257)>>16) as u8;
                        }
                    }
                    else if channel<colors {rgba[pixel+channel]=sample;}
                    else {rgba[pixel+3]=sample;}
                }
            }
        }
    }
    if let Some(decoder)=zip.as_mut() {
        let mut extra=[0u8;1];
        if decoder.read(&mut extra).map_err(|_|invalid("Invalid PSD ZIP image data"))?!=0 {
            return Err(invalid("PSD ZIP image data exceeds declared dimensions"));
        }
    }
    RgbaFrame::new(width,height,rgba).map_err(|error|AssetError::caused("image_psd","invalid PSD composite raster",error))
}
