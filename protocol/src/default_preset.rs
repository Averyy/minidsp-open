//! Canonical default preset images extracted from the official
//! `DSP_default_parameters.xml` corpus for every modern SHARC family shipped
//! under `protocol/assets/audio_firmware`.
//!
//! For Dirac-capable families, these images include the serialized `<dirac>`
//! channel-default block in the same preset-values layout used by the official
//! Device Console preset writer.

include!(concat!(env!("OUT_DIR"), "/default_preset_generated.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    fn word_at(image: &DefaultPresetImage, addr: u16) -> u32 {
        let offset = addr as usize * 4;
        let bytes: [u8; 4] = image.params[offset..offset + 4].try_into().unwrap();
        u32::from_le_bytes(bytes)
    }

    #[test]
    fn supported_default_images_are_available() {
        assert!(default_preset_image_for_dsp(100).is_some());
        assert!(default_preset_image_for_dsp(101).is_some());
        assert!(default_preset_image_for_dsp(105).is_some());
        assert!(default_preset_image_for_dsp(110).is_some());
        assert!(default_preset_image_for_dsp(111).is_some());
        assert!(default_preset_image_for_dsp(113).is_some());
        assert!(default_preset_image_for_dsp(115).is_some());
    }

    #[test]
    fn unsupported_families_remain_absent() {
        assert!(default_preset_image_for_dsp(99).is_none());
        assert!(default_preset_image_for_dsp(112).is_none());
        assert!(default_preset_image_for_dsp(200).is_none());
    }

    #[test]
    fn generated_image_lengths_match_supported_xml_families() {
        let dsp100 = default_preset_image_for_dsp(100).unwrap();
        assert_eq!(dsp100.params.len(), 34960);
        assert_eq!(dsp100.bypass.len(), 72 * 3);

        let dsp101 = default_preset_image_for_dsp(101).unwrap();
        assert_eq!(dsp101.params.len(), 12040);
        assert_eq!(dsp101.bypass.len(), 48 * 3);

        let dsp105 = default_preset_image_for_dsp(105).unwrap();
        assert_eq!(dsp105.params.len(), 20744);
        assert_eq!(dsp105.bypass.len(), 48 * 3);

        let dsp110 = default_preset_image_for_dsp(110).unwrap();
        assert_eq!(dsp110.params.len(), 20392);
        assert_eq!(dsp110.bypass.len(), 118 * 3);

        let dsp111 = default_preset_image_for_dsp(111).unwrap();
        assert_eq!(dsp111.params.len(), 13816);
        assert_eq!(dsp111.bypass.len(), 96 * 3);

        let dsp113 = default_preset_image_for_dsp(113).unwrap();
        assert_eq!(dsp113.params.len(), 33280);
        assert_eq!(dsp113.bypass.len(), 114 * 3);

        let dsp115 = default_preset_image_for_dsp(115).unwrap();
        assert_eq!(dsp115.params.len(), 34880);
        assert_eq!(dsp115.bypass.len(), 194 * 3);
    }

    #[test]
    fn generated_images_preserve_known_xml_default_words() {
        let dsp100 = default_preset_image_for_dsp(100).unwrap();
        assert_eq!(word_at(dsp100, 0), 0x0000_0002);
        assert_eq!(word_at(dsp100, 8280), 1.0f32.to_bits());

        let dsp110 = default_preset_image_for_dsp(110).unwrap();
        assert_eq!(word_at(dsp110, 66), 0x0000_0002);
        assert_eq!(word_at(dsp110, 2120), 1.0f32.to_bits());

        let dsp101 = default_preset_image_for_dsp(101).unwrap();
        assert_eq!(word_at(dsp101, 448), 0x0000_0002);
        assert_eq!(word_at(dsp101, 457), 0x0000_0001);
        assert_eq!(word_at(dsp101, 458), 1088);
        assert_eq!(word_at(dsp101, 1578), 0x3D04_2108);
        assert_eq!(word_at(dsp101, 1579), 0x3D04_2108);
        assert_eq!(word_at(dsp101, 1729), 0x0000_0002);

        let dsp105 = default_preset_image_for_dsp(105).unwrap();
        assert_eq!(word_at(dsp105, 448), 0x0000_0002);
        assert_eq!(word_at(dsp105, 458), 2176);
        assert_eq!(word_at(dsp105, 2666), 0x3D04_2108);
        assert_eq!(word_at(dsp105, 2667), 0x3D04_2108);

        let dsp111 = default_preset_image_for_dsp(111).unwrap();
        assert_eq!(word_at(dsp111, 892), 0x0000_0002);
        assert_eq!(word_at(dsp111, 901), 0x0000_0001);
        assert_eq!(word_at(dsp111, 2022), 0x3D04_2108);
        assert_eq!(word_at(dsp111, 2023), 0x3D04_2108);

        let dsp113 = default_preset_image_for_dsp(113).unwrap();
        assert_eq!(word_at(dsp113, 1656), 0x0000_0002);
        assert_eq!(word_at(dsp113, 1666), 640);
        assert_eq!(word_at(dsp113, 2338), 0x3D04_2108);
        assert_eq!(word_at(dsp113, 2339), 0x3D04_2108);
        assert_eq!(word_at(dsp113, 2489), 0x0000_0002);

        let dsp115 = default_preset_image_for_dsp(115).unwrap();
        assert_eq!(word_at(dsp115, 1656), 1.0f32.to_bits());
        assert_eq!(word_at(dsp115, 2056), 0x0000_0002);
        assert_eq!(word_at(dsp115, 2066), 640);
        assert_eq!(word_at(dsp115, 2738), 0x3D04_2108);
        assert_eq!(word_at(dsp115, 2739), 0x3D04_2108);
    }
}
