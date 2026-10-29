//! Linked image/GIF animation I/O and decoding. Call only on a host-owned worker.
//!
//! Owns no canonical mutation and never writes files. A prepared asset records an
//! absolute, exact UTF-8 path, encoded-byte SHA-256 and validated raster dimensions.
//! Reload reads one owned byte snapshot and checks its hash before decoding; file
//! replacement during later rendering cannot change that accepted frame.
//! Recorded dimensions are checked against decoder headers before raster decode,
//! so the host's dimension-based decoded-work admission cannot be understated by
//! a forged asset descriptor. The postdecode dimensions are checked defensively.
//! GIF frame rectangles must fit their logical screen, and actual WebP VP8/VP8L
//! coded dimensions must agree with an extended VP8X canvas before codec decode.
//! These subimage checks prevent a tiny advertised canvas hiding a large raster.
//!
//! Explicit bounds: encoded input 32 MiB, edge 8192, 16,777,216 pixels, native
//! decoded output 64 MiB and final RGBA 64 MiB. image 0.25.10's max_alloc=64 MiB
//! is a BEST-EFFORT decoder limit (only dimension limits are strict). PNG receives
//! limits at construction. Native->RGBA conversion can overlap two 64 MiB buffers;
//! JPEG additionally copies the <=32 MiB encoded input in its decoder constructor.
//! Encoded staging/read growth and codec scratch are not a process-wide memory
//! guarantee. GIF clips retain at most 512 complete RGBA frames / 128 MiB, with
//! canvas/count/byte admission before frame iteration. GIF decoder disposal
//! scratch can overlap up to three logical canvases; it is separate from retained
//! frame bytes. APNG and animated WebP use the same retained frame/count limits;
//! WebP additionally admits three full canvases of raster workspace before
//! decoding. Codec-internal scratch remains separate. No ICC conversion, EXIF autorotation or HDR
//! fidelity is claimed. GIF delay/loop policy follows OBS's libnsgif/image-file.
//!
//! Share one `ReadBudget` across a host operation to bound cumulative encoded
//! work, including failed reads, hashes and decodes. Each file cap is
//! min(32 MiB, remaining). A growth-detection sentinel is included only when the
//! remaining shared allowance can pay for it; every actually read byte is charged.
//! At an exact shared-budget boundary no extra byte is read: current length is
//! rechecked on the SAME open handle. This is an owned byte snapshot, not a
//! filesystem transaction; another writer can still replace/modify a file later.
use image::{DynamicImage, ImageDecoder, ImageFormat, Limits};
use sha2::{Digest, Sha256};
use skylive_core::ImageAsset;
use skylive_media::{AnimationClip, MediaTime, RgbaFrame};
use std::{
    error::Error,
    fmt, fs,
    io::{Cursor, Read},
    path::Path,
    sync::Arc,
};

mod gif;
mod webp_animation;
mod apng;

pub const MAX_IMAGE_FILE_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_IMAGE_PIXELS: u64 = 16_777_216;
pub const MAX_IMAGE_EDGE: u32 = 8192;
pub const MAX_IMAGE_DECODE_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_ANIMATION_FRAMES: usize = 512;
pub const MAX_ANIMATION_BYTES: usize = 128 * 1024 * 1024;

/// Encoded bytes consumed by one host operation, including failed resources.
/// Owners choose its scope; constructing a new budget per source defeats a
/// multi-resource operation's aggregate admission contract.
#[derive(Debug)]
pub struct ReadBudget {
    remaining: u64,
}
impl ReadBudget {
    pub fn new(max_bytes: u64) -> Self {
        Self {
            remaining: max_bytes,
        }
    }
    pub fn remaining(&self) -> u64 {
        self.remaining
    }
}

/// Logical RGBA work admitted across resources, including a clip that later
/// fails decoding. Charges are never refunded when an operation fails.
#[derive(Debug)]
pub struct DecodeBudget {
    remaining: usize,
}
impl DecodeBudget {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            remaining: max_bytes,
        }
    }
    pub fn remaining(&self) -> usize {
        self.remaining
    }
    fn reserve(&mut self, bytes: usize) -> Result<(), AssetError> {
        if bytes > self.remaining {
            return Err(AssetError::limit(
                "image_decode_budget",
                "aggregate decoded image work budget is exhausted",
            ));
        }
        self.remaining -= bytes;
        Ok(())
    }
}

#[derive(Debug)]
pub struct AssetError {
    kind: crate::resource_issue::ResourceIssueKind,
    code: &'static str,
    detail: String,
    cause: Option<Box<dyn Error + Send + Sync>>,
    decoded_capacity: bool,
}
impl AssetError {
    pub fn kind(&self) -> crate::resource_issue::ResourceIssueKind {
        self.kind
    }
    fn with_kind(mut self, kind: crate::resource_issue::ResourceIssueKind) -> Self {
        self.kind = kind;
        self
    }
    fn limit(code: &'static str, detail: impl Into<String>) -> Self {
        Self::rejected(code, detail).with_kind(crate::resource_issue::ResourceIssueKind::Limit)
    }
    fn decoded_capacity(detail: impl Into<String>) -> Self {
        let mut error=Self::limit("image_limit",detail);
        error.decoded_capacity=true;
        error
    }
    pub(crate) fn is_decoded_capacity(&self)->bool {self.decoded_capacity}
    pub fn code(&self) -> &'static str {
        self.code
    }
    pub fn detail(&self) -> &str {
        &self.detail
    }
    fn rejected(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            kind: crate::resource_issue::ResourceIssueKind::Unavailable,
            code,
            detail: detail.into(),
            cause: None,
            decoded_capacity: false,
        }
    }
    fn caused(
        code: &'static str,
        context: impl fmt::Display,
        cause: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind: crate::resource_issue::ResourceIssueKind::Unavailable,
            code,
            detail: format!("{context}: {cause}"),
            cause: Some(Box::new(cause)),
            decoded_capacity: false,
        }
    }
}
impl fmt::Display for AssetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.detail)
    }
}
impl Error for AssetError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.cause
            .as_ref()
            .map(|cause| cause.as_ref() as &(dyn Error + 'static))
    }
}

#[derive(Debug)]
pub struct PreparedImage {
    pub asset: ImageAsset,
    pub frame: Arc<RgbaFrame>,
    /// Full decoded clip bytes, not only frame zero; operation owners use this
    /// for cumulative decoded-work admission while publishing only metadata.
    pub decoded_bytes: usize,
}

fn read_owned(path: &Path, budget: &mut ReadBudget) -> Result<Box<[u8]>, AssetError> {
    if budget.remaining() == 0 {
        return Err(AssetError::limit(
            "image_read_budget",
            "aggregate encoded image read budget is exhausted",
        ));
    }
    let mut file = fs::File::open(path)
        .map_err(|error| AssetError::caused("image_io", path.display(), error))?;
    let metadata = file
        .metadata()
        .map_err(|error| AssetError::caused("image_io", path.display(), error))?;
    if !metadata.is_file() {
        return Err(AssetError::rejected(
            "image_io",
            format!("{} is not a regular file", path.display()),
        ));
    }
    if metadata.len() > MAX_IMAGE_FILE_BYTES {
        return Err(AssetError::limit(
            "image_limit",
            "image file exceeds 32 MiB",
        ));
    }
    if metadata.len() > budget.remaining() {
        return Err(AssetError::limit(
            "image_read_budget",
            "image file exceeds remaining aggregate encoded read budget",
        ));
    }
    let (bytes, exact_boundary) =
        read_with_budget(&mut file, budget, MAX_IMAGE_FILE_BYTES, metadata.len())?;
    if exact_boundary {
        let current = file
            .metadata()
            .map_err(|error| AssetError::caused("image_io", path.display(), error))?;
        check_exact_boundary(current.len(), bytes.len() as u64)?;
    }
    Ok(bytes)
}

fn check_exact_boundary(current_len: u64, captured_len: u64) -> Result<(), AssetError> {
    if current_len > captured_len {
        return Err(AssetError::limit(
            "image_read_budget",
            "image grew beyond the remaining read budget; no uncharged sentinel was read",
        ));
    }
    Ok(())
}

/// `exact_boundary` requires the file owner to recheck same-handle metadata.
fn read_with_budget(
    reader: &mut impl Read,
    budget: &mut ReadBudget,
    file_limit: u64,
    size_hint: u64,
) -> Result<(Box<[u8]>, bool), AssetError> {
    if budget.remaining() == 0 {
        return Err(AssetError::limit(
            "image_read_budget",
            "aggregate encoded image read budget is exhausted",
        ));
    }
    let cap = file_limit.min(budget.remaining());
    let allowance = cap.saturating_add(1).min(budget.remaining());
    let mut bytes = Vec::with_capacity(size_hint.min(cap) as usize);
    let result = reader.take(allowance).read_to_end(&mut bytes);
    let consumed = bytes.len() as u64;
    // `take` cannot read beyond the shared allowance, even on a partial I/O
    // failure. Charge before examining I/O/format/hash outcomes.
    budget.remaining -= consumed;
    result.map_err(|error| AssetError::caused("image_io", "reading linked image bytes", error))?;
    if consumed > cap {
        return Err(AssetError::limit(
            "image_limit",
            "image file grew beyond its encoded file cap while reading; sentinel byte charged",
        ));
    }
    Ok((
        bytes.into_boxed_slice(),
        consumed == cap && allowance == cap,
    ))
}

/// Prepare a linked source without publishing it into a Session.
/// Uses a fresh single-file budget; multi-resource owners use the shared variant.
pub fn prepare_image(path: &Path) -> Result<PreparedImage, AssetError> {
    prepare_image_with_budget(path, &mut ReadBudget::new(MAX_IMAGE_FILE_BYTES))
}

/// Charge encoded I/O even if the candidate subsequently fails hash/decoding.
pub fn prepare_image_with_budget(
    path: &Path,
    budget: &mut ReadBudget,
) -> Result<PreparedImage, AssetError> {
    prepare_image_with_limits(path, budget, MAX_ANIMATION_BYTES)
}

/// Admit an entire GIF/static raster against the caller's remaining decoded
/// budget before frame iteration. All encoded bytes use the shared hard reader.
pub fn prepare_image_with_limits(
    path: &Path,
    budget: &mut ReadBudget,
    max_decoded_bytes: usize,
) -> Result<PreparedImage, AssetError> {
    let absolute = std::path::absolute(path)
        .map_err(|error| AssetError::caused("image_path", path.display(), error))?;
    let path_string = absolute
        .to_str()
        .ok_or_else(|| AssetError::rejected("image_path", "image path is not exact UTF-8"))?;
    let bytes = read_owned(&absolute, budget)?;
    let hash: [u8; 32] = Sha256::digest(&bytes).into();
    let mut decoded_work = DecodeBudget::new(max_decoded_bytes.min(MAX_ANIMATION_BYTES));
    let clip = decode_clip(
        &bytes,
        &absolute,
        None,
        max_decoded_bytes,
        &mut decoded_work,
    )?;
    let decoded_bytes = clip.byte_len();
    let frame = clip.sample(MediaTime(0)).image;
    let asset = ImageAsset::new(path_string.to_owned(), hash, frame.width(), frame.height())
        .map_err(|error| {
            AssetError::caused("image_asset", "invalid prepared image asset", error)
        })?;
    Ok(PreparedImage {
        asset,
        frame,
        decoded_bytes,
    })
}

/// Prepare a complete bounded mask clip, retaining every decoded
/// frame. This is an I/O-worker primitive; publication remains a separate step.
pub fn prepare_mask_clip(path:&Path,max_decoded_bytes:usize)->Result<(ImageAsset,Arc<AnimationClip>),AssetError>{
    let absolute=std::path::absolute(path).map_err(|error|AssetError::caused("image_path",path.display(),error))?;
    let path_string=absolute.to_str().ok_or_else(||AssetError::rejected("image_path","mask path is not exact UTF-8"))?;
    let bytes=read_owned(&absolute,&mut ReadBudget::new(MAX_IMAGE_FILE_BYTES))?;
    let hash:[u8;32]=Sha256::digest(&bytes).into();
    let mut decoded=DecodeBudget::new(max_decoded_bytes.min(MAX_ANIMATION_BYTES));
    let clip=decode_clip(&bytes,&absolute,None,max_decoded_bytes,&mut decoded)?;
    let asset=ImageAsset::new(path_string.to_owned(),hash,clip.width(),clip.height()).map_err(|error|AssetError::caused("image_asset","invalid mask clip asset",error))?;
    if clip.is_animated()&&!mask_animation_candidate(asset.path()){return Err(AssetError::rejected("image_mask_animation","Animated masks require a GIF, PNG, APNG or WebP extension"));}
    Ok((asset,clip))
}
/// Animation-capable extensions tracked by the mask timeline. The actual
/// content is checked separately by the decoder, not inferred from its name.
pub(crate) fn mask_animation_candidate(path:&str)->bool{
    Path::new(path).extension().and_then(|v|v.to_str()).is_some_and(|v|
        ["gif","png","apng","webp"].iter().any(|extension|v.eq_ignore_ascii_case(extension)))
}
/// Mask preparation validates the entire clip; the returned frame is a preview.
pub fn prepare_mask_image(path:&Path)->Result<PreparedImage,AssetError>{
    prepare_mask_image_with_limits(path,&mut ReadBudget::new(MAX_IMAGE_FILE_BYTES),MAX_IMAGE_DECODE_BYTES as usize)
}
pub fn prepare_mask_image_with_limits(path:&Path,budget:&mut ReadBudget,max_decoded_bytes:usize)->Result<PreparedImage,AssetError>{
    let prepared=prepare_image_with_limits(path,budget,max_decoded_bytes)?;
    if prepared.decoded_bytes>prepared.frame.byte_len()&&!mask_animation_candidate(prepared.asset.path()){return Err(AssetError::rejected("image_mask_animation","Animated masks require a GIF, PNG, APNG or WebP extension"));}
    Ok(prepared)
}
/// Read one bounded, stable-by-metadata snapshot of a live mask path.
/// Metadata cannot prevent a writer from changing bytes and restoring stamps;
/// the digest nevertheless always identifies exactly the bytes decoded.
pub(crate) fn read_live_mask_clip(asset:&ImageAsset,reads:&mut ReadBudget)->Result<(Box<[u8]>,[u8;32]),AssetError>{
    let path=Path::new(asset.path());
    if !path.is_absolute(){return Err(AssetError::rejected("image_path","linked mask path must be absolute"));}
    let stamp=|metadata:&fs::Metadata|->Result<(u64,std::time::SystemTime),AssetError>{Ok((metadata.len(),metadata.modified().map_err(|e|AssetError::caused("image_io",path.display(),e))?))};
    let before=stamp(&fs::metadata(path).map_err(|e|AssetError::caused("image_io",path.display(),e))?)?;
    let mut file=fs::File::open(path).map_err(|e|AssetError::caused("image_io",path.display(),e))?;
    let meta=file.metadata().map_err(|e|AssetError::caused("image_io",path.display(),e))?;
    if !meta.is_file(){return Err(AssetError::rejected("image_io","mask path is not a regular file"));}
    if stamp(&meta)?!=before{return Err(AssetError::rejected("image_changed","mask file changed before reading"));}
    if meta.len()>MAX_IMAGE_FILE_BYTES{return Err(AssetError::limit("image_limit","image file exceeds 32 MiB"));}
    if meta.len()>reads.remaining(){return Err(AssetError::limit("image_read_budget","image file exceeds remaining aggregate encoded read budget"));}
    let (bytes,exact)=read_with_budget(&mut file,reads,MAX_IMAGE_FILE_BYTES,meta.len())?;
    if exact{let current=file.metadata().map_err(|e|AssetError::caused("image_io",path.display(),e))?;check_exact_boundary(current.len(),bytes.len() as u64)?;}
    let after=stamp(&fs::metadata(path).map_err(|e|AssetError::caused("image_io",path.display(),e))?)?;
    if before!=after{return Err(AssetError::rejected("image_changed","mask file changed while reading"));}
    let hash:[u8;32]=Sha256::digest(&bytes).into();Ok((bytes,hash))
}
pub(crate) fn decode_live_mask_clip(asset:&ImageAsset,bytes:&[u8],decoded:&mut DecodeBudget,max_decoded_bytes:usize)->Result<Arc<AnimationClip>,AssetError>{
    let path=Path::new(asset.path());
    let clip=decode_clip(bytes,path,Some((asset.width(),asset.height())),max_decoded_bytes,decoded).map_err(mask_decode_error)?;
    if clip.is_animated()&&!mask_animation_candidate(asset.path()){return Err(AssetError::rejected("image_mask_animation","Animated masks require a GIF, PNG, APNG or WebP extension"));}
    Ok(clip)
}
pub(crate) fn load_mask_clip_with_budgets(asset:&ImageAsset,reads:&mut ReadBudget,decoded:&mut DecodeBudget,max_decoded_bytes:usize,cached:Option<&Arc<AnimationClip>>)->Result<Arc<AnimationClip>,AssetError>{
    let path=Path::new(asset.path());if !path.is_absolute(){return Err(AssetError::rejected("image_path","linked mask path must be absolute"));}
    let bytes=read_owned(path,reads)?;let hash:[u8;32]=Sha256::digest(&bytes).into();
    if &hash!=asset.sha256(){return Err(AssetError::rejected("image_changed",format!("linked mask bytes changed: {}",asset.path())));}
    if let Some(clip)=cached{return Ok(clip.clone());}
    let clip=decode_clip(&bytes,path,Some((asset.width(),asset.height())),max_decoded_bytes,decoded).map_err(mask_decode_error)?;
    if clip.is_animated()&&!mask_animation_candidate(asset.path()){return Err(AssetError::rejected("image_mask_animation","Animated masks require a GIF, PNG, APNG or WebP extension"));}
    Ok(clip)
}pub(crate) fn load_mask_image_with_budgets(asset:&ImageAsset,reads:&mut ReadBudget,decoded:&mut DecodeBudget,max_decoded_bytes:usize,cached:Option<&Arc<RgbaFrame>>)->Result<Arc<RgbaFrame>,AssetError>{
    let path=Path::new(asset.path());if !path.is_absolute(){return Err(AssetError::rejected("image_path","linked mask path must be absolute"));}
    let bytes=read_owned(path,reads)?;let hash:[u8;32]=Sha256::digest(&bytes).into();
    if &hash!=asset.sha256(){return Err(AssetError::rejected("image_changed",format!("linked mask bytes changed: {}",asset.path())));}
    if let Some(frame)=cached {return Ok(frame.clone());}
    Ok(Arc::new(decode(&bytes,path,Some((asset.width(),asset.height())),max_decoded_bytes,decoded).map_err(mask_decode_error)?))
}

fn mask_decode_error(error:AssetError)->AssetError {if error.code()=="image_animation"{AssetError::rejected("image_mask_animation","This animated mask encoding is not supported.")}else{error}}

/// Load exactly the linked identity; never silently replace changed pixels.
/// Uses a fresh single-file budget; multi-resource owners use the shared variant.
pub fn load_image(asset: &ImageAsset) -> Result<Arc<RgbaFrame>, AssetError> {
    load_image_with_budget(asset, &mut ReadBudget::new(MAX_IMAGE_FILE_BYTES))
}

/// The caller keeps one budget across all distinct resources in an operation.
pub fn load_image_with_budget(
    asset: &ImageAsset,
    budget: &mut ReadBudget,
) -> Result<Arc<RgbaFrame>, AssetError> {
    Ok(load_clip_with_budget(asset, budget, MAX_ANIMATION_BYTES)?
        .sample(MediaTime(0))
        .image)
}

/// Load the complete immutable animation from the exact owned bytes identified
/// by the asset. The caller supplies its remaining retained/decoded clip budget.
pub fn load_clip_with_budget(
    asset: &ImageAsset,
    budget: &mut ReadBudget,
    max_decoded_bytes: usize,
) -> Result<Arc<AnimationClip>, AssetError> {
    load_clip_with_budgets(
        asset,
        budget,
        &mut DecodeBudget::new(max_decoded_bytes.min(MAX_ANIMATION_BYTES)),
        max_decoded_bytes,
    )
}

/// Keep both budgets across an operation. Full GIF logical-frame work is
/// charged before iteration and remains charged on a malformed late frame.
pub fn load_clip_with_budgets(
    asset: &ImageAsset,
    budget: &mut ReadBudget,
    decoded_work: &mut DecodeBudget,
    max_decoded_bytes: usize,
) -> Result<Arc<AnimationClip>, AssetError> {
    let path = Path::new(asset.path());
    if !path.is_absolute() {
        return Err(AssetError::rejected(
            "image_path",
            format!(
                "linked image path is not absolute on this host: {}",
                asset.path()
            ),
        ));
    }
    let bytes = read_owned(path, budget)?;
    let hash: [u8; 32] = Sha256::digest(&bytes).into();
    if &hash != asset.sha256() {
        return Err(AssetError::rejected(
            "image_changed",
            format!("linked image bytes changed: {}", asset.path()),
        ));
    }
    let clip = decode_clip(
        &bytes,
        path,
        Some((asset.width(), asset.height())),
        max_decoded_bytes,
        decoded_work,
    )?;
    if clip.width() != asset.width() || clip.height() != asset.height() {
        return Err(AssetError::rejected(
            "image_dimensions_changed",
            "linked image raster dimensions disagree with the recorded asset",
        ));
    }
    Ok(clip)
}

fn decode_clip(
    bytes: &[u8],
    path: &Path,
    expected: Option<(u32, u32)>,
    max_decoded_bytes: usize,
    decoded_work: &mut DecodeBudget,
) -> Result<Arc<AnimationClip>, AssetError> {
    let format = detect(bytes, path)?;
    if format == ImageFormat::Gif {
        return gif::decode_clip(bytes, expected, max_decoded_bytes, decoded_work);
    }
    if format == ImageFormat::WebP && webp_animation::is_animated(bytes) {
        return webp_animation::decode_clip(bytes, expected, max_decoded_bytes, decoded_work);
    }
    if format == ImageFormat::Png {
        if let Some(clip) = apng::decode_if_animated(bytes, expected, max_decoded_bytes, decoded_work)? {
            return Ok(clip);
        }
    }
    // Static image header checking remains in finish(). A caller-provided
    // expected identity is checked before raster allocation there.
    if let Some((width, height)) = expected {
        if u64::from(width) * u64::from(height) * 4 > max_decoded_bytes as u64 {
            return Err(AssetError::decoded_capacity(
                "static image exceeds remaining decoded byte budget",
            ));
        }
    }
    let frame = decode(bytes, path, expected, max_decoded_bytes, decoded_work)?;
    if frame.byte_len() > max_decoded_bytes {
        return Err(AssetError::decoded_capacity(
            "static image exceeds remaining decoded byte budget",
        ));
    }
    Ok(Arc::new(AnimationClip::static_frame(Arc::new(frame))))
}

fn limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_EDGE);
    limits.max_image_height = Some(MAX_IMAGE_EDGE);
    limits.max_alloc = Some(MAX_IMAGE_DECODE_BYTES);
    limits
}

fn dimensions(width: u32, height: u32) -> Result<(), AssetError> {
    if width == 0
        || height == 0
        || width > MAX_IMAGE_EDGE
        || height > MAX_IMAGE_EDGE
        || u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS
    {
        return Err(AssetError::limit(
            "image_limit",
            format!(
                "unsupported image dimensions {width}x{height}; max edge 8192, max pixels 16777216"
            ),
        ));
    }
    Ok(())
}

fn decoder_error(error: image::ImageError) -> AssetError {
    let limited = matches!(&error, image::ImageError::Limits(_));
    let code = if limited {
        "image_limit"
    } else {
        "image_decode"
    };
    AssetError::caused(code, "image decoder", error).with_kind(if limited {
        crate::resource_issue::ResourceIssueKind::Limit
    } else {
        crate::resource_issue::ResourceIssueKind::Unavailable
    })
}

fn finish(
    mut decoder: impl ImageDecoder,
    expected: Option<(u32, u32)>,
    max_decoded_bytes: usize,
    decoded_work: &mut DecodeBudget,
) -> Result<RgbaFrame, AssetError> {
    let (width, height) = decoder.dimensions();
    dimensions(width, height)?;
    if expected.is_some_and(|expected| expected != (width, height)) {
        return Err(AssetError::rejected(
            "image_dimensions_changed",
            "linked image header dimensions disagree with the recorded asset before pixel decoding",
        ));
    }
    if u64::from(width) * u64::from(height) * 4 > max_decoded_bytes as u64 {
        return Err(AssetError::decoded_capacity(
            "static image exceeds remaining decoded byte budget before raster allocation",
        ));
    }
    let native_bytes = decoder.total_bytes();
    if native_bytes > MAX_IMAGE_DECODE_BYTES {
        return Err(AssetError::limit(
            "image_limit",
            "native decoded raster exceeds 64 MiB",
        ));
    }
    decoded_work.reserve((u64::from(width) * u64::from(height) * 4) as usize)?;
    let mut budget = limits();
    budget.reserve(native_bytes).map_err(decoder_error)?;
    decoder.set_limits(budget).map_err(decoder_error)?;
    let rgba = DynamicImage::from_decoder(decoder)
        .map_err(decoder_error)?
        .into_rgba8();
    RgbaFrame::new(width, height, rgba.into_raw())
        .map_err(|error| AssetError::caused("image_decode", "invalid decoded RGBA frame", error))
}

fn plausible_tga(bytes: &[u8]) -> bool {
    bytes.len() >= 18
        && bytes[1] <= 1
        && [1, 2, 3, 9, 10, 11].contains(&bytes[2])
        && [8, 16, 24, 32].contains(&bytes[16])
        && bytes[17] & 0xc0 == 0
        && u16::from_le_bytes([bytes[12], bytes[13]]) != 0
        && u16::from_le_bytes([bytes[14], bytes[15]]) != 0
}

fn detect(bytes: &[u8], path: &Path) -> Result<ImageFormat, AssetError> {
    match image::guess_format(bytes) {
        Ok(
            format @ (ImageFormat::Png
            | ImageFormat::Jpeg
            | ImageFormat::Bmp
            | ImageFormat::Gif
            | ImageFormat::WebP),
        ) => Ok(format),
        Ok(format) => Err(AssetError::rejected(
            "image_format",
            format!("unsupported image format: {format:?}"),
        )),
        Err(_)
            if plausible_tga(bytes)
                && (bytes.ends_with(b"TRUEVISION-XFILE.\0")
                    || path
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("tga"))) =>
        {
            Ok(ImageFormat::Tga)
        }
        Err(error) => Err(AssetError::caused(
            "image_format",
            "unrecognized image content (header-only TGA requires .tga)",
            error,
        )),
    }
}

fn decode(
    bytes: &[u8],
    path: &Path,
    expected: Option<(u32, u32)>,
    max_decoded_bytes: usize,
    decoded_work: &mut DecodeBudget,
) -> Result<RgbaFrame, AssetError> {
    use image::codecs::{
        bmp::BmpDecoder, jpeg::JpegDecoder, png::PngDecoder, tga::TgaDecoder, webp::WebPDecoder,
    };
    let cursor = Cursor::new(bytes);
    match detect(bytes, path)? {
        ImageFormat::Png => {
            let decoder = PngDecoder::with_limits(cursor, limits()).map_err(decoder_error)?;
            if decoder.is_apng().map_err(decoder_error)? {
                return Err(animated());
            }
            finish(decoder, expected, max_decoded_bytes, decoded_work)
        }
        ImageFormat::WebP => {
            check_static_webp(bytes)?;
            let decoder = WebPDecoder::new(cursor).map_err(decoder_error)?;
            if decoder.has_animation() {
                return Err(animated());
            }
            finish(decoder, expected, max_decoded_bytes, decoded_work)
        }
        ImageFormat::Jpeg => finish(
            JpegDecoder::new(cursor).map_err(decoder_error)?,
            expected,
            max_decoded_bytes,
            decoded_work,
        ),
        ImageFormat::Bmp => finish(
            BmpDecoder::new(cursor).map_err(decoder_error)?,
            expected,
            max_decoded_bytes,
            decoded_work,
        ),
        ImageFormat::Tga => finish(
            TgaDecoder::new(cursor).map_err(decoder_error)?,
            expected,
            max_decoded_bytes,
            decoded_work,
        ),
        _ => Err(AssetError::rejected(
            "image_format",
            "unsupported image format",
        )),
    }
}

fn animated() -> AssetError {
    AssetError::rejected(
        "image_animation",
        "APNG and animated WebP require additional providers; only GIF animation is supported",
    )
}
fn malformed() -> AssetError {
    AssetError::rejected("image_decode", "malformed or truncated image container")
}
fn take<'a>(bytes: &'a [u8], position: &mut usize, count: usize) -> Result<&'a [u8], AssetError> {
    let end = position.checked_add(count).ok_or_else(malformed)?;
    let slice = bytes.get(*position..end).ok_or_else(malformed)?;
    *position = end;
    Ok(slice)
}
fn sub_blocks(bytes: &[u8], position: &mut usize) -> Result<(), AssetError> {
    loop {
        let count = usize::from(take(bytes, position, 1)?[0]);
        if count == 0 {
            return Ok(());
        }
        take(bytes, position, count)?;
    }
}

/// Inspect bounded RIFF chunks and actual coded dimensions before WebP can
/// decode its default first frame. An extended canvas is not an allocation
/// bound on its embedded VP8 frame: the codec checks that mismatch AFTER decode.
fn check_static_webp(bytes: &[u8]) -> Result<(), AssetError> {
    let mut position = 0;
    let header = take(bytes, &mut position, 12)?;
    let declared = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as u64 + 8;
    if declared != bytes.len() as u64 {
        return Err(malformed());
    }
    let mut canvas = None;
    let mut coded = None;
    while position < bytes.len() {
        let header = take(bytes, &mut position, 8)?;
        let size = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
        let payload = take(bytes, &mut position, size)?;
        if &header[..4] == b"ANIM"
            || &header[..4] == b"ANMF"
            || (&header[..4] == b"VP8X" && payload.first().is_some_and(|flags| flags & 2 != 0))
        {
            return Err(animated());
        }
        match &header[..4] {
            b"VP8X" => {
                if payload.len() != 10 || canvas.is_some() || coded.is_some() {
                    return Err(malformed());
                }
                let width = 1 + u32::from_le_bytes([payload[4], payload[5], payload[6], 0]);
                let height = 1 + u32::from_le_bytes([payload[7], payload[8], payload[9], 0]);
                dimensions(width, height)?;
                canvas = Some((width, height));
            }
            b"VP8 " | b"VP8L" => {
                if coded.is_some() {
                    return Err(AssetError::rejected(
                        "image_decode",
                        "static WebP contains multiple coded image chunks",
                    ));
                }
                let dimensions = webp_coded_dimensions(&header[..4], payload)?;
                if let Some(canvas) = canvas.filter(|canvas| *canvas != dimensions) {
                    return Err(AssetError::rejected(
                        "image_decode",
                        format!(
                            "WebP coded frame dimensions {}x{} disagree with VP8X canvas dimensions {}x{} before pixel decoding",
                            dimensions.0, dimensions.1, canvas.0, canvas.1
                        ),
                    ));
                }
                coded = Some(dimensions);
            }
            _ => {}
        }
        if size % 2 != 0 {
            take(bytes, &mut position, 1)?;
        }
    }
    if coded.is_none() {
        return Err(malformed());
    }
    Ok(())
}

fn webp_coded_dimensions(kind: &[u8], payload: &[u8]) -> Result<(u32, u32), AssetError> {
    let (width, height) = match kind {
        b"VP8 " => {
            if payload.len() < 10 || payload[0] & 1 != 0 || &payload[3..6] != b"\x9d\x01\x2a" {
                return Err(AssetError::rejected(
                    "image_decode",
                    "WebP VP8 payload is not a complete key-frame header",
                ));
            }
            (
                u32::from(u16::from_le_bytes([payload[6], payload[7]]) & 0x3fff),
                u32::from(u16::from_le_bytes([payload[8], payload[9]]) & 0x3fff),
            )
        }
        b"VP8L" => {
            if payload.len() < 5 || payload[0] != 0x2f {
                return Err(malformed());
            }
            let bits = u32::from_le_bytes([payload[1], payload[2], payload[3], payload[4]]);
            if bits >> 29 != 0 {
                return Err(AssetError::rejected(
                    "image_decode",
                    "unsupported WebP VP8L bitstream version",
                ));
            }
            ((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1)
        }
        _ => return Err(malformed()),
    };
    dimensions(width, height)?;
    Ok((width, height))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ExtendedColorType, ImageEncoder};
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };
    static SERIAL: AtomicU64 = AtomicU64::new(1);
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "skylive-image-{}-{}",
                std::process::id(),
                SERIAL.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, bytes).unwrap();
            path
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn png() -> Vec<u8> {
        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(
                &[255, 0, 0, 128, 0, 255, 0, 255],
                2,
                1,
                ExtendedColorType::Rgba8,
            )
            .unwrap();
        bytes
    }

    #[test]
    fn aggregate_budget_charges_failed_decode_and_hash_and_stops_after_exhaustion() {
        let directory = Temp::new();
        let path = directory.write("not-image.png", b"not image");
        let mut budget = ReadBudget::new(20);
        assert_eq!(
            prepare_image_with_budget(&path, &mut budget)
                .unwrap_err()
                .code(),
            "image_format"
        );
        assert_eq!(budget.remaining(), 11);
        let bytes = png();
        let path = directory.write("linked.png", &bytes);
        let prepared = prepare_image(&path).unwrap();
        fs::write(&path, b"changed").unwrap();
        let mut budget = ReadBudget::new(7);
        assert_eq!(
            load_image_with_budget(&prepared.asset, &mut budget)
                .unwrap_err()
                .code(),
            "image_changed"
        );
        assert_eq!(budget.remaining(), 0);
        fs::remove_file(&path).unwrap();
        // Exhausted admission is rejected before trying the now-missing file.
        assert_eq!(
            load_image_with_budget(&prepared.asset, &mut budget)
                .unwrap_err()
                .code(),
            "image_read_budget"
        );
        assert_eq!(budget.remaining(), 0);
    }

    #[test]
    fn exact_shared_budget_admits_file_and_larger_metadata_refuses_without_reads() {
        let directory = Temp::new();
        let bytes = png();
        let path = directory.write("exact.png", &bytes);
        let mut budget = ReadBudget::new(bytes.len() as u64);
        assert!(prepare_image_with_budget(&path, &mut budget).is_ok());
        assert_eq!(budget.remaining(), 0);
        let mut budget = ReadBudget::new(bytes.len() as u64 - 1);
        assert_eq!(
            prepare_image_with_budget(&path, &mut budget)
                .unwrap_err()
                .code(),
            "image_read_budget"
        );
        assert_eq!(budget.remaining(), bytes.len() as u64 - 1);
    }

    #[test]
    fn file_growth_sentinel_is_charged_and_exact_boundary_never_reads_extra_byte() {
        let mut reader = Cursor::new(b"abcdef");
        let mut budget = ReadBudget::new(10);
        assert_eq!(
            read_with_budget(&mut reader, &mut budget, 3, 2)
                .unwrap_err()
                .code(),
            "image_limit"
        );
        assert_eq!(reader.position(), 4);
        assert_eq!(budget.remaining(), 6);
        let mut reader = Cursor::new(b"abcdef");
        let mut budget = ReadBudget::new(3);
        let (captured, exact_boundary) =
            read_with_budget(&mut reader, &mut budget, MAX_IMAGE_FILE_BYTES, 2).unwrap();
        assert_eq!(&*captured, b"abc");
        assert!(exact_boundary);
        assert_eq!(reader.position(), 3);
        assert_eq!(budget.remaining(), 0);
        assert_eq!(
            check_exact_boundary(6, captured.len() as u64)
                .unwrap_err()
                .code(),
            "image_read_budget"
        );
        assert!(check_exact_boundary(3, 3).is_ok());
    }

    #[test]
    fn partial_io_failure_debits_the_bytes_already_consumed_and_keeps_cause() {
        struct Failing {
            sent: usize,
        }
        impl Read for Failing {
            fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
                if self.sent == 3 {
                    return Err(std::io::Error::other("injected partial read failure"));
                }
                let count = output.len().min(3 - self.sent);
                output[..count].copy_from_slice(&b"abc"[self.sent..self.sent + count]);
                self.sent += count;
                Ok(count)
            }
        }
        let mut reader = Failing { sent: 0 };
        let mut budget = ReadBudget::new(10);
        let error =
            read_with_budget(&mut reader, &mut budget, MAX_IMAGE_FILE_BYTES, 1).unwrap_err();
        assert_eq!(error.code(), "image_io");
        assert!(error.source().is_some());
        assert_eq!(budget.remaining(), 7);
    }

    #[test]
    fn gif_ports_charge_all_logical_frames_on_late_failure_and_verify_header_identity() {
        let directory = Temp::new();
        let mut bytes = super::gif::test_support::header(1, 1, None);
        super::gif::test_support::frame(&mut bytes, (0, 0, 1, 1), &[1], None, false);
        let last = bytes.len();
        super::gif::test_support::frame(&mut bytes, (0, 0, 1, 1), &[2], None, false);
        bytes[last + 10] = 13;
        bytes.push(0x3b);
        let path = directory.write("late-error.gif", &bytes);
        let absolute = std::path::absolute(path)
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let hash: [u8; 32] = Sha256::digest(&bytes).into();
        let asset = ImageAsset::new(absolute.clone(), hash, 1, 1).unwrap();
        let mut reads = ReadBudget::new(bytes.len() as u64);
        let mut work = DecodeBudget::new(8);
        assert_eq!(
            load_clip_with_budgets(&asset, &mut reads, &mut work, 8)
                .unwrap_err()
                .code(),
            "image_decode"
        );
        assert_eq!((reads.remaining(), work.remaining()), (0, 0));
        let forged = ImageAsset::new(absolute, hash, 2, 1).unwrap();
        let mut reads = ReadBudget::new(bytes.len() as u64);
        let mut work = DecodeBudget::new(8);
        assert_eq!(
            load_clip_with_budgets(&forged, &mut reads, &mut work, 8)
                .unwrap_err()
                .code(),
            "image_dimensions_changed"
        );
        assert_eq!((reads.remaining(), work.remaining()), (0, 8));
    }

    #[test]
    fn gif_preparation_reports_complete_clip_bytes_and_respects_callers_remaining_budget() {
        let directory = Temp::new();
        let bytes = super::gif::test_support::two_frames(None);
        let path = directory.write("bounded.gif", &bytes);
        let prepared = prepare_image(&path).unwrap();
        assert_eq!(prepared.decoded_bytes, 8);
        assert_eq!(prepared.frame.pixels(), &[255, 0, 0, 255]);
        let mut reads = ReadBudget::new(bytes.len() as u64);
        assert_eq!(
            prepare_image_with_limits(&path, &mut reads, 7)
                .unwrap_err()
                .code(),
            "image_limit"
        );
        assert_eq!(reads.remaining(), 0);
        let mut reads = ReadBudget::new(bytes.len() as u64 * 2);
        let mut work = DecodeBudget::new(8);
        let clip = load_clip_with_budgets(&prepared.asset, &mut reads, &mut work, 8).unwrap();
        assert_eq!(clip.frame_count(), 2);
        assert_eq!(work.remaining(), 0);
        assert_eq!(
            load_clip_with_budgets(&prepared.asset, &mut reads, &mut work, 8)
                .unwrap_err()
                .code(),
            "image_decode_budget"
        );
        assert_eq!(reads.remaining(), 0);
    }

    #[test]
    fn png_content_detection_preserves_alpha_hash_and_absolute_path() {
        let directory = Temp::new();
        let bytes = png();
        let path = directory.write("content.wrong", &bytes);
        let prepared = prepare_image(&path).unwrap();
        assert_eq!(prepared.frame.pixels(), &[255, 0, 0, 128, 0, 255, 0, 255]);
        assert_eq!((prepared.asset.width(), prepared.asset.height()), (2, 1));
        assert_eq!(
            prepared.asset.sha256(),
            &<[u8; 32]>::from(Sha256::digest(&bytes))
        );
        assert_eq!(
            prepared.asset.path(),
            std::path::absolute(&path).unwrap().to_str().unwrap()
        );
        assert_eq!(
            load_image(&prepared.asset).unwrap().pixels(),
            prepared.frame.pixels()
        );
    }

    #[test]
    fn static_jpeg_bmp_tga_gif_and_webp_are_real_decoded_rasters() {
        let directory = Temp::new();
        for (name, format) in [
            ("static.jpg", ImageFormat::Jpeg),
            ("static.bmp", ImageFormat::Bmp),
            ("static.tga", ImageFormat::Tga),
            ("static.gif", ImageFormat::Gif),
            ("static.webp", ImageFormat::WebP),
        ] {
            let image = DynamicImage::ImageRgb8(
                image::RgbImage::from_raw(2, 1, vec![80, 120, 160, 80, 120, 160]).unwrap(),
            );
            let mut bytes = Cursor::new(Vec::new());
            image.write_to(&mut bytes, format).unwrap();
            let prepared = prepare_image(&directory.write(name, bytes.get_ref())).unwrap();
            assert_eq!((prepared.frame.width(), prepared.frame.height()), (2, 1));
            for pixel in prepared.frame.pixels().chunks_exact(4) {
                for (actual, expected) in pixel[..3].iter().zip([80i16, 120, 160]) {
                    assert!(
                        (i16::from(*actual) - expected).abs()
                            <= if format == ImageFormat::Jpeg { 3 } else { 0 }
                    );
                }
                assert_eq!(pixel[3], 255);
            }
        }
        let mut webp = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut webp)
            .encode(&[1, 2, 3, 64], 1, 1, ExtendedColorType::Rgba8)
            .unwrap();
        assert_eq!(
            prepare_image(&directory.write("alpha.webp", &webp))
                .unwrap()
                .frame
                .pixels(),
            &[1, 2, 3, 64]
        );
    }

    #[test]
    fn changes_missing_files_and_inconsistent_recorded_dimensions_are_explicit() {
        let directory = Temp::new();
        let bytes = png();
        let path = directory.write("linked.png", &bytes);
        let prepared = prepare_image(&path).unwrap();
        let wrong = ImageAsset::new(
            prepared.asset.path().to_owned(),
            *prepared.asset.sha256(),
            1,
            1,
        )
        .unwrap();
        assert_eq!(
            load_image(&wrong).unwrap_err().code(),
            "image_dimensions_changed"
        );
        fs::write(&path, b"changed").unwrap();
        assert_eq!(
            load_image(&prepared.asset).unwrap_err().code(),
            "image_changed"
        );
        fs::remove_file(path).unwrap();
        let error = load_image(&prepared.asset).unwrap_err();
        assert_eq!(error.code(), "image_io");
        assert!(error.source().is_some());
        let relative =
            ImageAsset::new("relative.png".into(), *prepared.asset.sha256(), 2, 1).unwrap();
        assert_eq!(load_image(&relative).unwrap_err().code(), "image_path");
    }

    #[test]
    fn recorded_header_dimensions_are_checked_before_corrupt_pixel_body_decode() {
        let directory = Temp::new();
        let image = DynamicImage::ImageRgb8(
            image::RgbImage::from_raw(2, 1, vec![1, 2, 3, 4, 5, 6]).unwrap(),
        );
        let mut encoded = Cursor::new(Vec::new());
        image.write_to(&mut encoded, ImageFormat::Bmp).unwrap();
        let mut bytes = encoded.into_inner();
        let pixel_offset =
            u32::from_le_bytes([bytes[10], bytes[11], bytes[12], bytes[13]]) as usize;
        // Keep a valid 2x1 header but remove every pixel. Reaching pixel decode
        // now produces a codec I/O failure, which must lose precedence to the
        // recorded-dimension mismatch checked before raster allocation.
        bytes.truncate(pixel_offset);
        let path = directory.write("header-only.bmp", &bytes);
        assert_eq!(prepare_image(&path).unwrap_err().code(), "image_decode");
        let hash: [u8; 32] = Sha256::digest(&bytes).into();
        let asset = ImageAsset::new(path.to_str().unwrap().to_owned(), hash, 1, 1).unwrap();
        let mut budget = ReadBudget::new(bytes.len() as u64 + 10);
        assert_eq!(
            load_image_with_budget(&asset, &mut budget)
                .unwrap_err()
                .code(),
            "image_dimensions_changed"
        );
        assert_eq!(budget.remaining(), 10);
    }

    #[test]
    fn malformed_unsupported_and_oversized_inputs_are_rejected() {
        let directory = Temp::new();
        assert_eq!(
            prepare_image(&directory.write("bad.png", b"not an image"))
                .unwrap_err()
                .code(),
            "image_format"
        );
        assert_eq!(
            prepare_image(&directory.write("truncated.png", b"\x89PNG\r\n\x1a\n"))
                .unwrap_err()
                .code(),
            "image_decode"
        );
        assert_eq!(
            prepare_image(&directory.write("disguised.tga", b"II*\0unsupported tiff"))
                .unwrap_err()
                .code(),
            "image_format"
        );
        let path = directory.0.join("too-large.png");
        let file = fs::File::create(&path).unwrap();
        file.set_len(MAX_IMAGE_FILE_BYTES + 1).unwrap();
        drop(file);
        assert_eq!(prepare_image(&path).unwrap_err().code(), "image_limit");
        assert_eq!(dimensions(8192, 8192).unwrap_err().code(), "image_limit");
        let mut tga = [0u8; 18];
        tga[2] = 2;
        tga[12..14].copy_from_slice(&8193u16.to_le_bytes());
        tga[14..16].copy_from_slice(&1u16.to_le_bytes());
        tga[16] = 24;
        assert_eq!(
            prepare_image(&directory.write("huge-header.tga", &tga))
                .unwrap_err()
                .code(),
            "image_limit"
        );
    }

    fn png_chunk(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut result = (payload.len() as u32).to_be_bytes().to_vec();
        result.extend_from_slice(kind);
        result.extend_from_slice(payload);
        let mut crc = u32::MAX;
        for &byte in &result[4..] {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ if crc & 1 == 1 { 0xedb88320 } else { 0 };
            }
        }
        result.extend_from_slice(&(!crc).to_be_bytes());
        result
    }
    fn riff_chunk(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut result = kind.to_vec();
        result.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        result.extend_from_slice(payload);
        if payload.len() % 2 != 0 {
            result.push(0);
        }
        result
    }
    fn gif_single_frame(
        canvas: (u16, u16),
        origin: (u16, u16),
        frame: (u16, u16),
        valid_lzw: bool,
    ) -> Vec<u8> {
        let mut bytes = b"GIF89a".to_vec();
        bytes.extend_from_slice(&canvas.0.to_le_bytes());
        bytes.extend_from_slice(&canvas.1.to_le_bytes());
        bytes.extend_from_slice(&[0x80, 0, 0, 0, 0, 0, 255, 255, 255, 0x2c]);
        bytes.extend_from_slice(&origin.0.to_le_bytes());
        bytes.extend_from_slice(&origin.1.to_le_bytes());
        bytes.extend_from_slice(&frame.0.to_le_bytes());
        bytes.extend_from_slice(&frame.1.to_le_bytes());
        bytes.push(0);
        if valid_lzw {
            bytes.extend_from_slice(&[2, 2, 0x44, 0x01, 0]);
        } else {
            bytes.extend_from_slice(&[2, 1, 0, 0]);
        }
        bytes.push(0x3b);
        bytes
    }

    #[test]
    fn gif_subimages_cannot_bypass_logical_screen_decoded_work_admission() {
        let directory = Temp::new();
        for (canvas, origin, frame) in [
            ((1, 1), (0, 0), (4096, 3072)),
            ((16, 16), (u16::MAX, 0), (2, 1)),
            ((16, 16), (0, u16::MAX), (1, 2)),
        ] {
            let bytes = gif_single_frame(canvas, origin, frame, false);
            let error = prepare_image(&directory.write("outside.gif", &bytes)).unwrap_err();
            assert_eq!(error.code(), "image_decode");
            assert!(error.detail().contains("exceeds logical screen"));
            assert!(
                error.source().is_none(),
                "container admission must reject before corrupt LZW is decoded"
            );
        }
        let bytes = gif_single_frame((2, 2), (1, 1), (1, 1), true);
        let accepted = prepare_image(&directory.write("within.gif", &bytes)).unwrap();
        assert_eq!(
            accepted.frame.pixels(),
            &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255]
        );
    }

    fn extended_webp(canvas: (u32, u32), kind: &[u8; 4], coded: &[u8]) -> Vec<u8> {
        let mut extended = [0; 10];
        extended[4..7].copy_from_slice(&(canvas.0 - 1).to_le_bytes()[..3]);
        extended[7..10].copy_from_slice(&(canvas.1 - 1).to_le_bytes()[..3]);
        let mut body = b"WEBP".to_vec();
        body.extend(riff_chunk(b"VP8X", &extended));
        body.extend(riff_chunk(kind, coded));
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
        bytes.extend(body);
        bytes
    }

    #[test]
    fn webp_coded_frame_dimensions_precede_truncated_pixel_decode() {
        let directory = Temp::new();
        let mut vp8 = vec![0x10, 0, 0, 0x9d, 0x01, 0x2a];
        vp8.extend_from_slice(&4096u16.to_le_bytes());
        vp8.extend_from_slice(&3072u16.to_le_bytes());
        let mut vp8l = vec![0x2f];
        vp8l.extend_from_slice(&(4095u32 | (3071u32 << 14)).to_le_bytes());
        for (kind, payload) in [(b"VP8 ", vp8.as_slice()), (b"VP8L", vp8l.as_slice())] {
            let bytes = extended_webp((1, 1), kind, payload);
            let error = prepare_image(&directory.write("small-canvas.webp", &bytes)).unwrap_err();
            assert_eq!(error.code(), "image_decode");
            assert!(
                error
                    .detail()
                    .contains("coded frame dimensions 4096x3072 disagree")
            );
            assert!(
                error.source().is_none(),
                "coded header mismatch must reject before the raster codec runs"
            );
        }
        vp8[6..8].copy_from_slice(&8193u16.to_le_bytes());
        vp8[8..10].copy_from_slice(&1u16.to_le_bytes());
        let bytes = extended_webp((1, 1), b"VP8 ", &vp8);
        assert_eq!(
            prepare_image(&directory.write("oversized-coded.webp", &bytes))
                .unwrap_err()
                .code(),
            "image_limit"
        );
        vp8[3] = 0;
        let bytes = extended_webp((1, 1), b"VP8 ", &vp8);
        assert_eq!(
            prepare_image(&directory.write("bad-keyframe.webp", &bytes))
                .unwrap_err()
                .code(),
            "image_decode"
        );
    }
    #[test]
    fn png_header_pixel_and_native_decode_limits_precede_raster_allocation() {
        let directory = Temp::new();
        for (width, height, bit_depth) in [(8192u32, 8192u32, 8u8), (8192, 2048, 16)] {
            let original = png();
            let mut ihdr = original[16..29].to_vec();
            ihdr[..4].copy_from_slice(&width.to_be_bytes());
            ihdr[4..8].copy_from_slice(&height.to_be_bytes());
            ihdr[8] = bit_depth;
            let mut bytes = original[..8].to_vec();
            bytes.extend(png_chunk(b"IHDR", &ihdr));
            bytes.extend_from_slice(&original[33..]);
            assert_eq!(
                prepare_image(&directory.write("oversized-header.png", &bytes))
                    .unwrap_err()
                    .code(),
                "image_limit"
            );
        }
    }
    #[test]
    fn animated_png_webp_remain_refused_while_gif_decodes_all_frames() {
        let directory = Temp::new();
        let mut apng = png();
        let mut control = 1u32.to_be_bytes().to_vec();
        control.extend_from_slice(&0u32.to_be_bytes());
        let mut frame_control = 0u32.to_be_bytes().to_vec(); // sequence number
        frame_control.extend_from_slice(&2u32.to_be_bytes());
        frame_control.extend_from_slice(&1u32.to_be_bytes());
        frame_control.extend_from_slice(&[0; 8]); // x/y offsets
        frame_control.extend_from_slice(&1u16.to_be_bytes());
        frame_control.extend_from_slice(&10u16.to_be_bytes());
        frame_control.extend_from_slice(&[0, 0]); // disposal/blend
        let mut chunks = png_chunk(b"acTL", &control);
        chunks.extend(png_chunk(b"fcTL", &frame_control));
        apng.splice(33..33, chunks);
        assert!(
            image::codecs::png::PngDecoder::with_limits(Cursor::new(&apng), limits())
                .unwrap()
                .is_apng()
                .unwrap()
        );
        assert_eq!(
            prepare_image(&directory.write("animation.png", &apng))
                .unwrap_err()
                .code(),
            "image_animation"
        );
        let mut gif = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut gif);
            for color in [[255, 0, 0, 255], [0, 255, 0, 255]] {
                encoder
                    .encode_frame(image::Frame::new(
                        image::RgbaImage::from_raw(1, 1, color.to_vec()).unwrap(),
                    ))
                    .unwrap();
            }
        }
        let prepared = prepare_image(&directory.write("animation.gif", &gif)).unwrap();
        assert_eq!(prepared.decoded_bytes, 8);
        assert_eq!(prepared.frame.pixels(), &[255, 0, 0, 255]);
        let clip =
            load_clip_with_budget(&prepared.asset, &mut ReadBudget::new(gif.len() as u64), 8)
                .unwrap();
        assert_eq!(clip.frame_count(), 2);
        assert_eq!(
            clip.sample(MediaTime(100_000_001)).image.pixels(),
            &[0, 255, 0, 255]
        );
        let mut still_webp = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut still_webp)
            .encode(&[255, 0, 0], 1, 1, ExtendedColorType::Rgb8)
            .unwrap();
        assert_eq!(&still_webp[12..16], b"VP8L");
        let mut frame = vec![0; 16];
        frame[12] = 100;
        frame[15] = 2;
        frame.extend_from_slice(&still_webp[12..]);
        let mut body = b"WEBP".to_vec();
        body.extend(riff_chunk(b"VP8X", &[2, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
        body.extend(riff_chunk(b"ANIM", &[0; 6]));
        body.extend(riff_chunk(b"ANMF", &frame));
        body.extend(riff_chunk(b"ANMF", &frame));
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&(body.len() as u32).to_le_bytes());
        webp.extend(body);
        assert!(
            image::codecs::webp::WebPDecoder::new(Cursor::new(&webp))
                .unwrap()
                .has_animation()
        );
        assert_eq!(
            prepare_image(&directory.write("animation.webp", &webp))
                .unwrap_err()
                .code(),
            "image_animation"
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_path_is_refused_without_lossy_conversion() {
        use std::os::unix::ffi::OsStringExt;
        let path = std::ffi::OsString::from_vec(vec![b'/', b't', 0xff]);
        assert_eq!(
            prepare_image(Path::new(&path)).unwrap_err().code(),
            "image_path"
        );
    }
}
