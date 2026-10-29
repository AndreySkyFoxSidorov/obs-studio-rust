use super::*;
#[test]
fn exact_builtin_defaults_ranges_and_provider_are_distinct_from_craft() {
    assert!(matches!(
        AudioFilter::obs_limiter(),
        AudioFilter::ObsLimiter {
            threshold_db: -6.,
            release_ms: 60,
            enabled: true
        }
    ));
    assert!(matches!(
        AudioFilter::obs_compressor(),
        AudioFilter::ObsCompressor {
            threshold_db: -18.,
            ratio: 10.,
            attack_ms: 6,
            release_ms: 60,
            output_db: 0.,
            sidechain: None,
            enabled: true
        }
    ));
    assert!(matches!(
        AudioFilter::obs_expander(),
        AudioFilter::ObsExpander {
            threshold_db: -40.,
            ratio: 2.,
            attack_ms: 10,
            release_ms: 50,
            output_db: 0.,
            detector: ObsExpanderDetector::Rms,
            preset: ObsExpanderPreset::Expander,
            enabled: true
        }
    ));
    assert!(matches!(
        AudioFilter::obs_expander_preset(ObsExpanderPreset::Gate),
        AudioFilter::ObsExpander {
            ratio: 10.,
            release_ms: 125,
            ..
        }
    ));
    for filter in [
        AudioFilter::obs_limiter(),
        AudioFilter::obs_compressor(),
        AudioFilter::obs_expander(),
    ] {
        assert_eq!(filter.plugin_id(), crate::OBS_VIDEO_FILTERS_PLUGIN);
        assert!(filter.validate().is_ok());
    }
    for threshold_db in [f32::NAN, f32::INFINITY, -60.01, 0.01] {
        assert!(
            AudioFilter::ObsLimiter {
                threshold_db,
                release_ms: 60,
                enabled: true
            }
            .validate()
            .is_err()
        );
    }
    for ratio in [0.99, 32.01, f32::NAN] {
        assert!(
            AudioFilter::ObsCompressor {
                threshold_db: -18.,
                ratio,
                attack_ms: 6,
                release_ms: 60,
                output_db: 0.,
                sidechain: None,
            enabled: true
            }
            .validate()
            .is_err()
        );
    }
    for attack_ms in [0, 101, u32::MAX] {
        assert!(
            AudioFilter::ObsExpander {
                threshold_db: -40.,
                ratio: 2.,
                attack_ms,
                release_ms: 50,
                output_db: 0.,
                detector: ObsExpanderDetector::Peak,
                preset: ObsExpanderPreset::Expander,
                enabled: false
            }
            .validate()
            .is_err()
        );
    }
}
