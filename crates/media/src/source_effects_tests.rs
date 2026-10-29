use super::*;
use crate::{
    Affine2, AffineLayer, CompositeLimits, UvRect, composite_affine, composite_affine_premultiplied,
};
fn layer(image: &RgbaFrame) -> AffineLayer<'_> {
    AffineLayer::new(image, Affine2::IDENTITY, UvRect::FULL, 1.0)
        .with_blend_mode(SourceBlend::PremultipliedLinearSrgb)
}
fn limits() -> CraftVideoLimits {
    CraftVideoLimits {
        max_output_bytes: 4,
        max_pixel_stages: 8,
    }
}
fn filtered(pixel: [u8; 4], settings: ColorCorrectionSettings) -> SourceEffectFrame {
    source_video_effects(
        &RgbaFrame::new(1, 1, pixel.to_vec()).unwrap(),
        SourceBlend::LinearSrgb,
        &[SourceVideoEffect::ColorCorrection(settings)],
        limits(),
    )
    .unwrap()
}
#[test]
fn literal_lp_energy_survives_two_nested_targets_and_subbyte_alpha() {
    for (settings, expected, alpha) in [
        (
            ColorCorrectionSettings {
                brightness: 1.0,
                ..Default::default()
            },
            225,
            128,
        ),
        (
            ColorCorrectionSettings {
                opacity: 0.001,
                ..Default::default()
            },
            3,
            0,
        ),
    ] {
        let source = filtered(
            if alpha == 128 {
                [255, 255, 255, 128]
            } else {
                [255; 4]
            },
            settings,
        );
        assert_eq!(source.blend, SourceBlend::PremultipliedLinearSrgb);
        assert_eq!(
            source.image.pixels(),
            &[expected, expected, expected, alpha]
        );
        let group = composite_affine_premultiplied(
            1,
            1,
            &[layer(&source.image)],
            CompositeLimits::default(),
        )
        .unwrap();
        let outer =
            composite_affine_premultiplied(1, 1, &[layer(&group)], CompositeLimits::default())
                .unwrap();
        assert_eq!(outer.pixels(), source.image.pixels());
        let output = composite_affine(
            1,
            1,
            [0, 0, 0, 255],
            &[layer(&outer)],
            CompositeLimits::default(),
        )
        .unwrap();
        assert_eq!(output.pixels(), &[expected, expected, expected, 255]);
    }
}
#[test]
fn mixed_craft_stages_preserve_lp_energy_and_obs_targets_are_not_fused() {
    let input = RgbaFrame::new(1, 1, vec![255, 255, 255, 128]).unwrap();
    let chain = [
        SourceVideoEffect::ColorCorrection(ColorCorrectionSettings {
            brightness: 1.0,
            ..Default::default()
        }),
        SourceVideoEffect::Craft(CraftVideoEffect::SpillSuppression {
            blue: false,
            balance: 50.0,
            amount: 0.0,
        }),
    ];
    assert_eq!(
        source_video_effects(&input, SourceBlend::LinearSrgb, &chain, limits())
            .unwrap()
            .image
            .pixels(),
        &[225, 225, 225, 128]
    );
    let chain = [
        SourceVideoEffect::ColorCorrection(ColorCorrectionSettings {
            brightness: -1.0,
            ..Default::default()
        }),
        SourceVideoEffect::ColorCorrection(ColorCorrectionSettings {
            brightness: 1.0,
            ..Default::default()
        }),
    ];
    let gray = RgbaFrame::new(1, 1, vec![128, 128, 128, 255]).unwrap();
    assert_eq!(
        source_video_effects(&gray, SourceBlend::LinearSrgb, &chain, limits())
            .unwrap()
            .image
            .pixels(),
        &[255; 4]
    );
    assert!(matches!(
        source_video_effects(
            &gray,
            SourceBlend::LinearSrgb,
            &chain,
            CraftVideoLimits {
                max_output_bytes: 4,
                max_pixel_stages: 1
            }
        ),
        Err(MediaError::BudgetExceeded)
    ));
}
#[test]
fn craft_only_delegates_exactly_and_input_encoding_is_meaningful() {
    let input = RgbaFrame::new(1, 1, vec![255, 255, 255, 128]).unwrap();
    let craft = CraftVideoEffect::BrightnessContrast {
        brightness: 12.5,
        contrast: -31.0,
        legacy: false,
    };
    let old = crate::craft_video_effects(&input, &[craft], limits()).unwrap();
    let new = source_video_effects(
        &input,
        SourceBlend::EncodedPremultipliedSrgb,
        &[SourceVideoEffect::Craft(craft)],
        limits(),
    )
    .unwrap();
    assert_eq!(old.pixels(), new.image.pixels());
    assert_eq!(new.blend, SourceBlend::EncodedPremultipliedSrgb);
    let effect = [SourceVideoEffect::ColorCorrection(Default::default())];
    assert_eq!(
        source_video_effects(&input, SourceBlend::LinearSrgb, &effect, limits())
            .unwrap()
            .image
            .pixels(),
        &[188, 188, 188, 128]
    );
    assert_eq!(
        source_video_effects(
            &input,
            SourceBlend::EncodedPremultipliedSrgb,
            &effect,
            limits()
        )
        .unwrap()
        .image
        .pixels(),
        &[128; 4]
    );
}
