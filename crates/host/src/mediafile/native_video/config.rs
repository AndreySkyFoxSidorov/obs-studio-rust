//! Metadata-only native admission. Existing Rust SPS/PPS/slice parsers stay intact.
use super::*;
use skylive_h264::{params::{Sps,Pps},slice::{NalHeader,SliceHeader}};
use skylive_codec_bits::{unescape_rbsp,length_prefixed_nals};
#[derive(Clone)]
pub(super) struct Configuration {
    pub prefix:usize,pub sps:Sps,pub pps:Pps,pub sps_nal:Vec<u8>,pub pps_nal:Vec<u8>,pub header:Vec<u8>,
    pub width:u32,pub height:u32,pub chroma_left:bool,
}
pub(super) struct AccessUnit {pub bytes:Vec<u8>,pub discard_prior:bool}
impl Configuration {
    pub(super) fn admit(raw:&skylive_mp4::AvcConfig,track:&Track)->Result<Option<Self>,MediaFileError>{
        if raw.sps.len()!=1||raw.pps.len()!=1||!matches!(raw.profile,66|77|100){return Ok(None);}
        let sps_nal=&raw.sps[0];let pps_nal=&raw.pps[0];
        if sps_nal.len()<4||sps_nal.len()>8192||pps_nal.is_empty()||pps_nal.len()>8192||sps_nal[0]&31!=7||pps_nal[0]&31!=8{return Err(MediaFileError::Unsupported("invalid bounded native AVC parameter sets"));}
        let (sps,pps)=catch_unwind(AssertUnwindSafe(||->Result<_,MediaFileError>{
            let sps=Sps::parse(&unescape_rbsp(&sps_nal[1..])).map_err(|e|MediaFileError::Decode(e.to_string()))?;
            let mut table=vec![None;32];table[sps.id as usize]=Some(sps.clone());
            let pps=Pps::parse(&unescape_rbsp(&pps_nal[1..]),&table).map_err(|e|MediaFileError::Decode(e.to_string()))?;Ok((sps,pps))
        })).map_err(|_|MediaFileError::Decode("native AVC header parser invariant failed".into()))??;
        let crop=sps.crop_rect();let params=track.video().ok_or(MediaFileError::Unsupported("native video parameters missing"))?;
        if !sps.frame_mbs_only||sps.separate_colour_plane||sps.chroma_format_idc!=1||sps.bit_depth_luma!=8||sps.bit_depth_chroma!=8||pps.num_slice_groups!=1
            ||sps.profile_idc!=raw.profile||sps.max_num_ref_frames>16||u64::from(sps.width())*u64::from(sps.height())>4_194_304
            ||(crop.2,crop.3)!=(u32::from(params.width),u32::from(params.height))||[crop.0,crop.1,crop.2,crop.3].iter().any(|v|v%2!=0){return Ok(None);}
        let mut chroma_left=true;
        if let Some(vui)=&sps.vui {
            if vui.full_range||![vui.colour_primaries,vui.transfer_characteristics,vui.matrix_coefficients].iter().all(|v|matches!(v,1|2))
                ||(vui.sar!=(0,0)&&(vui.sar.0==0||vui.sar.0!=vui.sar.1)){return Ok(None);}
            chroma_left=match vui.chroma_loc.unwrap_or((0,0)){(0,0)=>true,(1,1)=>false,_=>return Ok(None)};
            if vui.bitstream_restriction.as_ref().is_some_and(|b|b.max_num_reorder_frames>16||b.max_dec_frame_buffering>16){return Ok(None);}
        }
        if let Some(color)=&params.color {let valid=match color {
            skylive_mp4::ColorInfo::Nclx {primaries,transfer,matrix,full_range}=>!*full_range&&[primaries,transfer,matrix].into_iter().all(|v|matches!(v,1|2)),
            skylive_mp4::ColorInfo::Nclc {primaries,transfer,matrix}=>[primaries,transfer,matrix].into_iter().all(|v|matches!(v,1|2)),_=>false};if !valid{return Ok(None);}}
        let mut header=Vec::new();for nal in [sps_nal,pps_nal]{header.extend_from_slice(&[0,0,0,1]);header.extend_from_slice(nal);}
        Ok(Some(Self {prefix:raw.length_size as usize,sps,pps,sps_nal:sps_nal.clone(),pps_nal:pps_nal.clone(),header,width:crop.2,height:crop.3,chroma_left}))
    }
    pub(super) fn access_unit(&self,data:&[u8],first:bool,sync:bool)->Result<AccessUnit,MediaFileError>{
        if data.len()>2*1024*1024{return Err(MediaFileError::Limit("native AVC compressed packet"));}
        let nals=length_prefixed_nals(data,self.prefix).map_err(|e|MediaFileError::Decode(e.to_string()))?;
        if nals.len()>1024{return Err(MediaFileError::Limit("native AVC NAL count"));}
        let mut identity=None;let mut previous_mb=None;let mut picture=false;let mut discard_prior=false;
        let mut bytes=if first{self.header.clone()}else{Vec::new()};
        for nal in nals {
            let header=NalHeader::parse(*nal.first().ok_or(MediaFileError::Unsupported("empty native AVC NAL"))?).map_err(|e|MediaFileError::Decode(e.to_string()))?;
            match header.nal_unit_type {
                1|5=>{
                    let rbsp=unescape_rbsp(&nal[1..]);
                    let (slice,_,_)=SliceHeader::parse(&rbsp,header,|id|if id==self.pps.id {Ok((&self.pps,&self.sps))}else{Err(skylive_h264::Error::MissingParameterSet("native AVC PPS changed".into()))}).map_err(|e|MediaFileError::Decode(e.to_string()))?;
                    let id=(slice.pps_id,slice.frame_num,slice.idr,slice.idr_pic_id,slice.pic_order_cnt_lsb,slice.delta_pic_order_cnt_bottom,slice.delta_pic_order_cnt,slice.nal_ref_idc!=0);
                    if slice.field_pic||slice.redundant_pic_cnt!=0||slice.first_mb_in_slice>=self.sps.pic_width_in_mbs*self.sps.frame_height_in_mbs()
                        ||previous_mb.is_some_and(|p|slice.first_mb_in_slice<=p)||(!picture&&slice.first_mb_in_slice!=0)||identity.as_ref().is_some_and(|old|*old!=id)
                        ||(first&&(!sync||!slice.idr)){return Err(MediaFileError::Unsupported("native AVC requires one complete ordered-slice picture per sample and an initial sync IDR"));}
                    discard_prior|=slice.idr&&slice.no_output_of_prior_pics;identity=Some(id);previous_mb=Some(slice.first_mb_in_slice);picture=true;
                },
                7 if nal!=self.sps_nal.as_slice()=>return Err(MediaFileError::Unsupported("native AVC SPS changes require source re-preparation")),
                8 if nal!=self.pps_nal.as_slice()=>return Err(MediaFileError::Unsupported("native AVC PPS changes require source re-preparation")),
                6=>check_sei(&nal[1..])?,
                7|8|9|10|11|12=>{},
                _=>return Err(MediaFileError::Unsupported("native AVC NAL extension or partitioning unsupported")),
            }
            if bytes.len().checked_add(nal.len()+4).is_none_or(|n|n>2*1024*1024+65536){return Err(MediaFileError::Limit("native AnnexB packet"));}
            bytes.extend_from_slice(&[0,0,0,1]);bytes.extend_from_slice(nal);
        }
        if !picture{return Err(MediaFileError::Unsupported("native AVC sample contains no picture"));}
        Ok(AccessUnit {bytes,discard_prior})
    }
}

// FFmpeg's supplied libavcodec/sei.h supplies these standardized payload IDs;
// no decoder or C implementation is linked. Do not silently ignore display/HDR
// processing outside this native source's explicitly admitted SDR scope.
fn check_sei(bytes:&[u8])->Result<(),MediaFileError>{
    let rbsp=unescape_rbsp(bytes);let mut at=0usize;let mut messages=0usize;
    while at<rbsp.len(){
        if at+1==rbsp.len()&&rbsp[at]==0x80{return Ok(());}
        let read=|at:&mut usize|->Result<usize,MediaFileError>{let mut value=0usize;loop{let byte=*rbsp.get(*at).ok_or_else(||MediaFileError::Decode("truncated native SEI field".into()))?;*at+=1;value=value.checked_add(byte as usize).ok_or(MediaFileError::Limit("native SEI field"))?;if byte!=255{return Ok(value);}}};
        let kind=read(&mut at)?;let count=read(&mut at)?;messages+=1;if messages>256{return Err(MediaFileError::Limit("native SEI message count"));}
        at=at.checked_add(count).filter(|n|*n<=rbsp.len()).ok_or_else(||MediaFileError::Decode("truncated native SEI payload".into()))?;
        if matches!(kind,45|47|137|138|144|147){return Err(MediaFileError::Unsupported("native media SEI requires unsupported orientation/stereo/HDR display processing"));}
    }
    Err(MediaFileError::Decode("native SEI trailing bits missing".into()))
}
