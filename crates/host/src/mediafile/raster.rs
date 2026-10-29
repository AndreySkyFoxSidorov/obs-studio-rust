use super::*;
use skylive_media::{
    CameraMatrix, CameraRange, PackedFrameLimits, Planar420Layout, RgbaFrame, ingest_planar_420,
};
impl MediaFileReader {
    /// Pure conversion of this file's decoded planar picture; no additional I/O.
    /// Container colr fills unspecified VUI fields; conflicting explicit fields
    /// and unsupported display processing are rejected rather than guessed.
    pub fn rgba(&self, picture: &skylive_h264::Picture) -> Result<RgbaFrame, MediaFileError> {
        let track = self
            .video_track
            .ok_or(MediaFileError::Unsupported("file has no video track"))?;
        let params = self.file.tracks[track]
            .video()
            .ok_or(MediaFileError::Unsupported("video parameters missing"))?;
        if (picture.width, picture.height) != (u32::from(params.width), u32::from(params.height)) {
            return Err(MediaFileError::Unsupported(
                "dynamic video resolution or conflicting sample-entry dimensions",
            ));
        }
        if picture.sar != (0, 0) && (picture.sar.0 == 0 || picture.sar.0 != picture.sar.1) {
            return Err(MediaFileError::Unsupported(
                "non-square video sample aspect ratio",
            ));
        }
        let (mut primaries, mut transfer, mut matrix, mut full) = (
            u16::from(picture.color.primaries),
            u16::from(picture.color.transfer),
            u16::from(picture.color.matrix),
            picture.color.full_range,
        );
        if let Some(color) = &params.color {
            let (p, t, m, f) = match color {
                skylive_mp4::ColorInfo::Nclx {
                    primaries,
                    transfer,
                    matrix,
                    full_range,
                } => (*primaries, *transfer, *matrix, *full_range),
                skylive_mp4::ColorInfo::Nclc {
                    primaries,
                    transfer,
                    matrix,
                } => (*primaries, *transfer, *matrix, full),
                _ => {
                    return Err(MediaFileError::Unsupported(
                        "embedded media ICC profiles require color management",
                    ));
                }
            };
            for (vui, container) in [(primaries, p), (transfer, t), (matrix, m)] {
                if vui != 2 && container != 2 && vui != container {
                    return Err(MediaFileError::Unsupported(
                        "conflicting container and bitstream color metadata",
                    ));
                }
            }
            if primaries == 2 {
                primaries = p;
            }
            if transfer == 2 {
                transfer = t;
            }
            if matrix == 2 {
                matrix = m;
            }
            full = f;
        }
        if !matches!(primaries, 1 | 2 | 5 | 6) || !matches!(transfer, 1 | 2 | 6 | 13) {
            return Err(MediaFileError::Unsupported(
                "media transfer/primaries are outside the supported SDR profile",
            ));
        }
        let matrix = match matrix {
            1 | 2 => CameraMatrix::Bt709,
            5 | 6 => CameraMatrix::Bt601,
            _ => {
                return Err(MediaFileError::Unsupported(
                    "media YUV matrix is outside BT.601/BT.709",
                ));
            }
        };
        let chroma_left = match picture.chroma_location.unwrap_or((0, 0)) {
            (0, 0) => true,
            (1, 1) => false,
            _ => {
                return Err(MediaFileError::Unsupported(
                    "video chroma location is outside supported progressive left/center sampling",
                ));
            }
        };
        ingest_planar_420(
            &picture.y,
            &picture.u,
            &picture.v,
            Planar420Layout {
                width: picture.width,
                height: picture.height,
                y_stride: picture.y_stride,
                uv_stride: picture.uv_stride,
                matrix,
                range: if full {
                    CameraRange::Full
                } else {
                    CameraRange::Limited
                },
                chroma_left,
            },
            PackedFrameLimits {
                max_input_bytes: 64 * 1024 * 1024,
                max_output_bytes: 16 * 1024 * 1024,
                max_pixel_visits: 6 * 4_194_304,
            },
        )
        .map_err(|e| MediaFileError::Decode(format!("video raster conversion: {e}")))
    }
}
