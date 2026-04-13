//! Canonical default preset images extracted from the official
//! `DSP_default_parameters.xml` corpus for non-Dirac families.
//!
//! These images match the Device Console preset-save serializer for the
//! SHARC families whose reset flow is fully represented by the XML defaults
//! alone. Dirac-capable families append extra default channel-config bytes in
//! the official app and are therefore intentionally excluded until that
//! payload is modeled exactly.

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
    fn non_dirac_default_images_are_available() {
        assert!(default_preset_image_for_dsp(100).is_some());
        assert!(default_preset_image_for_dsp(110).is_some());
        assert!(default_preset_image_for_dsp(115).is_some());
    }

    #[test]
    fn dirac_families_are_not_exposed_prematurely() {
        assert!(default_preset_image_for_dsp(101).is_none());
        assert!(default_preset_image_for_dsp(105).is_none());
        assert!(default_preset_image_for_dsp(111).is_none());
        assert!(default_preset_image_for_dsp(113).is_none());
    }

    #[test]
    fn generated_image_lengths_match_supported_xml_families() {
        let dsp100 = default_preset_image_for_dsp(100).unwrap();
        assert_eq!(dsp100.params.len(), 34960);
        assert_eq!(dsp100.bypass.len(), 72 * 3);

        let dsp110 = default_preset_image_for_dsp(110).unwrap();
        assert_eq!(dsp110.params.len(), 20392);
        assert_eq!(dsp110.bypass.len(), 118 * 3);

        let dsp115 = default_preset_image_for_dsp(115).unwrap();
        assert_eq!(dsp115.params.len(), 8224);
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

        let dsp115 = default_preset_image_for_dsp(115).unwrap();
        assert_eq!(word_at(dsp115, 1656), 1.0f32.to_bits());
    }
}
