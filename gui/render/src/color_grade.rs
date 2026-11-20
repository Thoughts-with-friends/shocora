//! Colour adjustments applied to the TV and GamePad pictures when they are shown:
//! brightness, contrast, saturation, vibrance and gamma.
//!
//! They work on the final (display-encoded) picture, after the title's own output, so
//! they change only how the picture looks: what the title reads back is untouched.
use std::sync::{Arc, Mutex};

/// The adjustments, each at its neutral value by default.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(default)]
pub struct ColorGrade {
    /// Multiplies the colour (1 = unchanged).
    pub brightness: f32,
    /// Spreads the colours away from mid grey (1 = unchanged).
    pub contrast: f32,
    /// Moves every colour away from its grey (1 = unchanged, 0 = black and white).
    pub saturation: f32,
    /// Saturates dull colours more than vivid ones (0 = unchanged).
    pub vibrance: f32,
    /// Display gamma: above 1 lifts the shadows and mid tones (1 = unchanged).
    pub gamma: f32,
}

/// Only used to detect changes; a NaN from a hand-edited config just compares unequal.
impl Eq for ColorGrade {}

impl Default for ColorGrade {
    fn default() -> Self {
        Self::NEUTRAL
    }
}

impl ColorGrade {
    /// No change.
    pub const NEUTRAL: Self =
        Self { brightness: 1.0, contrast: 1.0, saturation: 1.0, vibrance: 0.0, gamma: 1.0 };

    /// Closer to the Switch release of Xenoblade Chronicles X: a brighter picture with
    /// lifted shadows and more vivid colour than the Wii U original.
    pub const SWITCH_LIKE: Self =
        Self { brightness: 1.08, contrast: 1.06, saturation: 1.18, vibrance: 0.25, gamma: 1.12 };

    /// Slider ranges offered in the settings.
    pub const BRIGHTNESS_RANGE: std::ops::RangeInclusive<f32> = 0.5..=1.5;
    pub const CONTRAST_RANGE: std::ops::RangeInclusive<f32> = 0.5..=1.5;
    pub const SATURATION_RANGE: std::ops::RangeInclusive<f32> = 0.0..=2.0;
    pub const VIBRANCE_RANGE: std::ops::RangeInclusive<f32> = -1.0..=1.0;
    pub const GAMMA_RANGE: std::ops::RangeInclusive<f32> = 0.5..=2.0;

    #[inline]
    pub fn is_neutral(&self) -> bool {
        *self == Self::NEUTRAL
    }

    /// The values clamped to the ranges offered (a hand-edited config may hold anything).
    pub fn clamped(self) -> Self {
        let clamp = |v: f32, range: std::ops::RangeInclusive<f32>, neutral: f32| {
            if v.is_finite() { v.clamp(*range.start(), *range.end()) } else { neutral }
        };
        Self {
            brightness: clamp(self.brightness, Self::BRIGHTNESS_RANGE, 1.0),
            contrast: clamp(self.contrast, Self::CONTRAST_RANGE, 1.0),
            saturation: clamp(self.saturation, Self::SATURATION_RANGE, 1.0),
            vibrance: clamp(self.vibrance, Self::VIBRANCE_RANGE, 0.0),
            gamma: clamp(self.gamma, Self::GAMMA_RANGE, 1.0),
        }
    }

    /// Uniform data of [`SHADER`]: `brightness, contrast, saturation, vibrance, 1/gamma`
    /// padded to 32 bytes.
    pub(crate) fn uniform_bytes(self) -> [u8; 32] {
        let g = self.clamped();
        let values =
            [g.brightness, g.contrast, g.saturation, g.vibrance, 1.0 / g.gamma, 0.0, 0.0, 0.0];
        let mut bytes = [0_u8; 32];
        for (chunk, value) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(values) {
            chunk.copy_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    /// The same adjustment on the CPU (tests and reference for the shader).
    pub fn apply(self, rgb: [f32; 3]) -> [f32; 3] {
        let g = self.clamped();
        let mut c = rgb.map(|v| v * g.brightness);
        c = c.map(|v| (v - 0.5).mul_add(g.contrast, 0.5));
        let luma = c[2].mul_add(0.0722, c[1].mul_add(0.7152, c[0] * 0.2126));
        c = c.map(|v| (v - luma).mul_add(g.saturation, luma));
        let max = c[0].max(c[1]).max(c[2]);
        let min = c[0].min(c[1]).min(c[2]);
        let amount = g.vibrance.mul_add(1.0 - (max - min).clamp(0.0, 1.0), 1.0);
        c = c.map(|v| (v - luma).mul_add(amount, luma));
        c.map(|v| v.clamp(0.0, 1.0).powf(1.0 / g.gamma))
    }
}

/// WGSL of the adjustment, appended to the present shader.
pub(crate) const SHADER: &str = "
struct ColorGrade { brightness: f32, contrast: f32, saturation: f32, vibrance: f32, inv_gamma: f32 }
@group(0) @binding(2) var<uniform> grade: ColorGrade;
fn color_grade(rgb: vec3<f32>) -> vec3<f32> {
    var c = rgb * grade.brightness;
    c = (c - vec3<f32>(0.5)) * grade.contrast + vec3<f32>(0.5);
    let luma = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
    c = vec3<f32>(luma) + (c - vec3<f32>(luma)) * grade.saturation;
    let spread = clamp(max(c.r, max(c.g, c.b)) - min(c.r, min(c.g, c.b)), 0.0, 1.0);
    c = vec3<f32>(luma) + (c - vec3<f32>(luma)) * (1.0 + grade.vibrance * (1.0 - spread));
    return pow(clamp(c, vec3<f32>(0.0), vec3<f32>(1.0)), vec3<f32>(grade.inv_gamma));
}
";

/// The adjustments in effect, changeable from any thread while the renderer runs.
#[derive(Debug, Clone, Default)]
pub struct ColorGradeSetting(Arc<Mutex<ColorGrade>>);

impl ColorGradeSetting {
    pub fn set(&self, grade: ColorGrade) {
        if let Ok(mut current) = self.0.lock() {
            *current = grade.clamped();
        }
    }

    pub fn get(&self) -> ColorGrade {
        self.0.lock().map(|g| *g).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::ColorGrade;

    /// Intent: the neutral setting leaves every colour as it is.
    /// Pass: a few colours come back unchanged (within float rounding).
    #[test]
    fn neutral_changes_nothing() {
        for rgb in [[0.0, 0.0, 0.0], [0.2, 0.5, 0.9], [1.0, 1.0, 1.0]] {
            let out = ColorGrade::NEUTRAL.apply(rgb);
            for (a, b) in out.iter().zip(rgb) {
                assert!((a - b).abs() < 1e-5, "{out:?} vs {rgb:?}");
            }
        }
    }

    /// Intent: the Switch-like preset brightens the picture and makes colours more vivid.
    /// Pass: a dull mid-tone gets brighter and its channels spread further apart.
    #[test]
    fn switch_like_is_brighter_and_more_vivid() {
        let rgb = [0.35, 0.40, 0.30];
        let out = ColorGrade::SWITCH_LIKE.apply(rgb);
        let sum = |c: [f32; 3]| c.iter().sum::<f32>();
        let spread = |c: [f32; 3]| c[1] - c[2];
        assert!(sum(out) > sum(rgb), "{out:?}");
        assert!(spread(out) > spread(rgb), "{out:?}");
    }

    /// Intent: out-of-range values from a hand-edited config cannot produce NaN or
    /// extreme pictures.
    /// Pass: gamma 0 and NaN brightness are replaced by values in range.
    #[test]
    fn clamps_bad_values() {
        let g = ColorGrade { brightness: f32::NAN, gamma: 0.0, ..ColorGrade::NEUTRAL }.clamped();
        assert!((g.brightness - 1.0).abs() < f32::EPSILON);
        assert!((g.gamma - 0.5).abs() < f32::EPSILON);
    }
}
