//! Part D tokens (D2 colours, D4 type, D5 radii, D7 states, D8 controls) as iced styles. The values are the
//! desktop's (`src/theme.rs`, `src/render/mod.rs`); the window is opaque, so materials are their solid tints.

use iced::{
    Background, Border, Color, Shadow, Vector,
    overlay::menu,
    widget::{button, container, pick_list, scrollable, slider, svg, text_input, toggler},
};

/// D2's accent set: light and dark.
pub const ACCENTS: [(&str, u32, u32); 8] = [
    ("blue", 0x007AFF, 0x0A84FF),
    ("purple", 0xAF52DE, 0xBF5AF2),
    ("pink", 0xFF2D55, 0xFF375F),
    ("red", 0xFF3B30, 0xFF453A),
    ("orange", 0xFF9500, 0xFF9F0A),
    ("yellow", 0xFFCC00, 0xFFD60A),
    ("green", 0x34C759, 0x30D158),
    ("graphite", 0x8E8E93, 0x98989D),
];

pub fn rgb(hex: u32) -> Color {
    Color::from_rgb8((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

#[derive(Clone, Copy, Debug)]
pub struct Tokens {
    pub dark: bool,
    pub accent: Color,
}

impl Tokens {
    pub fn new(dark: bool, accent: &str) -> Tokens {
        let (_, light, dark_hex) = ACCENTS.iter().find(|(name, ..)| *name == accent).copied().unwrap_or(ACCENTS[0]);
        Tokens { dark, accent: rgb(if dark { dark_hex } else { light }) }
    }

    fn ink(self, light: f32, dark: f32) -> Color {
        if self.dark { Color::from_rgba(1.0, 1.0, 1.0, dark) } else { Color::from_rgba(0.0, 0.0, 0.0, light) }
    }

    pub fn label(self) -> Color {
        self.ink(0.85, 0.85)
    }
    pub fn secondary(self) -> Color {
        self.ink(0.5, 0.55)
    }
    pub fn tertiary(self) -> Color {
        self.ink(0.26, 0.25)
    }
    pub fn separator(self) -> Color {
        self.ink(0.1, 0.1)
    }
    pub fn fill(self) -> Color {
        self.ink(0.05, 0.08)
    }
    /// D7 hover overlay.
    pub fn hover(self) -> Color {
        self.ink(0.06, 0.08)
    }
    pub fn window(self) -> Color {
        rgb(if self.dark { 0x1E1E1E } else { 0xECECEC })
    }
    /// `panel` material, solid: the sidebar.
    pub fn panel(self) -> Color {
        rgb(if self.dark { 0x262626 } else { 0xF2F2F2 })
    }
    /// `card` material, solid: grouped rows.
    pub fn card(self) -> Color {
        rgb(if self.dark { 0x323232 } else { 0xFFFFFF })
    }
    fn hairline(self) -> Color {
        Color::from_rgba(0.0, 0.0, 0.0, if self.dark { 0.5 } else { 0.1 })
    }

    pub fn theme(self) -> iced::Theme {
        let palette = iced::theme::Palette {
            background: self.window(),
            text: self.label(),
            primary: self.accent,
            success: rgb(0x34C759),
            warning: rgb(0xFF9500),
            danger: rgb(0xFF3B30),
        };
        iced::Theme::custom(if self.dark { "Noobsbot Dark" } else { "Noobsbot Light" }, palette)
    }

    pub fn sidebar(self) -> impl Fn(&iced::Theme) -> container::Style {
        move |_| container::Style {
            background: Some(self.panel().into()),
            border: Border { color: self.hairline(), width: 0.5, radius: 10.0.into() },
            ..Default::default()
        }
    }

    /// A grouped card (E7.1: radius 10) with a hairline.
    pub fn group(self) -> impl Fn(&iced::Theme) -> container::Style {
        move |_| container::Style {
            background: Some(self.card().into()),
            border: Border { color: self.separator(), width: 0.5, radius: 10.0.into() },
            ..Default::default()
        }
    }

    pub fn rule(self) -> impl Fn(&iced::Theme) -> container::Style {
        move |_| container::Style { background: Some(self.separator().into()), ..Default::default() }
    }

    /// A 20 px icon squircle in `color`.
    pub fn squircle(color: Color, radius: f32) -> impl Fn(&iced::Theme) -> container::Style {
        move |_| container::Style { background: Some(color.into()), border: Border { radius: radius.into(), ..Default::default() }, ..Default::default() }
    }

    pub fn field(self) -> impl Fn(&iced::Theme) -> container::Style {
        move |_| container::Style { background: Some(self.fill().into()), border: Border { radius: 6.0.into(), ..Default::default() }, ..Default::default() }
    }

    pub fn tint(color: Color) -> impl Fn(&iced::Theme, svg::Status) -> svg::Style {
        move |_, _| svg::Style { color: Some(color) }
    }

    /// D7 sidebar row: selected = accent @ 20%, hover = the overlay.
    pub fn sidebar_row(self, selected: bool) -> impl Fn(&iced::Theme, button::Status) -> button::Style {
        move |_, status| {
            let background = match status {
                _ if selected => Some(Color { a: 0.2, ..self.accent }),
                button::Status::Hovered => Some(self.hover()),
                button::Status::Pressed => Some(self.ink(0.12, 0.14)),
                _ => None,
            };
            button::Style {
                background: background.map(Background::from),
                text_color: self.label(),
                border: Border { radius: 6.0.into(), ..Default::default() },
                ..Default::default()
            }
        }
    }

    /// D8 segmented control segment: selected = card material + level-2 shadow.
    pub fn segment(self, selected: bool) -> impl Fn(&iced::Theme, button::Status) -> button::Style {
        move |_, status| {
            let lift = if self.dark { rgb(0x5A5A5A) } else { rgb(0xFFFFFF) };
            let background = match status {
                _ if selected => Some(lift),
                button::Status::Hovered => Some(self.hover()),
                _ => None,
            };
            let shadow = if selected {
                Shadow { color: Color::from_rgba(0.0, 0.0, 0.0, if self.dark { 0.4 } else { 0.15 }), offset: Vector::new(0.0, 1.0), blur_radius: 3.0 }
            } else {
                Shadow::default()
            };
            button::Style {
                background: background.map(Background::from),
                text_color: self.label(),
                border: Border { radius: 5.0.into(), ..Default::default() },
                shadow,
                snap: true,
            }
        }
    }

    /// D8 push button: the default one accent-filled, the others a raised segment.
    pub fn push(self, default: bool) -> impl Fn(&iced::Theme, button::Status) -> button::Style {
        move |theme, status| {
            if !default {
                let mut style = self.segment(true)(theme, status);
                style.border = Border { color: self.separator(), width: 0.5, radius: 6.0.into() };
                if status == button::Status::Pressed {
                    style.background = Some(self.ink(0.12, 0.3).into());
                }
                return style;
            }
            let pressed = status == button::Status::Pressed;
            button::Style {
                background: Some(Color { a: if pressed { 0.8 } else { 1.0 }, ..self.accent }.into()),
                text_color: Color::WHITE,
                border: Border { radius: 6.0.into(), ..Default::default() },
                shadow: Shadow { color: Color::from_rgba(0.0, 0.0, 0.0, 0.15), offset: Vector::new(0.0, 1.0), blur_radius: 2.0 },
                snap: true,
            }
        }
    }

    /// A dropdown (D8 pop-up button): a raised field with the chevron in `label-secondary`.
    pub fn pick(self) -> impl Fn(&iced::Theme, pick_list::Status) -> pick_list::Style {
        move |_, status| {
            let lift = if self.dark { rgb(0x4A4A4A) } else { rgb(0xFFFFFF) };
            let opened = matches!(status, pick_list::Status::Opened { .. });
            pick_list::Style {
                text_color: self.label(),
                placeholder_color: self.tertiary(),
                handle_color: self.secondary(),
                background: lift.into(),
                border: Border {
                    color: if opened { Color { a: 0.5, ..self.accent } } else { self.separator() },
                    width: if opened { 2.0 } else { 0.5 },
                    radius: 6.0.into(),
                },
            }
        }
    }

    /// A dropdown's menu: `card` material, the hovered row in the accent.
    pub fn menu(self) -> impl Fn(&iced::Theme) -> menu::Style {
        move |_| menu::Style {
            background: self.card().into(),
            border: Border { color: self.separator(), width: 0.5, radius: 6.0.into() },
            text_color: self.label(),
            selected_text_color: Color::WHITE,
            selected_background: self.accent.into(),
            shadow: Shadow { color: Color::from_rgba(0.0, 0.0, 0.0, 0.2), offset: Vector::new(0.0, 4.0), blur_radius: 12.0 },
        }
    }

    /// A sheet over the window (D8 alert): `card` material, radius 12, a level-3 shadow.
    pub fn sheet(self) -> impl Fn(&iced::Theme) -> container::Style {
        move |_| container::Style {
            background: Some(self.card().into()),
            border: Border { color: self.separator(), width: 0.5, radius: 12.0.into() },
            shadow: Shadow { color: Color::from_rgba(0.0, 0.0, 0.0, 0.3), offset: Vector::new(0.0, 8.0), blur_radius: 24.0 },
            ..Default::default()
        }
    }

    /// The dimming behind a sheet.
    pub fn dim(self) -> impl Fn(&iced::Theme) -> container::Style {
        move |_| container::Style { background: Some(Color::from_rgba(0.0, 0.0, 0.0, if self.dark { 0.4 } else { 0.2 }).into()), ..Default::default() }
    }

    /// An accent swatch: a ring in the swatch's colour when it is chosen.
    pub fn swatch(self, color: Color, selected: bool) -> impl Fn(&iced::Theme, button::Status) -> button::Style {
        move |_, status| {
            let ring = if selected {
                2.0
            } else if status == button::Status::Hovered {
                1.0
            } else {
                0.0
            };
            let edge = if selected { self.card() } else { Color { a: 0.0, ..color } };
            button::Style { background: Some(color.into()), border: Border { color: edge, width: ring, radius: 999.0.into() }, ..Default::default() }
        }
    }

    /// D7 toggle: track accent / `fill` × 2, white knob.
    pub fn toggle(self) -> impl Fn(&iced::Theme, toggler::Status) -> toggler::Style {
        move |_, status| {
            let on = matches!(
                status,
                toggler::Status::Active { is_toggled: true } | toggler::Status::Hovered { is_toggled: true } | toggler::Status::Disabled { is_toggled: true }
            );
            let track = if on { self.accent } else { self.ink(0.1, 0.16) };
            let disabled = matches!(status, toggler::Status::Disabled { .. });
            let alpha = if disabled { 0.4 } else { 1.0 };
            toggler::Style {
                background: Color { a: track.a * alpha, ..track }.into(),
                background_border_width: 0.0,
                background_border_color: Color::TRANSPARENT,
                foreground: Color { a: alpha, ..Color::WHITE }.into(),
                foreground_border_width: 0.5,
                foreground_border_color: Color::from_rgba(0.0, 0.0, 0.0, 0.12),
                text_color: None,
                border_radius: None,
                padding_ratio: 0.1,
            }
        }
    }

    /// A thin rail with the accent left of a white knob, as macOS Settings draws it.
    pub fn slider(self) -> impl Fn(&iced::Theme, slider::Status) -> slider::Style {
        move |_, _| slider::Style {
            rail: slider::Rail {
                backgrounds: (self.accent.into(), self.ink(0.1, 0.16).into()),
                width: 4.0,
                border: Border { radius: 2.0.into(), ..Default::default() },
            },
            handle: slider::Handle {
                shape: slider::HandleShape::Circle { radius: 8.0 },
                background: Color::WHITE.into(),
                border_width: 0.5,
                border_color: Color::from_rgba(0.0, 0.0, 0.0, 0.25),
            },
        }
    }

    /// A text field (D8): `fill` background, radius 6, accent focus ring.
    pub fn input(self, bare: bool) -> impl Fn(&iced::Theme, text_input::Status) -> text_input::Style {
        move |_, status| {
            let focused = matches!(status, text_input::Status::Focused { .. });
            let border = match () {
                _ if bare => Border::default(),
                _ if focused => Border { color: Color { a: 0.5, ..self.accent }, width: 2.0, radius: 6.0.into() },
                _ => Border { color: self.separator(), width: 0.5, radius: 6.0.into() },
            };
            text_input::Style {
                background: if bare { Color::TRANSPARENT.into() } else { self.fill().into() },
                border,
                icon: self.secondary(),
                placeholder: self.tertiary(),
                value: self.label(),
                selection: Color { a: 0.3, ..self.accent },
            }
        }
    }

    pub fn scroll(self) -> impl Fn(&iced::Theme, scrollable::Status) -> scrollable::Style {
        move |theme, status| {
            let mut style = scrollable::default(theme, status);
            for rail in [&mut style.vertical_rail, &mut style.horizontal_rail] {
                rail.background = None;
                rail.border = Border::default();
                rail.scroller.background = self.tertiary().into();
                rail.scroller.border = Border { radius: 3.0.into(), ..Default::default() };
            }
            style
        }
    }
}

/// A 6 px scrollbar floating 2 px from the edge.
pub fn thin() -> scrollable::Direction {
    scrollable::Direction::Vertical(scrollable::Scrollbar::new().width(6).scroller_width(6).margin(2))
}
