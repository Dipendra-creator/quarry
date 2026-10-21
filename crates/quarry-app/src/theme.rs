//! Colors of the original app's light and dark themes, plus the mapping onto
//! the component library's theme.

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::{App, Global, Hsla, Pixels, Window, px, rgb};

fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

#[derive(Clone)]
pub struct Palette {
    pub dark: bool,
    pub window: Hsla,
    pub window_secondary: Hsla,
    pub sidebar: Hsla,
    pub title_bar: Hsla,
    pub separator: Hsla,
    pub border: Hsla,
    pub text: Hsla,
    pub data_text: Hsla,
    pub secondary_text: Hsla,
    pub icon: Hsla,
    pub capsule_hover: Hsla,
    pub button: Hsla,
    pub button_border: Hsla,
    pub button_hover: Hsla,
    pub segment_selected: Hsla,
    pub selection: Hsla,
    pub selection_text: Hsla,
    pub hover: Hsla,
    pub grid_header: Hsla,
    pub grid_header_border: Hsla,
    pub grid_row: Hsla,
    pub grid_alt_row: Hsla,
    pub added: Hsla,
    pub deleted: Hsla,
    pub modified: Hsla,
    pub primary_key: Hsla,
    pub null_value: Hsla,
    pub tab: Hsla,
    pub tab_selected: Hsla,
    pub workspace_tab_selected: Hsla,
    pub accent: Hsla,
    pub danger: Hsla,
    pub success: Hsla,
    /// Title-bar status background, index = connection status color.
    pub status: [Hsla; 6],
    /// Title-bar status text, index = connection status color.
    pub status_text: [Hsla; 6],
}

impl Palette {
    pub fn light() -> Self {
        Self {
            dark: false,
            window: c(0xFFFFFF),
            window_secondary: c(0xF6F6F6),
            sidebar: c(0xF6F6F6),
            title_bar: c(0xFFFFFF),
            separator: c(0xD9D9D9),
            border: c(0xE5E5E5),
            text: c(0x000000),
            data_text: c(0x000000),
            secondary_text: c(0x969696),
            icon: c(0x363636),
            capsule_hover: c(0xF1F1F1),
            button: c(0xFFFFFF),
            button_border: c(0xE5E5E5),
            button_hover: c(0xD9E6F5),
            segment_selected: c(0xE0E0E0),
            selection: c(0x2465DA),
            selection_text: c(0xFFFFFF),
            hover: c(0xDAEFFF),
            grid_header: c(0xFFFFFF),
            grid_header_border: c(0xEFEFEF),
            grid_row: c(0xFFFFFF),
            grid_alt_row: c(0xF6F6F6),
            added: c(0xDAEBC2),
            deleted: c(0xF5A6B0),
            modified: c(0xFFE18E),
            primary_key: c(0xFF8F00),
            null_value: c(0x529BD8),
            tab: c(0xF0F0F0),
            tab_selected: c(0xFFFFFF),
            workspace_tab_selected: c(0xF6F6F6),
            accent: c(0x2465DA),
            danger: c(0xEB4E3D),
            success: c(0x63CA56),
            status: [c(0xFFFFFF), c(0xDAEBC2), c(0xF8F8F8), c(0xBDCCF4), c(0xF8F7BD), c(0xFFD7D4)],
            status_text: [c(0x252525), c(0x007F3D), c(0x686B6F), c(0x005392), c(0xA3793E), c(0x6D0000)],
        }
    }

    pub fn dark() -> Self {
        Self {
            dark: true,
            window: c(0x1E1E1E),
            window_secondary: c(0x252525),
            sidebar: c(0x252525),
            title_bar: c(0x252525),
            separator: c(0x111111),
            border: c(0x3C3C3C),
            text: c(0xFFFFFF),
            data_text: c(0xD5D5D5),
            secondary_text: c(0xA9A9A9),
            icon: c(0xE0E0E0),
            capsule_hover: c(0x3D3D3D),
            button: c(0x333333),
            button_border: c(0x3C3C3C),
            button_hover: c(0x243140),
            segment_selected: c(0x4B4B4B),
            selection: c(0x003B5E),
            selection_text: c(0xFFFFFF),
            hover: c(0x3E3E40),
            grid_header: c(0x292929),
            grid_header_border: c(0x353535),
            grid_row: c(0x1E1E1E),
            grid_alt_row: c(0x292929),
            added: c(0x558C3A),
            deleted: c(0x8F2F31),
            modified: c(0xAD7B15),
            primary_key: c(0xFFB347),
            null_value: c(0x529BD8),
            tab: c(0x333333),
            tab_selected: c(0x2A2A2A),
            workspace_tab_selected: c(0x252525),
            accent: c(0x3B82F7),
            danger: c(0xEB5545),
            success: c(0x6BD45F),
            status: [c(0x252525), c(0x007F3D), c(0x686B6F), c(0x005392), c(0xA3793E), c(0x6D0000)],
            status_text: [c(0xFFFFFF), c(0xDAEBC2), c(0xF8F8F8), c(0xBDCCF4), c(0xF8F7BD), c(0xFFD7D4)],
        }
    }

    /// Color of an environment tag shown next to a connection name.
    pub fn tag(&self, tag: quarry_core::ConnTag) -> Hsla {
        use quarry_core::ConnTag::*;
        match (tag, self.dark) {
            (Production, false) => c(0xEB4E3D),
            (Production, true) => c(0xEB5545),
            (Local, false) => c(0x63CA56),
            (Local, true) => c(0x6BD45F),
            (Staging, false) => c(0x3478F6),
            (Staging, true) => c(0x3B82F7),
            (Development, false) => c(0x5AC4BD),
            (Development, true) => c(0x83D1CE),
            (Testing, false) => c(0xF19A37),
            (Testing, true) => c(0xF2A33C),
            (None, _) => c(0x8E8E93),
        }
    }

    /// Badge color standing in for each vendor logo.
    pub fn driver(&self, driver: quarry_core::DriverKind) -> Hsla {
        use quarry_core::DriverKind::*;
        match driver {
            Postgres => c(0x336791),
            MySql => c(0x00758F),
            MariaDb => c(0x8C6A4F),
            Sqlite => c(0x0F80CC),
        }
    }

}

impl Global for Palette {}

pub trait PaletteExt {
    fn palette(&self) -> &Palette;
}

impl PaletteExt for App {
    fn palette(&self) -> &Palette {
        self.global::<Palette>()
    }
}

pub const MONO: &str = "Consolas";
pub const UI_TEXT: Pixels = px(12.);

/// Switches light/dark and pushes the palette into the component theme.
pub fn apply(dark: bool, window: Option<&mut Window>, cx: &mut App) {
    let p = if dark { Palette::dark() } else { Palette::light() };
    Theme::change(if dark { ThemeMode::Dark } else { ThemeMode::Light }, window, cx);
    Theme::update(cx, |t| {
        t.background = p.window;
        t.foreground = p.text;
        t.border = p.border;
        t.input = p.border;
        t.muted_foreground = p.secondary_text;
        t.title_bar = p.title_bar;
        t.title_bar_border = p.separator;
        t.sidebar = p.sidebar;
        t.sidebar_border = p.separator;
        t.popover = p.window;
        t.popover_foreground = p.text;
        t.primary = p.accent;
        t.primary_hover = p.accent.opacity(0.9);
        t.primary_active = p.accent.opacity(0.8);
        t.primary_foreground = c(0xFFFFFF);
        t.accent = p.hover;
        t.accent_foreground = p.text;
        t.list_active = p.selection;
        t.list_active_border = p.selection;
        t.list_hover = p.hover;
        t.table = p.grid_row;
        t.table_even = p.grid_alt_row;
        t.table_head = p.grid_header;
        t.table_head_foreground = p.text;
        t.table_hover = p.grid_row;
        // Drawn over the cell, so it must let the text show through.
        t.table_active = p.selection.opacity(0.28);
        t.table_active_border = p.selection;
        t.table_row_border = p.grid_header_border;
        t.tab_bar = p.tab;
        t.tab_active = p.tab_selected;
        t.selection = if dark { c(0x314F78) } else { c(0xB3D7FF) };
        t.font_size = px(13.);
        t.mono_font_family = MONO.into();
        t.mono_font_size = px(13.);
        t.radius = px(4.);
        t.radius_lg = px(8.);
        t.shadow = false;
    });
    cx.set_global(p);
    cx.refresh_windows();
}
