//! Independent syntax/energy oracle for ISO/IEC14496-3 §4.6.13 correlated PNS.
//! Adapted synthetic bit writer from FilmCraft decoder_oracle.rs (same MIT/Apache
//! source revision recorded in NOTICE); no external decoder/executable is called.
use skylive_aac::Decoder;
use skylive_codec_bits::BitWriter;

fn noise_channel(bits: &mut BitWriter, noise_energy: u32) {
    bits.write_bits(100, 8); //global gain
    bits.write_bits(13, 4); //NOISE_HCB, one section covering40 bands
    bits.write_bits(31, 5);
    bits.write_bits(9, 5);
    bits.write_bits(noise_energy, 9);
    for _ in 1..40 {
        bits.write_bits(0, 1);
    } //zero scalefactor deltas
    bits.write_bits(0, 3); //no pulse,TNS,gain-control data
}
#[test]
fn correlated_noise_preserves_normative_energy_ratio_and_channel_correlation() {
    let mut decoder = Decoder::new(&[0x12, 0x10]).unwrap(); //AAC-LC44.1k stereo
    assert!(!decoder.uses_perceptual_noise());
    let (mut left_energy, mut right_energy, mut product) = (0.0f64, 0.0f64, 0.0f64);
    for _ in 0..12 {
        let mut bits = BitWriter::new();
        bits.write_bits(1, 3);
        bits.write_bits(0, 4);
        bits.write_bits(1, 1); //CPE/tag/common_window
        bits.write_bits(0, 4);
        bits.write_bits(40, 6);
        bits.write_bits(0, 1); //long sineICS
        bits.write_bits(2, 2); //all bands have ms_used=1 ->shared PNS vector
        noise_channel(&mut bits, 276);
        noise_channel(&mut bits, 268);
        bits.write_bits(7, 3); //ID_END
        let pcm = decoder.decode(&bits.finish()).unwrap();
        assert_eq!(pcm.len(), 2);
        assert_eq!(pcm[0].len(), 1024);
        for (&left, &right) in pcm[0].iter().zip(&pcm[1]) {
            assert!(left.is_finite() && right.is_finite());
            left_energy += f64::from(left).powi(2);
            right_energy += f64::from(right).powi(2);
            product += f64::from(left) * f64::from(right);
        }
    }
    assert!(decoder.uses_perceptual_noise());
    assert!(left_energy > 0.0);
    // A difference of8 noise scalefactor units gives amplitude2^(-8/4)=1/4,
    // hence energy1/16. Correlated PNS must not invent independent stereo noise.
    assert!((right_energy / left_energy - 0.0625).abs() < 0.000001);
    assert!(product / (left_energy * right_energy).sqrt() > 0.999999);
}
