//! Exact alpha-zero shortcut checks against the independent frozen keyers.
use super::*;

#[test]
fn transparent_and_sub_byte_opacity_match_the_frozen_keyers() {
    for encoding in ENCODINGS {
        for opacity in [
            0.0,
            f32::from_bits(1),
            f32::MIN_POSITIVE,
            1e-7,
            1e-6,
            0.37,
            1.0,
        ] {
            for gamma in [-1.0, 0.0, 1.0] {
                let chroma = ChromaKeySettings {
                    opacity,
                    gamma,
                    similarity: 1,
                    smoothness: 1,
                    spill: 1,
                    contrast: 4.0,
                    brightness: 1.0,
                    ..Default::default()
                };
                let color = ColorKeySettings {
                    opacity,
                    gamma,
                    similarity: 1,
                    smoothness: 1,
                    contrast: 4.0,
                    brightness: 1.0,
                    ..Default::default()
                };
                let input = pixels(7, 5, 0x813f_2bc7);
                let mut expected = input.clone();
                let mut actual = input.clone();
                baseline_keyer::chroma_in_place(
                    &mut expected,
                    7,
                    5,
                    encoding,
                    baseline_keyer::PreparedKeyer::chroma(chroma),
                    &mut vec![[0.0; 4]; 7 * 3],
                );
                chroma_in_place(
                    &mut actual,
                    7,
                    5,
                    encoding,
                    PreparedKeyer::chroma(chroma),
                    &mut vec![0u32; 7 * 12],
                )
                .unwrap();
                assert_eq!(actual, expected, "tiny chroma {encoding:?} {chroma:?}");
                for pixel in input.chunks_exact(4) {
                    let pixel = [pixel[0], pixel[1], pixel[2], pixel[3]];
                    assert_eq!(
                        PreparedKeyer::color(color).pixel(pixel, encoding),
                        baseline_keyer::PreparedKeyer::color(color).pixel(pixel, encoding),
                        "tiny color {pixel:?} {encoding:?} {color:?}",
                    );
                }
            }
        }
    }
    // A quantized-alpha-zero shortcut would wrongly erase this premultiplied
    // red contribution. Only exact floating alpha zero may skip finish().
    let settings = ColorKeySettings {
        gamma: -1.0,
        opacity: 1e-6,
        similarity: 1,
        smoothness: 1,
        ..Default::default()
    };
    let encoding = ColorCorrectionInput::PremultipliedLinearSrgb;
    let expected = baseline_keyer::PreparedKeyer::color(settings).pixel([255, 0, 0, 1], encoding);
    let actual = PreparedKeyer::color(settings).pixel([255, 0, 0, 1], encoding);
    assert_eq!(actual, expected);
    assert_eq!(actual[3], 0);
    assert!(actual[0] > 0);
}
