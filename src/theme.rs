use ratatui::style::Color;

/// Catppuccin flavor, selected via the `PITWALL_THEME` env var.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    Mocha,
    Macchiato,
    Frappe,
    Latte,
}

impl Flavor {
    /// Parse a `PITWALL_THEME` value. Infallible: case-insensitive match on the
    /// four flavor names; anything else (typo, empty) falls back to `Mocha`.
    /// Named `parse_lenient` rather than `from_str` because it never fails — an
    /// inherent `from_str(&str) -> Self` trips `clippy::should_implement_trait`,
    /// and implementing real `FromStr` would force a pointless `Err` variant.
    pub fn parse_lenient(s: &str) -> Flavor {
        match s.trim().to_ascii_lowercase().as_str() {
            "macchiato" => Flavor::Macchiato,
            "frappe" => Flavor::Frappe,
            "latte" => Flavor::Latte,
            _ => Flavor::Mocha,
        }
    }
}

/// Semantic colors for one flavor. `ui.rs` references these roles, never raw
/// palette names. Colors are truecolor `Color::Rgb`; terminals limited to
/// 16/256 colors downsample to the nearest entry.
pub struct Palette {
    /// Full-screen background.
    pub base: Color,
    /// Default foreground (table cells, header).
    pub text: Color,
    /// Idle rows (rendered with the DIM modifier).
    pub idle: Color,
    /// Busy rows.
    pub busy: Color,
    /// Memory warn tier (approaching the cap): mem cell / gauge below near_cap.
    pub warn: Color,
    /// Near memory-cap rows.
    pub near_cap: Color,
    /// Memory gauge fill.
    pub gauge: Color,
    /// Status banner.
    pub error: Color,
    /// Title accent.
    pub accent: Color,
    /// True for light flavors (Latte). Idle rows skip the DIM modifier on a
    /// light base, where dimming a mid-gray toward near-white kills legibility.
    pub is_light: bool,
}

impl Palette {
    /// Build the palette for a flavor from its official Catppuccin hex values
    /// (see https://catppuccin.com/palette). Role mapping: base=base,
    /// text=text, idle=overlay0, busy=green, warn=yellow, near_cap/error=red,
    /// gauge=teal, accent=mauve.
    pub fn for_flavor(flavor: Flavor) -> Palette {
        match flavor {
            Flavor::Mocha => Palette {
                base: Color::Rgb(30, 30, 46),        // #1e1e2e
                text: Color::Rgb(205, 214, 244),     // #cdd6f4
                idle: Color::Rgb(108, 112, 134),     // #6c7086 overlay0
                busy: Color::Rgb(166, 227, 161),     // #a6e3a1 green
                warn: Color::Rgb(249, 226, 175),     // #f9e2af yellow
                near_cap: Color::Rgb(243, 139, 168), // #f38ba8 red
                gauge: Color::Rgb(148, 226, 213),    // #94e2d5 teal
                error: Color::Rgb(243, 139, 168),    // #f38ba8 red
                accent: Color::Rgb(203, 166, 247),   // #cba6f7 mauve
                is_light: false,
            },
            Flavor::Macchiato => Palette {
                base: Color::Rgb(36, 39, 58),        // #24273a
                text: Color::Rgb(202, 211, 245),     // #cad3f5
                idle: Color::Rgb(110, 115, 141),     // #6e738d overlay0
                busy: Color::Rgb(166, 218, 149),     // #a6da95 green
                warn: Color::Rgb(238, 212, 159),     // #eed49f yellow
                near_cap: Color::Rgb(237, 135, 150), // #ed8796 red
                gauge: Color::Rgb(139, 213, 202),    // #8bd5ca teal
                error: Color::Rgb(237, 135, 150),    // #ed8796 red
                accent: Color::Rgb(198, 160, 246),   // #c6a0f6 mauve
                is_light: false,
            },
            Flavor::Frappe => Palette {
                base: Color::Rgb(48, 52, 70),        // #303446
                text: Color::Rgb(198, 208, 245),     // #c6d0f5
                idle: Color::Rgb(115, 121, 148),     // #737994 overlay0
                busy: Color::Rgb(166, 209, 137),     // #a6d189 green
                warn: Color::Rgb(229, 200, 144),     // #e5c890 yellow
                near_cap: Color::Rgb(231, 130, 132), // #e78284 red
                gauge: Color::Rgb(129, 200, 190),    // #81c8be teal
                error: Color::Rgb(231, 130, 132),    // #e78284 red
                accent: Color::Rgb(202, 158, 230),   // #ca9ee6 mauve
                is_light: false,
            },
            Flavor::Latte => Palette {
                base: Color::Rgb(239, 241, 245),   // #eff1f5
                text: Color::Rgb(76, 79, 105),     // #4c4f69
                idle: Color::Rgb(156, 160, 176),   // #9ca0b0 overlay0
                busy: Color::Rgb(64, 160, 43),     // #40a02b green
                warn: Color::Rgb(223, 142, 29),    // #df8e1d yellow
                near_cap: Color::Rgb(210, 15, 57), // #d20f39 red
                gauge: Color::Rgb(23, 146, 153),   // #179299 teal
                error: Color::Rgb(210, 15, 57),    // #d20f39 red
                accent: Color::Rgb(136, 57, 239),  // #8839ef mauve
                is_light: true,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Pure parser tests — no env access, so they never race the env-mutating
    // config.rs tests under parallel execution.
    #[test]
    fn parse_lenient_matches_known_flavors_case_insensitively() {
        assert!(matches!(Flavor::parse_lenient("mocha"), Flavor::Mocha));
        assert!(matches!(Flavor::parse_lenient("MOCHA"), Flavor::Mocha));
        assert!(matches!(
            Flavor::parse_lenient("Macchiato"),
            Flavor::Macchiato
        ));
        assert!(matches!(Flavor::parse_lenient(" frappe "), Flavor::Frappe));
        assert!(matches!(Flavor::parse_lenient("latte"), Flavor::Latte));
    }

    #[test]
    fn parse_lenient_falls_back_to_mocha() {
        assert!(matches!(Flavor::parse_lenient("garbage"), Flavor::Mocha));
        assert!(matches!(Flavor::parse_lenient(""), Flavor::Mocha));
    }

    #[test]
    fn flavors_have_distinct_base_colors() {
        let bases = [
            Palette::for_flavor(Flavor::Mocha).base,
            Palette::for_flavor(Flavor::Macchiato).base,
            Palette::for_flavor(Flavor::Frappe).base,
            Palette::for_flavor(Flavor::Latte).base,
        ];
        for (i, a) in bases.iter().enumerate() {
            for b in &bases[i + 1..] {
                assert_ne!(a, b, "flavor base colors must be distinct");
            }
        }
    }
}
