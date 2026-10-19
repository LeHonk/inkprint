//! Compile-time description of each supported Inkplate variant.
//!
//! Everything that differs between boards (panel size, pixel depth, which driver family,
//! what to advertise over IPP) is a const on [`Board`]. The active board is chosen with a
//! Cargo feature and exported as [`Selected`].

#![no_std]

/// How the panel is driven.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverFamily {
    /// Bit-banged 8-bit parallel bus with waveform tables, TPS65186 PMIC (6, 6PLUS, 10, 5).
    ParallelEpd,
    /// Panel with its own controller on SPI (6COLOR, ACeP 7-color).
    SpiColor,
}

/// Pixel depth the panel can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelKind {
    /// 1 bit per pixel, black/white.
    Mono1,
    /// 3 bits per pixel, 8 gray levels.
    Gray3,
    /// 7-color ACeP palette.
    Color7,
}

/// Rotation applied from the logical (portrait) page to the native panel orientation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rotation {
    None,
    Cw90,
    Cw180,
    Cw270,
}

/// IO expander used for panel control lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoExpander {
    Mcp23017,
    Pcal6416a,
    None,
}

/// A media size advertised over IPP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Media {
    /// PWG 5101.1 self-describing name, e.g. `iso_a6_105x148mm`.
    pub keyword: &'static str,
    /// Size in hundredths of a millimetre, as used by `media-size` in `media-col`.
    pub width_hmm: i32,
    pub height_hmm: i32,
}

pub const ISO_A6: Media = Media { keyword: "iso_a6_105x148mm", width_hmm: 10500, height_hmm: 14800 };
pub const NA_INDEX_4X6: Media = Media { keyword: "na_index-4x6_4x6in", width_hmm: 10160, height_hmm: 15240 };
pub const ISO_A5: Media = Media { keyword: "iso_a5_148x210mm", width_hmm: 14800, height_hmm: 21000 };
pub const ISO_A4: Media = Media { keyword: "iso_a4_210x297mm", width_hmm: 21000, height_hmm: 29700 };
pub const NA_LETTER: Media = Media { keyword: "na_letter_8.5x11in", width_hmm: 21590, height_hmm: 27940 };

/// Small sizes first: the first entry is `media-default`, which makes print dialogs lay
/// content out large enough to read on a small panel. 4×6 comes second so it is reported
/// as ready too (see [`Board::READY_MEDIA`]).
pub const DEFAULT_MEDIA: &[Media] = &[ISO_A6, NA_INDEX_4X6, ISO_A5, ISO_A4, NA_LETTER];

pub trait Board {
    /// Human-readable model, used for `printer-make-and-model` and the mDNS `ty` key.
    const MODEL: &'static str;
    /// Native panel size in pixels (as the driver addresses it).
    const WIDTH: u16;
    const HEIGHT: u16;
    /// Rotation from portrait page to native panel orientation.
    const ROTATION: Rotation;
    const PIXEL: PixelKind;
    const DRIVER: DriverFamily;
    const IO_EXPANDER: IoExpander;
    /// Raster resolutions (dpi) to advertise. Clients render at one of these.
    const RESOLUTIONS: &'static [u16];
    const MEDIA: &'static [Media];
    /// How many leading [`Board::MEDIA`] entries to report as loaded (`media-ready`,
    /// `media-col-ready`). Android's built-in print service takes its default from the
    /// ready sizes it knows and has no A6, so the ready list must include a size it does
    /// know (4×6, A5, …) or it falls back to its own default (A4/Letter).
    const READY_MEDIA: usize = 2;

    /// Panel size after rotation, i.e. the portrait canvas pages are rendered into.
    fn logical_size() -> (u16, u16) {
        match Self::ROTATION {
            Rotation::None | Rotation::Cw180 => (Self::WIDTH, Self::HEIGHT),
            Rotation::Cw90 | Rotation::Cw270 => (Self::HEIGHT, Self::WIDTH),
        }
    }

    fn is_color() -> bool {
        Self::PIXEL == PixelKind::Color7
    }
}

/// Inkplate 6 (original, MCP23017 IO expander). ED060SC4, 800×600.
pub struct Inkplate6;
impl Board for Inkplate6 {
    const MODEL: &'static str = "Inkplate 6";
    const WIDTH: u16 = 800;
    const HEIGHT: u16 = 600;
    const ROTATION: Rotation = Rotation::Cw90;
    const PIXEL: PixelKind = PixelKind::Gray3;
    const DRIVER: DriverFamily = DriverFamily::ParallelEpd;
    const IO_EXPANDER: IoExpander = IoExpander::Mcp23017;
    const RESOLUTIONS: &'static [u16] = &[150, 300];
    const MEDIA: &'static [Media] = DEFAULT_MEDIA;
}

/// Inkplate 6 V2 (PCAL6416A IO expander). Same panel as [`Inkplate6`].
pub struct Inkplate6V2;
impl Board for Inkplate6V2 {
    const MODEL: &'static str = "Inkplate 6 V2";
    const WIDTH: u16 = 800;
    const HEIGHT: u16 = 600;
    const ROTATION: Rotation = Rotation::Cw90;
    const PIXEL: PixelKind = PixelKind::Gray3;
    const DRIVER: DriverFamily = DriverFamily::ParallelEpd;
    const IO_EXPANDER: IoExpander = IoExpander::Pcal6416a;
    const RESOLUTIONS: &'static [u16] = &[150, 300];
    const MEDIA: &'static [Media] = DEFAULT_MEDIA;
}

// TODO: verify IO expander and panel details for the boards below against
// Soldered's Inkplate-Arduino-library before enabling them.

/// Inkplate 6PLUS, 1024×758.
pub struct Inkplate6Plus;
impl Board for Inkplate6Plus {
    const MODEL: &'static str = "Inkplate 6PLUS";
    const WIDTH: u16 = 1024;
    const HEIGHT: u16 = 758;
    const ROTATION: Rotation = Rotation::Cw90;
    const PIXEL: PixelKind = PixelKind::Gray3;
    const DRIVER: DriverFamily = DriverFamily::ParallelEpd;
    const IO_EXPANDER: IoExpander = IoExpander::Mcp23017;
    const RESOLUTIONS: &'static [u16] = &[150, 300];
    const MEDIA: &'static [Media] = DEFAULT_MEDIA;
}

/// Inkplate 10, 1200×825.
pub struct Inkplate10;
impl Board for Inkplate10 {
    const MODEL: &'static str = "Inkplate 10";
    const WIDTH: u16 = 1200;
    const HEIGHT: u16 = 825;
    const ROTATION: Rotation = Rotation::Cw90;
    const PIXEL: PixelKind = PixelKind::Gray3;
    const DRIVER: DriverFamily = DriverFamily::ParallelEpd;
    const IO_EXPANDER: IoExpander = IoExpander::Mcp23017;
    const RESOLUTIONS: &'static [u16] = &[150, 300];
    const MEDIA: &'static [Media] = &[ISO_A5, ISO_A6, ISO_A4, NA_LETTER, NA_INDEX_4X6];
}

/// Inkplate 5, 960×540.
pub struct Inkplate5;
impl Board for Inkplate5 {
    const MODEL: &'static str = "Inkplate 5";
    const WIDTH: u16 = 960;
    const HEIGHT: u16 = 540;
    const ROTATION: Rotation = Rotation::Cw90;
    const PIXEL: PixelKind = PixelKind::Gray3;
    const DRIVER: DriverFamily = DriverFamily::ParallelEpd;
    const IO_EXPANDER: IoExpander = IoExpander::Pcal6416a;
    const RESOLUTIONS: &'static [u16] = &[150, 300];
    const MEDIA: &'static [Media] = DEFAULT_MEDIA;
}

/// Inkplate 6COLOR, 7-color ACeP, 600×448.
pub struct Inkplate6Color;
impl Board for Inkplate6Color {
    const MODEL: &'static str = "Inkplate 6COLOR";
    const WIDTH: u16 = 600;
    const HEIGHT: u16 = 448;
    const ROTATION: Rotation = Rotation::Cw90;
    const PIXEL: PixelKind = PixelKind::Color7;
    const DRIVER: DriverFamily = DriverFamily::SpiColor;
    const IO_EXPANDER: IoExpander = IoExpander::None;
    const RESOLUTIONS: &'static [u16] = &[150, 300];
    const MEDIA: &'static [Media] = DEFAULT_MEDIA;
}

macro_rules! select_board {
    ($($feature:literal => $ty:ty),+ $(,)?) => {
        // Libraries stay generic over `B: Board`; only binaries name `Selected`, so a
        // build without any board feature fails there with "cannot find type `Selected`".
        $(#[cfg(feature = $feature)] pub type Selected = $ty;)+

        const _: () = {
            let enabled = 0 $(+ cfg!(feature = $feature) as u32)+;
            assert!(enabled <= 1, "enable exactly one board feature");
        };
    };
}

select_board! {
    "inkplate6" => Inkplate6,
    "inkplate6v2" => Inkplate6V2,
    "inkplate6plus" => Inkplate6Plus,
    "inkplate10" => Inkplate10,
    "inkplate5" => Inkplate5,
    "inkplate6color" => Inkplate6Color,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inkplate6_is_portrait_after_rotation() {
        assert_eq!(Inkplate6::logical_size(), (600, 800));
    }
}
