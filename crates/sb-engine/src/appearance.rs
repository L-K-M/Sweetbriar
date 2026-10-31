//! Account appearance's pure color layer (research 15 "Color design",
//! spec/02 `account_appearance`/`identity_appearance`, spec/08
//! "Identity strip and account colors"): the v1 palette, the
//! custom-color parser and the OKLCH-derived rendering tokens.
//! Deterministic math over one stored sRGB color — no store, no
//! platform seams. Palette assignment, persistence, the service
//! commands and the shells' use of the tokens land in the later M1.10
//! slices; this module is the library they all consume.
//!
//! ## Token contract
//!
//! One stored color yields a bundle per appearance — light and dark —
//! each holding the four roles research 15 names ("separate light, dark
//! and high-contrast variants", spec/08):
//!
//! - `marker`: the saturated accent drawn as the 4 pt list rail and the
//!   monograms. Kept at the stored lightness when the color already
//!   clears the bar, otherwise darkened (light appearance) or lightened
//!   (dark appearance) until it reaches **3:1 against the variant's
//!   strip** — WCAG's minimum for large text and essential non-text
//!   indicators (SC 1.4.3 large, SC 1.4.11). The strip is the strictest
//!   opaque surface used by this bundle. Shells must check contrast on
//!   their actual surfaces, including selected rows.
//! - `strip`: the identity strip's opaque, restrained fill — fixed
//!   lightness per appearance, the stored chroma capped to a tint.
//! - `foreground`: the strip's text and content, picked per surface
//!   between pure black and pure white; research 15 shows neither ink
//!   fits every hue. The fixed strip lightness guarantees **4.5:1**
//!   (SC 1.4.3) for every input.
//! - `outline`: the Increase-Contrast edge and separator, hue-preserving
//!   like the marker but pushed halfway from the 3:1 boundary to the
//!   contrast extreme, so the boundary is visible, not just met.
//!
//! ## Derivation
//!
//! The stored color is converted to OKLCH (the `palette` crate), and
//! each token keeps its hue and its chroma — folded onto the sRGB gamut
//! surface where needed — while lightness is solved per role on the
//! *quantized* sRGB output. The guarantees therefore hold for the exact
//! channel bytes handed to the shells, not just the f32 intermediates,
//! and nothing extra needs persisting when the appearance changes.

use std::fmt;

use palette::{Clamp, IsWithinBounds, OklabHue, Oklch, Srgb, convert::FromColorUnclamped};

/// WCAG's minimum contrast for ordinary text (SC 1.4.3): the strip
/// foreground against its fill.
const TEXT_RATIO: f64 = 4.5;
/// WCAG's minimum for large text and essential non-text indicators (SC
/// 1.4.3 large, SC 1.4.11): markers and the outline against the surface
/// they're drawn on.
const GRAPHIC_RATIO: f64 = 3.0;
/// Headroom over a bound when lightness is solved, so a token never
/// lands on the exact ratio boundary after quantization.
const SOLVE_MARGIN: f64 = 0.05;

// WCAG 2.2 relative luminance uses the sRGB transfer curve and Rec. 709
// weights. These are standard definitions, not tuning parameters.
const SRGB_BYTE_MAX: f64 = u8::MAX as f64;
const SRGB_LINEAR_LIMIT: f64 = 0.04045;
const SRGB_LINEAR_DIVISOR: f64 = 12.92;
const SRGB_TRANSFER_OFFSET: f64 = 0.055;
const SRGB_TRANSFER_SCALE: f64 = 1.055;
const SRGB_TRANSFER_POWER: f64 = 2.4;
const RED_LUMINANCE_WEIGHT: f64 = 0.2126;
const GREEN_LUMINANCE_WEIGHT: f64 = 0.7152;
const BLUE_LUMINANCE_WEIGHT: f64 = 0.0722;
const WCAG_LUMINANCE_OFFSET: f64 = 0.05;

const SRGB_CHANNELS: usize = 3;
const HEX_DIGITS_PER_CHANNEL: usize = 2;
const SRGB_HEX_DIGITS: usize = SRGB_CHANNELS * HEX_DIGITS_PER_CHANNEL;
const HEX_RADIX: u32 = 16;
const HEX_NIBBLE_BITS: u32 = 4;
const GAMUT_SEARCH_STEPS: usize = 24;
const LIGHTNESS_SEARCH_STEPS: usize = 32;

/// The light strip's fixed OKLCH lightness and chroma cap: a restrained
/// tint that always admits a `TEXT_RATIO` dark foreground.
const STRIP_LIGHT_L: f32 = 0.88;
const STRIP_LIGHT_C: f32 = 0.06;
/// The dark strip's fixed lightness and chroma cap: a deep tint that
/// always admits a `TEXT_RATIO` light foreground.
const STRIP_DARK_L: f32 = 0.30;
const STRIP_DARK_C: f32 = 0.07;

/// The two inks the strip foreground is picked between.
const INK_DARK: SrgbColor = SrgbColor::new(0x00, 0x00, 0x00);
const INK_LIGHT: SrgbColor = SrgbColor::new(0xff, 0xff, 0xff);

/// One opaque sRGB color — the unit appearance colors are stored,
/// validated and rendered in. Its canonical text form is lowercase
/// `#rrggbb` (spec/02 rule 10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SrgbColor {
    red: u8,
    green: u8,
    blue: u8,
}

impl SrgbColor {
    /// An opaque sRGB color from its channel values.
    pub const fn new(red: u8, green: u8, blue: u8) -> Self {
        Self { red, green, blue }
    }

    /// Parses `#rrggbb`, the stored form. Per spec/02 input is
    /// lowercased before validation, so the palette's published
    /// uppercase values parse; everything else — a missing '#', the
    /// `#rgb` shorthand, an alpha suffix, a stray digit — is rejected.
    pub fn parse(text: &str) -> Result<Self, SrgbColorError> {
        let digits = text.strip_prefix('#').ok_or(SrgbColorError::MissingHash)?;
        if digits.chars().count() != SRGB_HEX_DIGITS {
            return Err(SrgbColorError::Length {
                digits: digits.chars().count(),
            });
        }
        let mut channels = [0u8; SRGB_CHANNELS];
        for (index, digit) in digits.chars().enumerate() {
            let nibble = digit
                .to_digit(HEX_RADIX)
                .ok_or(SrgbColorError::Digit { offset: index })? as u8;
            let channel = &mut channels[index / HEX_DIGITS_PER_CHANNEL];
            *channel = *channel << HEX_NIBBLE_BITS | nibble;
        }
        Ok(Self::new(channels[0], channels[1], channels[2]))
    }

    /// The red channel value.
    pub const fn red(&self) -> u8 {
        self.red
    }

    /// The green channel value.
    pub const fn green(&self) -> u8 {
        self.green
    }

    /// The blue channel value.
    pub const fn blue(&self) -> u8 {
        self.blue
    }

    /// The canonical lowercase `#rrggbb` (spec/02 rule 10).
    pub fn to_hex(&self) -> String {
        self.to_string()
    }

    /// The WCAG relative luminance of the color as drawn: linearize
    /// each quantized channel per the sRGB transfer curve and sum with
    /// the Rec. 709 weights.
    fn luminance(self) -> f64 {
        fn linear(channel: u8) -> f64 {
            let srgb = f64::from(channel) / SRGB_BYTE_MAX;
            if srgb <= SRGB_LINEAR_LIMIT {
                srgb / SRGB_LINEAR_DIVISOR
            } else {
                ((srgb + SRGB_TRANSFER_OFFSET) / SRGB_TRANSFER_SCALE).powf(SRGB_TRANSFER_POWER)
            }
        }
        RED_LUMINANCE_WEIGHT * linear(self.red)
            + GREEN_LUMINANCE_WEIGHT * linear(self.green)
            + BLUE_LUMINANCE_WEIGHT * linear(self.blue)
    }

    /// The WCAG contrast ratio of the two colors, in [1, 21].
    fn contrast(self, other: SrgbColor) -> f64 {
        let (high, low) = (
            self.luminance().max(other.luminance()),
            self.luminance().min(other.luminance()),
        );
        (high + WCAG_LUMINANCE_OFFSET) / (low + WCAG_LUMINANCE_OFFSET)
    }

    /// The rendering tokens for both appearances (module docs "Token
    /// contract"). Pure and deterministic.
    pub fn derive_tokens(self) -> AppearanceTokens {
        let Oklch { l, chroma, hue, .. } = oklch(self);

        // The strips: restrained fills at a fixed lightness; the stored
        // chroma capped so the fill stays a tint.
        let strip_light = render(STRIP_LIGHT_L, chroma.min(STRIP_LIGHT_C), hue);
        let strip_dark = render(STRIP_DARK_L, chroma.min(STRIP_DARK_C), hue);

        // The markers: the saturated accent, at the stored lightness
        // when that already clears the bar against the strictest
        // surface it sits on — its appearance's strip.
        let marker_light = {
            let bound = darkest_luminance(strip_light.luminance(), GRAPHIC_RATIO);
            let stored = render(l, chroma, hue);
            if stored.luminance() <= bound {
                stored
            } else {
                let bound =
                    darkest_luminance(strip_light.luminance(), GRAPHIC_RATIO + SOLVE_MARGIN);
                render(max_l_at_luminance(hue, chroma, bound), chroma, hue)
            }
        };
        let marker_dark = {
            let bound = lightest_luminance(strip_dark.luminance(), GRAPHIC_RATIO);
            let stored = render(l, chroma, hue);
            if stored.luminance() >= bound {
                stored
            } else {
                let bound =
                    lightest_luminance(strip_dark.luminance(), GRAPHIC_RATIO + SOLVE_MARGIN);
                render(min_l_at_luminance(hue, chroma, bound), chroma, hue)
            }
        };

        // The outlines: a visible Increase-Contrast edge, pushed
        // halfway from the 3:1 boundary toward the contrast extreme.
        let outline_light = {
            let bound = darkest_luminance(strip_light.luminance(), GRAPHIC_RATIO + SOLVE_MARGIN);
            render(max_l_at_luminance(hue, chroma, bound / 2.0), chroma, hue)
        };
        let outline_dark = {
            let bound = lightest_luminance(strip_dark.luminance(), GRAPHIC_RATIO + SOLVE_MARGIN);
            render(
                min_l_at_luminance(hue, chroma, (bound + 1.0) / 2.0),
                chroma,
                hue,
            )
        };

        // The foregrounds: the better ink per surface.
        let pick = |strip: SrgbColor| {
            if INK_DARK.contrast(strip) >= INK_LIGHT.contrast(strip) {
                INK_DARK
            } else {
                INK_LIGHT
            }
        };
        let foreground_light = pick(strip_light);
        let foreground_dark = pick(strip_dark);

        let tokens = AppearanceTokens::new(
            AppearanceVariant::new(marker_light, strip_light, foreground_light, outline_light),
            AppearanceVariant::new(marker_dark, strip_dark, foreground_dark, outline_dark),
        );
        debug_assert!(tokens.light().foreground().contrast(strip_light) >= TEXT_RATIO);
        debug_assert!(tokens.dark().foreground().contrast(strip_dark) >= TEXT_RATIO);
        debug_assert!(marker_light.contrast(strip_light) >= GRAPHIC_RATIO);
        debug_assert!(marker_dark.contrast(strip_dark) >= GRAPHIC_RATIO);
        debug_assert!(outline_light.contrast(strip_light) >= GRAPHIC_RATIO);
        debug_assert!(outline_dark.contrast(strip_dark) >= GRAPHIC_RATIO);
        tokens
    }
}

impl fmt::Display for SrgbColor {
    /// The canonical lowercase `#rrggbb` (spec/02 rule 10).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.red, self.green, self.blue)
    }
}

impl std::str::FromStr for SrgbColor {
    type Err = SrgbColorError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}

/// Why a text isn't a `SrgbColor` (see `SrgbColor::parse`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SrgbColorError {
    /// The input doesn't start with '#'.
    MissingHash,
    /// The field after '#' isn't exactly six hexadecimal digits.
    Length { digits: usize },
    /// The character at `offset` (0-based, into the six-digit field)
    /// isn't a hexadecimal digit.
    Digit { offset: usize },
}

impl fmt::Display for SrgbColorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingHash => write!(f, "expected '#' followed by six hexadecimal digits"),
            Self::Length { digits } => {
                write!(
                    f,
                    "expected six hexadecimal digits after '#', found {digits}"
                )
            }
            Self::Digit { offset } => {
                write!(
                    f,
                    "character {offset} of the six-digit field isn't hexadecimal"
                )
            }
        }
    }
}

impl std::error::Error for SrgbColorError {}

/// Palette v1: the seven chromatic Okabe–Ito colors in the order new
/// accounts are offered them (research 15 "Default palette"). The
/// separate neutral choice and the custom picker belong to the settings
/// slice; assignment itself to a later one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteColor {
    /// Okabe–Ito blue, `#0072B2`.
    Blue,
    /// Okabe–Ito orange, `#E69F00`.
    Orange,
    /// Okabe–Ito bluish green, `#009E73`.
    BluishGreen,
    /// Okabe–Ito reddish purple, `#CC79A7`.
    ReddishPurple,
    /// Okabe–Ito sky blue, `#56B4E9`.
    SkyBlue,
    /// Okabe–Ito vermilion, `#D55E00`.
    Vermilion,
    /// Okabe–Ito yellow, `#F0E442`.
    Yellow,
}

impl PaletteColor {
    /// Every palette color, in assignment order.
    pub const ALL: [Self; 7] = [
        Self::Blue,
        Self::Orange,
        Self::BluishGreen,
        Self::ReddishPurple,
        Self::SkyBlue,
        Self::Vermilion,
        Self::Yellow,
    ];

    /// The picker's display name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Blue => "Blue",
            Self::Orange => "Orange",
            Self::BluishGreen => "Bluish green",
            Self::ReddishPurple => "Reddish purple",
            Self::SkyBlue => "Sky blue",
            Self::Vermilion => "Vermilion",
            Self::Yellow => "Yellow",
        }
    }

    /// The stored sRGB color.
    pub const fn color(self) -> SrgbColor {
        match self {
            Self::Blue => SrgbColor::new(0x00, 0x72, 0xb2),
            Self::Orange => SrgbColor::new(0xe6, 0x9f, 0x00),
            Self::BluishGreen => SrgbColor::new(0x00, 0x9e, 0x73),
            Self::ReddishPurple => SrgbColor::new(0xcc, 0x79, 0xa7),
            Self::SkyBlue => SrgbColor::new(0x56, 0xb4, 0xe9),
            Self::Vermilion => SrgbColor::new(0xd5, 0x5e, 0x00),
            Self::Yellow => SrgbColor::new(0xf0, 0xe4, 0x42),
        }
    }
}

/// One appearance's tokens (module docs "Token contract"): `light` for
/// the light appearance, `dark` for the dark one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppearanceVariant {
    marker: SrgbColor,
    strip: SrgbColor,
    foreground: SrgbColor,
    outline: SrgbColor,
}

impl AppearanceVariant {
    fn new(marker: SrgbColor, strip: SrgbColor, foreground: SrgbColor, outline: SrgbColor) -> Self {
        Self {
            marker,
            strip,
            foreground,
            outline,
        }
    }

    /// The saturated accent: the 4 pt list rail and the monograms.
    /// ≥ 3:1 against `strip`.
    pub const fn marker(&self) -> SrgbColor {
        self.marker
    }

    /// The identity strip's restrained opaque fill.
    pub const fn strip(&self) -> SrgbColor {
        self.strip
    }

    /// The strip's text and content ink. ≥ 4.5:1 against `strip`.
    pub const fn foreground(&self) -> SrgbColor {
        self.foreground
    }

    /// The Increase-Contrast edge and separator. ≥ 3:1 against `strip`.
    pub const fn outline(&self) -> SrgbColor {
        self.outline
    }
}

/// The tokens derived from one stored color for both appearances
/// (`SrgbColor::derive_tokens`; module docs "Token contract").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppearanceTokens {
    light: AppearanceVariant,
    dark: AppearanceVariant,
}

impl AppearanceTokens {
    fn new(light: AppearanceVariant, dark: AppearanceVariant) -> Self {
        Self { light, dark }
    }

    /// The light appearance's tokens.
    pub const fn light(&self) -> &AppearanceVariant {
        &self.light
    }

    /// The dark appearance's tokens.
    pub const fn dark(&self) -> &AppearanceVariant {
        &self.dark
    }
}

/// The color in OKLCH. Unclamped conversion — the input is in-gamut
/// sRGB by construction.
fn oklch(color: SrgbColor) -> Oklch<f32> {
    let srgb = Srgb::<u8>::new(color.red, color.green, color.blue).into_format::<f32>();
    Oklch::from_color_unclamped(srgb)
}

/// The largest chroma up to `chroma` whose (`l`, `hue`) still converts
/// inside the sRGB gamut; folding chroma to the gamut surface keeps hue
/// and lightness intact (research 15's gamut mapping).
fn fit_chroma(l: f32, chroma: f32, hue: OklabHue<f32>) -> f32 {
    let in_gamut = |chroma: f32| {
        Srgb::<f32>::from_color_unclamped(Oklch::new(l, chroma, hue)).is_within_bounds()
    };
    if in_gamut(chroma) {
        return chroma;
    }
    let (mut low, mut high) = (0.0f32, chroma);
    for _ in 0..GAMUT_SEARCH_STEPS {
        let mid = (low + high) / 2.0;
        if in_gamut(mid) {
            low = mid;
        } else {
            high = mid;
        }
    }
    low
}

/// OKLCH → quantized sRGB: chroma folded to the gamut surface, then
/// clamped and rounded to whole channel values — the exact bytes the
/// contract's ratios are checked on.
fn render(l: f32, chroma: f32, hue: OklabHue<f32>) -> SrgbColor {
    let srgb = Srgb::<f32>::from_color_unclamped(Oklch::new(l, fit_chroma(l, chroma, hue), hue));
    let srgb = srgb.clamp().into_format::<u8>();
    SrgbColor::new(srgb.red, srgb.green, srgb.blue)
}

/// The darkest luminance still within `ratio` of a `surface`-luminance
/// background: the bound for a token that must sit below it.
fn darkest_luminance(surface: f64, ratio: f64) -> f64 {
    (surface + WCAG_LUMINANCE_OFFSET) / ratio - WCAG_LUMINANCE_OFFSET
}

/// The lightest luminance still within `ratio` of a `surface`-luminance
/// background: the bound for a token that must sit above it.
fn lightest_luminance(surface: f64, ratio: f64) -> f64 {
    ratio * (surface + WCAG_LUMINANCE_OFFSET) - WCAG_LUMINANCE_OFFSET
}

/// The greatest OKLCH lightness whose rendered color keeps luminance at
/// or under `bound`. Always feasible: at lightness 0 the color is black
/// and its luminance 0.
fn max_l_at_luminance(hue: OklabHue<f32>, chroma: f32, bound: f64) -> f32 {
    let (mut low, mut high) = (0.0f32, 1.0f32);
    for _ in 0..LIGHTNESS_SEARCH_STEPS {
        let mid = (low + high) / 2.0;
        if render(mid, chroma, hue).luminance() <= bound {
            low = mid;
        } else {
            high = mid;
        }
    }
    low
}

/// The least OKLCH lightness whose rendered color reaches `bound`
/// luminance. Feasible for any bound under white's 1.0.
fn min_l_at_luminance(hue: OklabHue<f32>, chroma: f32, bound: f64) -> f32 {
    let (mut low, mut high) = (0.0f32, 1.0f32);
    for _ in 0..LIGHTNESS_SEARCH_STEPS {
        let mid = (low + high) / 2.0;
        if render(mid, chroma, hue).luminance() >= bound {
            high = mid;
        } else {
            low = mid;
        }
    }
    high
}
