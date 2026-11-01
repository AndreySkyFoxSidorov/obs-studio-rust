//! Windows WIC adapter for bounded JPEG XR image sources.
//! WIC's internal codec allocations are outside the host's explicit RGBA budget.
use super::{dimensions, AssetError, DecodeBudget};
use skylive_media::RgbaFrame;
use std::ptr;
use windows::Win32::{
    Foundation::RPC_E_CHANGED_MODE,
    Graphics::Imaging::{CLSID_WICImagingFactory, GUID_ContainerFormatWmp,
        GUID_WICPixelFormat32bppRGBA, IWICImagingFactory, IWICPalette,
        WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand},
    System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED},
};

fn wic(error: windows::core::Error) -> AssetError {
    AssetError::rejected("image_jxr", format!("Windows JPEG XR decoder: {error}"))
}

struct Apartment(bool);
impl Apartment {
    fn enter() -> Result<Self,AssetError> {
        let result=unsafe {CoInitializeEx(None,COINIT_MULTITHREADED)};
        if result==RPC_E_CHANGED_MODE {return Ok(Self(false));}
        result.ok().map_err(wic)?;
        Ok(Self(true))
    }
}
impl Drop for Apartment {fn drop(&mut self){if self.0 {unsafe {CoUninitialize()}}}}

/// The decoder borrows an owned encoded snapshot, not the live file. Header
/// dimensions are checked before RGBA allocation and before frame publication.
pub(super) fn decode(bytes:&[u8],expected:Option<(u32,u32)>,max_bytes:usize,budget:&mut DecodeBudget)->Result<RgbaFrame,AssetError>{
    let _apartment=Apartment::enter()?;
    unsafe {
        let factory:IWICImagingFactory=CoCreateInstance(&CLSID_WICImagingFactory,None,CLSCTX_INPROC_SERVER).map_err(wic)?;
        let stream=factory.CreateStream().map_err(wic)?;
        stream.InitializeFromMemory(bytes).map_err(wic)?;
        let decoder=factory.CreateDecoderFromStream(&stream,ptr::null(),WICDecodeMetadataCacheOnDemand).map_err(wic)?;
        if decoder.GetContainerFormat().map_err(wic)?!=GUID_ContainerFormatWmp {
            return Err(AssetError::rejected("image_jxr","The selected .jxr file is not a JPEG XR image"));
        }
        let frame=decoder.GetFrame(0).map_err(wic)?;
        let (mut width,mut height)=(0,0);
        frame.GetSize(&mut width,&mut height).map_err(wic)?;
        dimensions(width,height)?;
        if expected.is_some_and(|size|size!=(width,height)) {
            return Err(AssetError::rejected("image_dimensions_changed","JPEG XR dimensions disagree with the recorded asset"));
        }
        let size=(width as usize).checked_mul(height as usize).and_then(|n|n.checked_mul(4)).ok_or_else(||AssetError::decoded_capacity("JPEG XR raster size overflow"))?;
        if size>max_bytes {return Err(AssetError::decoded_capacity("JPEG XR exceeds decoded byte budget"));}
        budget.reserve(size)?;
        let converter=factory.CreateFormatConverter().map_err(wic)?;
        converter.Initialize(&frame,&GUID_WICPixelFormat32bppRGBA,WICBitmapDitherTypeNone,None::<&IWICPalette>,0.0,WICBitmapPaletteTypeCustom).map_err(wic)?;
        let mut rgba=vec![0;size];
        converter.CopyPixels(ptr::null(),width*4,&mut rgba).map_err(wic)?;
        RgbaFrame::new(width,height,rgba).map_err(|error|AssetError::caused("image_jxr","invalid JPEG XR raster",error))
    }
}
