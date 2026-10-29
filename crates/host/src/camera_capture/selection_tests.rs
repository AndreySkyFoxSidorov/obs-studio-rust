use super::*;
fn mode(width: u32, height: u32, num: u32, den: u32, format: VideoPixelFormat) -> CameraModeInfo {
    let mode = VideoCaptureMode::new(width, height, num, den, format).unwrap();
    CameraModeInfo {
        width,
        height,
        fps_num: num,
        fps_den: den,
        native_subtype: format.label().into(),
        mode: Some(mode),
        unsupported_reason: None,
    }
}
fn nv12() -> VideoPixelFormat {
    VideoPixelFormat::Nv12 {
        matrix: VideoColorMatrix::Bt601,
        range: VideoColorRange::Full,
        chroma_left: false,
    }
}
fn unsupported() -> CameraModeInfo {
    CameraModeInfo {
        width: 1920,
        height: 1080,
        fps_num: 30,
        fps_den: 1,
        native_subtype: "MJPG".into(),
        mode: None,
        unsupported_reason: Some("decoder not implemented"),
    }
}
#[test]
fn driver_lowest_first_no_longer_wins_and_fullhd_nv12_is_recommended_independently_of_order() {
    let expected = mode(1920, 1080, 30, 1, nv12()).mode;
    let mut modes = vec![
        mode(160, 120, 5, 1, VideoPixelFormat::Rgb32),
        mode(640, 480, 30, 1, VideoPixelFormat::Rgb32),
        mode(
            1920,
            1080,
            5,
            1,
            VideoPixelFormat::Yuy2 {
                matrix: VideoColorMatrix::Bt601,
                range: VideoColorRange::Limited,
                chroma_left: true,
            },
        ),
        mode(1920, 1080, 30, 1, nv12()),
        unsupported(),
    ];
    for _ in 0..modes.len() {
        let recommendation = CameraModeRecommendations::from_modes(&modes).unwrap();
        assert_eq!(recommendation.recommended, expected);
        assert_eq!(recommendation.full_hd, expected);
        modes.rotate_left(1);
    }
    arrange_camera_modes(&mut modes).unwrap();
    assert_eq!(modes[0].mode, expected);
    assert!(
        modes.iter().position(|info| info.mode.is_none()).unwrap()
            < modes.iter().position(|info| info.width == 640).unwrap(),
        "unsupported HD must remain above lower resolution modes"
    );
}

#[test]
fn visible_modes_are_strictly_resolution_then_rational_fps_descending_even_when_unsupported() {
    let mut compressed = unsupported();
    compressed.fps_num = 60;
    let mut modes = vec![
        mode(640, 480, 120, 1, VideoPixelFormat::Rgb32),
        mode(1920, 1080, 30, 1, nv12()),
        compressed.clone(),
        mode(3840, 2160, 5, 1, VideoPixelFormat::Rgb32),
        mode(1920, 1080, 30000, 1001, nv12()),
    ];
    let chosen = CameraModeRecommendations::from_modes(&modes)
        .unwrap()
        .recommended
        .unwrap();
    for _ in 0..modes.len() {
        modes.rotate_left(1);
        arrange_camera_modes(&mut modes).unwrap();
        let observed: Vec<_> = modes
            .iter()
            .map(|mode| (mode.width, mode.height, mode.fps_num, mode.fps_den))
            .collect();
        assert_eq!(
            observed,
            [
                (3840, 2160, 5, 1),
                (1920, 1080, 60, 1),
                (1920, 1080, 30, 1),
                (1920, 1080, 30000, 1001),
                (640, 480, 120, 1)
            ]
        );
        assert_eq!(modes[1], compressed);
        assert!(
            modes.iter().any(|mode| mode.mode == Some(chosen)),
            "sorting never remaps the selected actual-mode identity"
        );
    }
}
#[test]
fn exact_rational_rates_unsupported_formats_and_slower_fullhd_remain_honest() {
    let hd = mode(1920, 1080, 30000, 1001, nv12());
    let mut modes = vec![
        unsupported(),
        mode(1920, 1080, 60, 1, nv12()),
        hd.clone(),
        mode(1920, 1080, 5, 1, nv12()),
    ];
    assert_eq!(
        CameraModeRecommendations::from_modes(&modes)
            .unwrap()
            .recommended,
        hd.mode
    );
    modes = vec![
        mode(1920, 1080, 5, 1, nv12()),
        mode(1280, 720, 30, 1, VideoPixelFormat::Rgb32),
        unsupported(),
    ];
    let suggestion = CameraModeRecommendations::from_modes(&modes).unwrap();
    assert_eq!(
        (
            suggestion.recommended.unwrap().width(),
            suggestion.recommended.unwrap().height()
        ),
        (1280, 720)
    );
    assert_eq!(
        suggestion.full_hd.unwrap().fps_num(),
        5,
        "Full HD shortcut must not invent30fps"
    );
    assert!(
        CameraModeRecommendations::from_modes(&[unsupported()])
            .unwrap()
            .recommended
            .is_none()
    );
}
#[test]
fn recommendation_admission_and_color_metadata_are_bounded_and_deterministic() {
    assert!(CameraModeRecommendations::from_modes(&vec![unsupported(); 1025]).is_err());
    let mut inconsistent = mode(1920, 1080, 30, 1, nv12());
    inconsistent.width = 640;
    assert!(CameraModeRecommendations::from_modes(&[inconsistent]).is_err());
    let a = mode(1920, 1080, 30, 1, nv12());
    let b = mode(
        1920,
        1080,
        30,
        1,
        VideoPixelFormat::Nv12 {
            matrix: VideoColorMatrix::Bt709,
            range: VideoColorRange::Limited,
            chroma_left: true,
        },
    );
    let x = CameraModeRecommendations::from_modes(&[a.clone(), b.clone()]).unwrap();
    let y = CameraModeRecommendations::from_modes(&[b, a]).unwrap();
    assert_eq!(x, y);
}
