// Ayuz - Unofficial Control Center for Asus Laptops
// Copyright (C) 2026 Guido Philipp
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see https://www.gnu.org/licenses/.

//! Static, proportional NumberPad grid layouts.
//!
//! Each [`Layout`] describes a `rows x cols` grid of touch cells, row-major.
//! The numberpad service slices the touchpad's `ABS_MT_POSITION_X/Y` range
//! into equal-sized rectangles and maps the cell under a tap to a Linux
//! key code (`KEY_KP0`..`KEY_KPENTER`, etc.) emitted via uinput.
//!
//! This is deliberately model-agnostic. The upstream Python driver ships
//! ~30 layout files with hardcoded pixel offsets per laptop; we instead
//! provide a single universal 4x4 grid and a DMI-matched override table
//! for laptops where the universal layout demonstrably misbehaves. Adding
//! a new override is purely additive - no architectural change required.

use evdev::KeyCode;

/// A single cell in the proportional grid. `keys` is a press-and-release
/// sequence — single-key cells use a one-element slice, while macro cells
/// (e.g. `Shift + 5` for `%`) chain multiple codes that the emitter fires
/// in order forward and releases in reverse.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    pub keys: &'static [KeyCode],
}

/// A row-major proportional layout. `cells.len()` must equal `rows * cols`.
/// `None` entries are inert (no key emitted on tap).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub rows: u8,
    pub cols: u8,
    pub cells: &'static [Option<Cell>],
}

macro_rules! k {
    ($code:ident) => {
        Some(Cell {
            keys: &[KeyCode::$code],
        })
    };
}

macro_rules! m {
    ($($code:ident),+) => {
        Some(Cell {
            keys: &[$(KeyCode::$code),+],
        })
    };
}

/// Default 4x4 layout, used when the laptop is unknown or no override
/// applies. Mirrors a standard physical numpad.
pub const UNIVERSAL_4X4: Layout = Layout {
    rows: 4,
    cols: 4,
    #[rustfmt::skip]
    cells: &[
        k!(KEY_KP7), k!(KEY_KP8),  k!(KEY_KP9),      k!(KEY_BACKSPACE),
        k!(KEY_KP4), k!(KEY_KP5),  k!(KEY_KP6),      k!(KEY_KPASTERISK),
        k!(KEY_KP1), k!(KEY_KP2),  k!(KEY_KP3),      k!(KEY_KPMINUS),
        k!(KEY_KP0), k!(KEY_KPDOT), k!(KEY_KPENTER), k!(KEY_KPPLUS),
    ],
};

/// ASUS Zenbook 14 (UX3405 series, e.g. UX3405MA, UX3405CA) 5x4 NumberPad. Five columns: the rightmost
/// adds `/`, `*`, `-`, `+` as a dedicated operator column and a tall
/// `Backspace` zone (stacked over rows 0+1). The `%` cell is emitted as a
/// `Shift + 5` macro because Linux has no `KEY_KPPERCENT` evdev code; this
/// produces `%` under any keyboard layout that follows the standard top-row
/// digit mapping (US, DE, LT, ...).
pub const UX3405MA_5X4: Layout = Layout {
    rows: 4,
    cols: 5,
    #[rustfmt::skip]
    cells: &[
        k!(KEY_KP7), k!(KEY_KP8),   k!(KEY_KP9),     k!(KEY_KPSLASH),    k!(KEY_BACKSPACE),
        k!(KEY_KP4), k!(KEY_KP5),   k!(KEY_KP6),     k!(KEY_KPASTERISK), k!(KEY_BACKSPACE),
        k!(KEY_KP1), k!(KEY_KP2),   k!(KEY_KP3),     k!(KEY_KPMINUS),    m!(KEY_LEFTSHIFT, KEY_5),
        k!(KEY_KP0), k!(KEY_KPDOT), k!(KEY_KPENTER), k!(KEY_KPPLUS),     k!(KEY_KPEQUAL),
    ],
};

/// Override table: substring of `/sys/class/dmi/id/product_name` -> layout.
/// First match wins. Add entries here as users report model-specific quirks.
const LAYOUTS: &[(&str, &Layout)] = &[
    ("UX3405", &UX3405MA_5X4),
    ("UM3402", &UX3405MA_5X4),
    ("B3302", &UX3405MA_5X4),
];

/// Returns the layout for the given DMI product name, falling back to
/// [`UNIVERSAL_4X4`] when no override matches.
pub fn for_product(product_name: &str) -> &'static Layout {
    LAYOUTS
        .iter()
        .find(|(needle, _)| product_name.contains(needle))
        .map(|(_, l)| *l)
        .unwrap_or(&UNIVERSAL_4X4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ux3405ca_dmi_matching() {
        // Verbatim DMI product_name on ASUS Zenbook 14 UX3405CA
        let verbatim_dmi = "ASUS Zenbook 14 UX3405CA_UX3405CA";
        let layout = for_product(verbatim_dmi);
        assert_eq!(layout, &UX3405MA_5X4);
        assert_eq!(layout.rows, 4);
        assert_eq!(layout.cols, 5);
        assert_eq!(layout.cells.len(), 20);

        // Substrings / variations
        assert_eq!(for_product("UX3405CA"), &UX3405MA_5X4);
        assert_eq!(for_product("Zenbook 14 OLED UX3405CA"), &UX3405MA_5X4);
    }

    #[test]
    fn test_ux3405ma_backward_compatibility() {
        let verbatim_dmi = "ASUS Zenbook 14 UX3405MA_UX3405MA";
        let layout = for_product(verbatim_dmi);
        assert_eq!(layout, &UX3405MA_5X4);
        assert_eq!(for_product("UX3405MA"), &UX3405MA_5X4);
    }

    #[test]
    fn test_ux3405_family_variants() {
        // Bare prefix and future variants
        assert_eq!(for_product("UX3405"), &UX3405MA_5X4);
        assert_eq!(for_product("UX3405V"), &UX3405MA_5X4);
        assert_eq!(for_product("UX3405EA"), &UX3405MA_5X4);
        assert_eq!(for_product("ASUS Zenbook UX3405-OLED"), &UX3405MA_5X4);
    }

    #[test]
    fn test_other_overrides_retention() {
        // UM3402 models
        assert_eq!(for_product("UM3402"), &UX3405MA_5X4);
        assert_eq!(for_product("Zenbook UM3402YA_UM3402YA"), &UX3405MA_5X4);
        assert_eq!(for_product("ASUS Zenbook 14 UM3402"), &UX3405MA_5X4);

        // B3302 models
        assert_eq!(for_product("B3302"), &UX3405MA_5X4);
        assert_eq!(for_product("ASUS ExpertBook B3302FEA"), &UX3405MA_5X4);
        assert_eq!(for_product("B3302F"), &UX3405MA_5X4);
    }

    #[test]
    fn test_universal_4x4_fallback() {
        // Other ASUS laptops
        let fallback_zenbook = for_product("Zenbook UX430UA");
        assert_eq!(fallback_zenbook, &UNIVERSAL_4X4);
        assert_eq!(fallback_zenbook.rows, 4);
        assert_eq!(fallback_zenbook.cols, 4);
        assert_eq!(fallback_zenbook.cells.len(), 16);

        assert_eq!(for_product("ROG Zephyrus G14"), &UNIVERSAL_4X4);
        assert_eq!(for_product("TUF Gaming A15"), &UNIVERSAL_4X4);

        // Non-ASUS laptops
        assert_eq!(for_product("ThinkPad X1 Carbon Gen 10"), &UNIVERSAL_4X4);
        assert_eq!(for_product("Dell XPS 13 9310"), &UNIVERSAL_4X4);
    }

    #[test]
    fn test_near_misses_do_not_match_ux3405() {
        assert_eq!(for_product("UX340"), &UNIVERSAL_4X4);
        assert_eq!(for_product("UX3404"), &UNIVERSAL_4X4);
        assert_eq!(for_product("UX3406"), &UNIVERSAL_4X4);
        assert_eq!(for_product("UX3450"), &UNIVERSAL_4X4);
        assert_eq!(for_product("UX5405"), &UNIVERSAL_4X4);
        assert_eq!(for_product("3405"), &UNIVERSAL_4X4);
        assert_eq!(for_product("UM340"), &UNIVERSAL_4X4);
        assert_eq!(for_product("B330"), &UNIVERSAL_4X4);
    }

    #[test]
    fn test_edge_cases_and_empty_inputs() {
        // Empty string
        assert_eq!(for_product(""), &UNIVERSAL_4X4);
        // Whitespace only
        assert_eq!(for_product("   "), &UNIVERSAL_4X4);
        assert_eq!(for_product("\t\n"), &UNIVERSAL_4X4);
        // Case sensitivity (DMI is uppercase; lowercase should not match)
        assert_eq!(for_product("ux3405"), &UNIVERSAL_4X4);
        assert_eq!(for_product("ux3405ca"), &UNIVERSAL_4X4);
        assert_eq!(for_product("um3402"), &UNIVERSAL_4X4);
        // Punctuation and symbols
        assert_eq!(for_product("!@#$%^&*()"), &UNIVERSAL_4X4);
        // Embedded substring matches
        assert_eq!(for_product("MODEL_UX3405_EXTRA"), &UX3405MA_5X4);
    }

    #[test]
    fn test_layout_grid_invariants() {
        // Dimensions and length invariant
        assert_eq!(
            UX3405MA_5X4.cells.len(),
            (UX3405MA_5X4.rows as usize) * (UX3405MA_5X4.cols as usize)
        );
        assert_eq!(
            UNIVERSAL_4X4.cells.len(),
            (UNIVERSAL_4X4.rows as usize) * (UNIVERSAL_4X4.cols as usize)
        );

        // All cells are Some
        assert!(UX3405MA_5X4.cells.iter().all(|c| c.is_some()));
        assert!(UNIVERSAL_4X4.cells.iter().all(|c| c.is_some()));
    }

    #[test]
    fn test_ux3405_macro_and_stacked_backspace_keys() {
        // Row 0 Col 4 (index 4) and Row 1 Col 4 (index 9) are stacked Backspace
        let cell_r0_c4 = UX3405MA_5X4.cells[4].unwrap();
        let cell_r1_c4 = UX3405MA_5X4.cells[9].unwrap();
        assert_eq!(cell_r0_c4.keys, &[KeyCode::KEY_BACKSPACE]);
        assert_eq!(cell_r1_c4.keys, &[KeyCode::KEY_BACKSPACE]);

        // Row 2 Col 4 (index 14) is '%' macro: Shift + 5
        let percent_cell = UX3405MA_5X4.cells[14].unwrap();
        assert_eq!(percent_cell.keys, &[KeyCode::KEY_LEFTSHIFT, KeyCode::KEY_5]);

        // Dedicated operators in Row 0..3 Col 3/4
        assert_eq!(UX3405MA_5X4.cells[3].unwrap().keys, &[KeyCode::KEY_KPSLASH]);
        assert_eq!(UX3405MA_5X4.cells[8].unwrap().keys, &[KeyCode::KEY_KPASTERISK]);
        assert_eq!(UX3405MA_5X4.cells[13].unwrap().keys, &[KeyCode::KEY_KPMINUS]);
        assert_eq!(UX3405MA_5X4.cells[18].unwrap().keys, &[KeyCode::KEY_KPPLUS]);
        assert_eq!(UX3405MA_5X4.cells[19].unwrap().keys, &[KeyCode::KEY_KPEQUAL]);
    }

    #[test]
    fn test_layouts_table_validity() {
        for &(needle, layout) in LAYOUTS {
            assert!(!needle.is_empty(), "needle must not be empty");
            assert!(
                needle.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()),
                "needle should be uppercase ASCII alphanumeric: {}",
                needle
            );
            assert_eq!(
                layout.cells.len(),
                (layout.rows as usize) * (layout.cols as usize)
            );
        }
    }

    #[test]
    fn test_distinct_layouts() {
        assert_ne!(&UNIVERSAL_4X4, &UX3405MA_5X4);
        assert_ne!(UNIVERSAL_4X4.cols, UX3405MA_5X4.cols);
    }
}
