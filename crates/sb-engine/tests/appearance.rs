//! The appearance library's contract, exercised through `sb-engine`'s
//! public API only (integration test: nothing private is reachable).
//! The WCAG luminance, contrast and OKLCH conversions below are
//! reimplemented from the published formulas — never the production
//! code's — and evaluated on the quantized `SrgbColor`s the tokens are
//! handed out as, so the proofs hold for the exact bytes the shells
//! draw (spec/08, research 15 §"Contrast").

use sb_engine::{AppearanceTokens, PaletteColor, SrgbColor, SrgbColorError};

/// WCAG relative luminance of an sRGB color (WCAG 2.2 definition).
fn luminance(color: SrgbColor) -> f64 {
    fn linear(channel: u8) -> f64 {
        let srgb = f64::from(channel) / 255.0;
        if srgb <= 0.04045 {
            srgb / 12.92
        } else {
            ((srgb + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * linear(color.red()) + 0.7152 * linear(color.green()) + 0.0722 * linear(color.blue())
}

/// The WCAG contrast ratio of two colors, in [1, 21].
fn contrast(a: SrgbColor, b: SrgbColor) -> f64 {
    let (high, low) = (
        luminance(a).max(luminance(b)),
        luminance(a).min(luminance(b)),
    );
    (high + 0.05) / (low + 0.05)
}

/// The color in OKLCH, converted with the published matrices (Ottosson,
/// 2020): sRGB → linear → LMS → Oklab → polar. Returns `(l, chroma,
/// hue°)`; `hue` is meaningless at `chroma ≈ 0`.
fn oklch(color: SrgbColor) -> (f64, f64, f64) {
    fn linear(channel: u8) -> f64 {
        let srgb = f64::from(channel) / 255.0;
        if srgb <= 0.04045 {
            srgb / 12.92
        } else {
            ((srgb + 0.055) / 1.055).powf(2.4)
        }
    }
    let (r, g, b) = (
        linear(color.red()),
        linear(color.green()),
        linear(color.blue()),
    );
    let (l_, m_, s_) = (
        (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt(),
        (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt(),
        (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt(),
    );
    let lightness = 0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_;
    let a = 1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_;
    let b_ = 0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_;
    let chroma = (a * a + b_ * b_).sqrt();
    (
        lightness,
        chroma,
        b_.atan2(a).to_degrees().rem_euclid(360.0),
    )
}

/// Every preset, and the custom extremes that stress the contract:
/// gamut corners, achromatic edges and near-boundary mid tones.
const CUSTOM_EXTREMES: &[&str] = &[
    "#000000", "#ffffff", "#ff0000", "#00ff00", "#0000ff", "#ffff00", "#ff00ff", "#00ffff",
    "#808080", "#010101", "#fefefe", "#7f7f7f", "#336699", "#cc0066", "#123456", "#deadbe",
];

/// The contract's guarantees for one derived bundle (module docs
/// "Token contract"), checked on the quantized outputs.
fn check_contract(color: SrgbColor, tokens: &AppearanceTokens) {
    let (light, dark) = (tokens.light(), tokens.dark());

    // ≥ 4.5:1 strip text (SC 1.4.3).
    assert!(
        contrast(light.foreground(), light.strip()) >= 4.5,
        "{color}: light foreground vs strip {}",
        contrast(light.foreground(), light.strip())
    );
    assert!(
        contrast(dark.foreground(), dark.strip()) >= 4.5,
        "{color}: dark foreground vs strip {}",
        contrast(dark.foreground(), dark.strip())
    );

    // ≥ 3:1 markers (SC 1.4.3 large, SC 1.4.11) — against their strip,
    // and against the pure white/black backgrounds they also sit on.
    assert!(
        contrast(light.marker(), light.strip()) >= 3.0,
        "{color}: light marker"
    );
    assert!(
        contrast(dark.marker(), dark.strip()) >= 3.0,
        "{color}: dark marker"
    );
    assert!(
        contrast(light.marker(), SrgbColor::new(0xff, 0xff, 0xff)) >= 3.0,
        "{color}: light marker vs white"
    );
    assert!(
        contrast(dark.marker(), SrgbColor::new(0x00, 0x00, 0x00)) >= 3.0,
        "{color}: dark marker vs black"
    );

    // ≥ 3:1 Increase-Contrast outlines against their strip.
    assert!(
        contrast(light.outline(), light.strip()) >= 3.0,
        "{color}: light outline"
    );
    assert!(
        contrast(dark.outline(), dark.strip()) >= 3.0,
        "{color}: dark outline"
    );

    // The inks are the two candidates research 15 evaluates.
    assert_eq!(
        light.foreground(),
        SrgbColor::new(0x00, 0x00, 0x00),
        "{color}"
    );
    assert_eq!(
        dark.foreground(),
        SrgbColor::new(0xff, 0xff, 0xff),
        "{color}"
    );

    // The light variant is a light surface, the dark one a dark one.
    assert!(
        luminance(light.strip()) > luminance(dark.strip()),
        "{color}: strip variants not ordered"
    );
}

#[test]
fn palette_v1_is_the_seven_okabe_ito_colors_in_order() {
    let expected = [
        ("Blue", "#0072b2"),
        ("Orange", "#e69f00"),
        ("Bluish green", "#009e73"),
        ("Reddish purple", "#cc79a7"),
        ("Sky blue", "#56b4e9"),
        ("Vermilion", "#d55e00"),
        ("Yellow", "#f0e442"),
    ];
    assert_eq!(PaletteColor::ALL.len(), expected.len());
    for (color, (name, hex)) in PaletteColor::ALL.iter().zip(expected) {
        assert_eq!(color.name(), name);
        assert_eq!(color.color().to_hex(), hex);
    }
}

#[test]
fn parse_normalizes_to_canonical_lowercase() {
    for (input, hex) in [
        ("#0072B2", "#0072b2"),
        ("#0072b2", "#0072b2"),
        ("#Ff00Ff", "#ff00ff"),
        ("#ABCDEF", "#abcdef"),
        ("#000000", "#000000"),
        ("#f0e442", "#f0e442"),
    ] {
        let color = SrgbColor::parse(input).unwrap_or_else(|e| panic!("{input}: {e}"));
        assert_eq!(color.to_hex(), hex, "{input}");
        assert_eq!(format!("{color}"), hex, "{input}");
    }
}

#[test]
fn parse_rejects_non_canonical_inputs() {
    for (input, error) in [
        ("0072b2", SrgbColorError::MissingHash),
        ("", SrgbColorError::MissingHash),
        ("#", SrgbColorError::Length { digits: 0 }),
        ("#abc", SrgbColorError::Length { digits: 3 }),
        ("#abcd", SrgbColorError::Length { digits: 4 }),
        ("#abcdef0", SrgbColorError::Length { digits: 7 }),
        ("#abcdeff0", SrgbColorError::Length { digits: 8 }),
        ("#g0e442", SrgbColorError::Digit { offset: 0 }),
        ("#f0e44z", SrgbColorError::Digit { offset: 5 }),
        ("#１２３４５６", SrgbColorError::Digit { offset: 0 }), // full-width digits
    ] {
        assert_eq!(SrgbColor::parse(input), Err(error), "{input}");
    }
    // Anything that isn't `#` + six hex digits, including whitespace and
    // an alpha suffix the CHECK in spec/02 forbids.
    for input in [" #abcdef", "#abcdef ", "#aabbccdd", "##abcdef", "-#abcdef"] {
        assert!(SrgbColor::parse(input).is_err(), "{input}");
    }
}

#[test]
fn every_palette_color_meets_the_contrast_contract() {
    for palette in PaletteColor::ALL {
        let color = palette.color();
        check_contract(color, &color.derive_tokens());
    }
}

#[test]
fn custom_extremes_meet_the_contrast_contract() {
    for input in CUSTOM_EXTREMES {
        let color = SrgbColor::parse(input).unwrap();
        check_contract(color, &color.derive_tokens());
    }
}

#[test]
fn quantized_contrast_holds_across_the_srgb_cube() {
    const LEVELS: [u8; 9] = [0, 32, 64, 96, 128, 160, 192, 224, 255];
    for red in LEVELS {
        for green in LEVELS {
            for blue in LEVELS {
                let color = SrgbColor::new(red, green, blue);
                check_contract(color, &color.derive_tokens());
            }
        }
    }
}

#[test]
fn tokens_preserve_the_stored_hue() {
    // Every preset and the chromatic extremes: hue is an angle, so the
    // check is the wrapped difference — skipped where either color is
    // achromatic, where hue is undefined.
    let cases = PaletteColor::ALL
        .iter()
        .map(|palette| palette.color())
        .chain(
            CUSTOM_EXTREMES
                .iter()
                .map(|input| SrgbColor::parse(input).unwrap()),
        );
    for color in cases {
        let (.., stored_chroma, stored_hue) = oklch(color);
        if stored_chroma < 0.02 {
            continue;
        }
        let tokens = color.derive_tokens();
        for token in [
            tokens.light().marker(),
            tokens.light().strip(),
            tokens.light().outline(),
            tokens.dark().marker(),
            tokens.dark().strip(),
            tokens.dark().outline(),
        ] {
            let (.., chroma, hue) = oklch(token);
            if chroma < 0.02 {
                continue;
            }
            let difference = (hue - stored_hue)
                .abs()
                .min(360.0 - (hue - stored_hue).abs());
            assert!(
                difference <= 10.0,
                "{color}: token {token} hue {hue:.1}° vs stored {stored_hue:.1}°"
            );
        }
    }
}

#[test]
fn markers_stay_saturated_and_strips_stay_restrained() {
    for palette in PaletteColor::ALL {
        let (.., stored_chroma, _) = oklch(palette.color());
        let tokens = palette.color().derive_tokens();
        for variant in [tokens.light(), tokens.dark()] {
            let (.., marker_chroma, _) = oklch(variant.marker());
            assert!(
                marker_chroma >= stored_chroma * 0.5,
                "{}: marker chroma {marker_chroma:.3} vs stored {stored_chroma:.3}",
                palette.color()
            );
            let (.., strip_chroma, _) = oklch(variant.strip());
            assert!(
                strip_chroma <= stored_chroma + 0.001,
                "{}: strip chroma {strip_chroma:.3} exceeds stored {stored_chroma:.3}",
                palette.color()
            );
        }
    }
}

#[test]
fn derivation_is_deterministic() {
    for palette in PaletteColor::ALL {
        assert_eq!(
            palette.color().derive_tokens(),
            palette.color().derive_tokens()
        );
    }
}
