//! The colours of a brand, worked out from one accent colour.
//!
//! The portal and the webmail share their design tokens (`--uwu-pink`, `--uwu-pink-solid`, …). An
//! admin picks one colour; this derives the rest for the light and the dark theme in OKLCH, where
//! "lighter" and "darker" look the way they sound, and moves each until its text is readable
//! (WCAG contrast 4.5:1 for buttons and links). Without a chosen colour nothing is overridden and
//! the UwUMail pink stays exactly as designed.

use std::fmt::Write as _;

/// A colour in sRGB, each channel 0–1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgb(pub f64, pub f64, pub f64);

/// `#rrggbb` or `#rgb`, any case.
pub fn parse_hex(text: &str) -> Option<Rgb> {
    let hex = text.trim().strip_prefix('#')?;
    let expanded: String = match hex.len() {
        3 => hex.chars().flat_map(|c| [c, c]).collect(),
        6 => hex.to_string(),
        _ => return None,
    };
    let value = u32::from_str_radix(&expanded, 16).ok()?;
    let channel = |shift: u32| f64::from((value >> shift) & 0xff) / 255.0;
    Some(Rgb(channel(16), channel(8), channel(0)))
}

impl Rgb {
    pub fn hex(self) -> String {
        let byte = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        format!("#{:02x}{:02x}{:02x}", byte(self.0), byte(self.1), byte(self.2))
    }

    fn rgb_triplet(self) -> String {
        let byte = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        format!("{} {} {}", byte(self.0), byte(self.1), byte(self.2))
    }

    /// Relative luminance after WCAG 2.
    fn luminance(self) -> f64 {
        let linear = |v: f64| if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
        0.2126 * linear(self.0) + 0.7152 * linear(self.1) + 0.0722 * linear(self.2)
    }
}

/// How well two colours read on each other, from 1 (not at all) to 21.
pub fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (a.luminance(), b.luminance());
    let (light, dark) = if la > lb { (la, lb) } else { (lb, la) };
    (light + 0.05) / (dark + 0.05)
}

/// Lightness, chroma and hue (degrees).
#[derive(Debug, Clone, Copy)]
struct Oklch(f64, f64, f64);

fn to_linear(v: f64) -> f64 {
    if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

fn from_linear(v: f64) -> f64 {
    if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

fn oklch(rgb: Rgb) -> Oklch {
    let (r, g, b) = (to_linear(rgb.0), to_linear(rgb.1), to_linear(rgb.2));
    let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
    let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
    let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
    let lightness = 0.210_454_255_3 * l + 0.793_617_785 * m - 0.004_072_046_8 * s;
    let a = 1.977_998_495_1 * l - 2.428_592_205 * m + 0.450_593_709_9 * s;
    let b = 0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766 * s;
    Oklch(lightness, a.hypot(b), b.atan2(a).to_degrees())
}

/// The colour, or `None` when it lies outside sRGB.
fn try_rgb(color: Oklch) -> Option<Rgb> {
    let Oklch(lightness, chroma, hue) = color;
    let (a, b) = (chroma * hue.to_radians().cos(), chroma * hue.to_radians().sin());
    let l = (lightness + 0.396_337_777_4 * a + 0.215_803_757_3 * b).powi(3);
    let m = (lightness - 0.105_561_345_8 * a - 0.063_854_172_8 * b).powi(3);
    let s = (lightness - 0.089_484_177_5 * a - 1.291_485_548 * b).powi(3);
    let r = 4.076_741_662_1 * l - 3.307_711_591_3 * m + 0.230_969_929_2 * s;
    let g = -1.268_438_004_6 * l + 2.609_757_401_1 * m - 0.341_319_396_5 * s;
    let b = -0.004_196_086_3 * l - 0.703_418_614_7 * m + 1.707_614_701 * s;
    let inside = |v: f64| (-0.000_1..=1.000_1).contains(&v);
    (inside(r) && inside(g) && inside(b))
        .then(|| Rgb(from_linear(r.max(0.0)), from_linear(g.max(0.0)), from_linear(b.max(0.0))))
}

/// The colour at this lightness and hue, with as much of the chroma as sRGB can show.
fn rgb(color: Oklch) -> Rgb {
    let Oklch(lightness, chroma, hue) = color;
    let lightness = lightness.clamp(0.0, 1.0);
    let mut chroma = chroma.max(0.0);
    loop {
        if let Some(rgb) = try_rgb(Oklch(lightness, chroma, hue)) {
            return rgb;
        }
        if chroma <= 0.0 {
            return Rgb(lightness, lightness, lightness);
        }
        chroma = (chroma - 0.005).max(0.0);
    }
}

/// Moves the lightness in `step`s until `text` reads on the colour, or it cannot move further.
fn readable_behind(start: Oklch, text: Rgb, step: f64) -> Rgb {
    let mut color = start;
    for _ in 0..100 {
        let candidate = rgb(color);
        if contrast(candidate, text) >= 4.5 || !(0.0..=1.0).contains(&(color.0 + step)) {
            return candidate;
        }
        color.0 += step;
    }
    rgb(color)
}

/// One theme's tokens, in the order they are written.
pub type Tokens = Vec<(&'static str, String)>;

pub struct Palette {
    pub light: Tokens,
    pub dark: Tokens,
}

const WHITE: Rgb = Rgb(1.0, 1.0, 1.0);
/// `--uwu-ink` of the light theme, the text on accent buttons in the dark one.
const INK: Rgb = Rgb(28.0 / 255.0, 20.0 / 255.0, 32.0 / 255.0);
const DARK_SURFACE: Rgb = Rgb(28.0 / 255.0, 23.0 / 255.0, 31.0 / 255.0);

/// The accent tokens for `accent`, readable in both themes.
pub fn palette(accent: Rgb) -> Palette {
    let Oklch(lightness, chroma, hue) = oklch(accent);
    // A grey accent gives grey tints; everything else keeps its own hue.
    let at = |l: f64, c: f64| Oklch(l, c, hue);

    // Worked out first: the light theme's dark toasts carry the dark theme's accent.
    let dark_accent = readable_behind(at(lightness.max(0.74), chroma), INK, 0.01);
    let solid = readable_behind(at(lightness.min(0.62), chroma), WHITE, -0.01);
    let solid_lch = oklch(solid);
    let light: Tokens = vec![
        ("--uwu-pink", accent.hex()),
        ("--uwu-pink-solid", solid.hex()),
        ("--uwu-pink-solid-hover", rgb(Oklch(solid_lch.0 - 0.06, solid_lch.1, hue)).hex()),
        ("--uwu-on-pink", "#ffffff".into()),
        ("--uwu-pink-ink", readable_behind(at(lightness.min(0.5), chroma), rgb(at(0.95, 0.03)), -0.01).hex()),
        ("--uwu-pink-tint", rgb(at(0.95, chroma.min(0.035))).hex()),
        ("--uwu-pink-tint-strong", rgb(at(0.91, chroma.min(0.06))).hex()),
        ("--uwu-account-pink", accent.hex()),
        ("--uwu-focus", format!("0 0 0 3px rgb({} / 0.35)", accent.rgb_triplet())),
        ("--uwu-toast-accent", dark_accent.hex()),
    ];

    let dark_lch = oklch(dark_accent);
    let dark: Tokens = vec![
        ("--uwu-pink", dark_accent.hex()),
        ("--uwu-pink-solid", dark_accent.hex()),
        ("--uwu-pink-solid-hover", rgb(Oklch(dark_lch.0 + 0.05, dark_lch.1, hue)).hex()),
        ("--uwu-on-pink", INK.hex()),
        ("--uwu-pink-ink", readable_behind(at(lightness.max(0.82), chroma), DARK_SURFACE, 0.01).hex()),
        ("--uwu-pink-tint", rgb(at(0.28, chroma.min(0.05))).hex()),
        ("--uwu-pink-tint-strong", rgb(at(0.33, chroma.min(0.065))).hex()),
        ("--uwu-focus", format!("0 0 0 3px rgb({} / 0.4)", dark_accent.rgb_triplet())),
    ];
    Palette { light, dark }
}

impl Palette {
    /// A stylesheet that overrides the built-in tokens. `html:root` outranks the `:root` of the
    /// apps' own stylesheets, whichever of them the browser happens to load last.
    pub fn css(&self) -> String {
        let mut css = String::from("html:root {\n");
        for (name, value) in &self.light {
            let _ = writeln!(css, "  {name}: {value};");
        }
        css.push_str("}\nhtml:root[data-theme=\"dark\"] {\n");
        for (name, value) in &self.dark {
            let _ = writeln!(css, "  {name}: {value};");
        }
        css.push_str("}\n");
        css
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        assert_eq!(parse_hex("#FF4D8D").unwrap().hex(), "#ff4d8d");
        assert_eq!(parse_hex("#0af").unwrap().hex(), "#00aaff");
        assert!(parse_hex("ff4d8d").is_none());
        assert!(parse_hex("#ff4d8").is_none());
        assert!(parse_hex("#gggggg").is_none());
    }

    #[test]
    fn oklch_round_trips() {
        for hex in ["#ff4d8d", "#0ea5e9", "#10b981", "#777777", "#000000", "#ffffff", "#f59e0b"] {
            let color = parse_hex(hex).unwrap();
            assert_eq!(rgb(oklch(color)).hex(), hex, "{hex}");
        }
    }

    fn token(tokens: &Tokens, name: &str) -> Rgb {
        let value = &tokens.iter().find(|(key, _)| *key == name).unwrap().1;
        parse_hex(value).unwrap()
    }

    #[test]
    fn every_accent_gives_readable_buttons_and_links() {
        // Light, dark, saturated and grey accents alike.
        for hex in ["#ff4d8d", "#ffee00", "#00ff00", "#0000ff", "#1c1420", "#f8f4f6", "#808080", "#0ea5e9"] {
            let p = palette(parse_hex(hex).unwrap());
            let light_solid = token(&p.light, "--uwu-pink-solid");
            assert!(contrast(light_solid, WHITE) >= 4.5, "{hex}: light button");
            let light_ink = token(&p.light, "--uwu-pink-ink");
            assert!(contrast(light_ink, WHITE) >= 4.5, "{hex}: light link");
            let dark_solid = token(&p.dark, "--uwu-pink-solid");
            assert!(contrast(dark_solid, INK) >= 4.5, "{hex}: dark button");
            let dark_ink = token(&p.dark, "--uwu-pink-ink");
            assert!(contrast(dark_ink, DARK_SURFACE) >= 4.5, "{hex}: dark link");
        }
    }

    #[test]
    fn css_covers_both_themes() {
        let css = palette(parse_hex("#0ea5e9").unwrap()).css();
        assert!(css.starts_with("html:root {"));
        assert!(css.contains("html:root[data-theme=\"dark\"]"));
        assert!(css.contains("--uwu-pink: #0ea5e9;"));
        assert!(css.contains("--uwu-toast-accent: #"));
    }
}
