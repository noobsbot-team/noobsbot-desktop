//! Noobsbot Desktop System Settings (E7.1): the one separate program. It edits `config.toml`, which the desktop
//! reloads the moment it is saved, and asks the system services (NetworkManager, BlueZ, PipeWire) for the rest.

mod config;
mod pages;
mod style;
mod system;

use std::{collections::HashMap, sync::OnceLock};

use iced::{
    Alignment, Element, Font, Length, Task,
    font::Weight,
    widget::{button, column, container, row, scrollable, space, svg, text, text_input},
};

use pages::Page;
use style::Tokens;

fn main() -> iced::Result {
    let window = iced::window::Settings {
        size: iced::Size::new(720.0, 560.0),
        min_size: Some(iced::Size::new(620.0, 420.0)),
        platform_specific: iced::window::settings::PlatformSpecific { application_id: "noobsbot-desktop-settings".into(), ..Default::default() },
        ..Default::default()
    };
    iced::application(App::new, App::update, App::view)
        .title("System Settings")
        .theme(|app: &App| app.tokens.theme())
        .window(window)
        .default_font(font(Weight::Normal))
        .run()
}

/// Inter (D4) in `weight`, or the system sans-serif when it isn't installed.
pub fn font(weight: Weight) -> Font {
    static FAMILY: OnceLock<Option<&'static str>> = OnceLock::new();
    let family = FAMILY.get_or_init(|| {
        let mut fonts = iced::advanced::graphics::text::font_system().write().ok()?;
        let db = fonts.raw().db();
        ["Inter", "Inter Variable"].into_iter().find(|name| db.faces().any(|face| face.families.iter().any(|(family, _)| family == name)))
    });
    let mut font = family.map_or(Font::DEFAULT, Font::with_name);
    font.weight = weight;
    font
}

pub struct App {
    config: config::Config,
    tokens: Tokens,
    page: Page,
    query: String,
    /// Slider positions while dragging, by key; written on release.
    drafts: HashMap<String, f64>,
    /// Text fields being typed in, by key; written on Enter.
    texts: HashMap<String, String>,
    status: system::Status,
    /// The last save or read failure, shown under the page.
    error: Option<String>,
    user: (String, String),
}

#[derive(Clone, Debug)]
pub enum Message {
    Page(Page),
    Search(String),
    Toggle(&'static str, bool),
    Choose(&'static str, &'static str),
    Slide(String, f64),
    Release(String),
    Type(String, String),
    Submit(String),
    Status(system::Status),
    Wifi(bool),
    Bluetooth(bool),
    /// Output (`true`) or input muted.
    Mute(bool, bool),
}

impl App {
    fn new() -> (App, Task<Message>) {
        let (config, error) = config::Config::load(config::path());
        let page = std::env::args().skip_while(|a| a != "--page").nth(1).and_then(|name| Page::parse(&name)).unwrap_or(Page::Appearance);
        let mut app = App {
            tokens: Tokens::new(false, "blue"),
            config,
            page,
            query: String::new(),
            drafts: HashMap::new(),
            texts: HashMap::new(),
            status: system::Status::default(),
            error,
            user: system::user(),
        };
        app.retheme();
        // Typing searches at once, as in macOS.
        (app, Task::batch([Task::perform(system::read(), Message::Status), iced::widget::operation::focus("search")]))
    }

    fn retheme(&mut self) {
        // ponytail: `auto` is light, as in the desktop, until the Auto schedule exists.
        self.tokens = Tokens::new(self.config.str("appearance.mode") == "dark", self.config.str("appearance.accent"));
    }

    fn save(&mut self, key: &str, value: impl Into<toml_edit::Value>) {
        self.error = self.config.set(key, value).err().map(|err| format!("Couldn't save {key}: {err}"));
        self.retheme();
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Page(page) => {
                self.page = page;
                self.query.clear();
                if page.is_system() {
                    return Task::perform(system::read(), Message::Status);
                }
            }
            Message::Search(query) => self.query = query,
            Message::Toggle(key, on) => self.save(key, on),
            Message::Choose(key, value) => self.save(key, value),
            Message::Slide(key, value) => {
                self.drafts.insert(key, value);
            }
            Message::Release(key) => {
                let Some(value) = self.drafts.remove(&key) else { return Task::none() };
                if let Some(output) = key.strip_prefix("sound.").map(|k| k == "output") {
                    return Task::perform(system::set_volume(output, value.round() as u8), Message::Status);
                }
                match pages::setting(&key).map(|s| s.kind) {
                    Some(pages::Kind::Slider { step, .. }) if step >= 1.0 => self.save(&key, pages::whole(&key, value)),
                    _ => self.save(&key, (value * 100.0).round() / 100.0),
                }
            }
            Message::Type(key, value) => {
                self.texts.insert(key, value);
            }
            Message::Submit(key) => {
                if let Some(value) = self.texts.remove(&key) {
                    self.save(&key, value.trim());
                }
            }
            Message::Status(status) => self.status = status,
            Message::Wifi(on) => return Task::perform(system::set_wifi(on), Message::Status),
            Message::Bluetooth(on) => return Task::perform(system::set_bluetooth(on), Message::Status),
            Message::Mute(output, muted) => return Task::perform(system::set_muted(output, muted), Message::Status),
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let t = self.tokens;
        let content: Element<'_, Message> = if self.query.trim().is_empty() { pages::page(self, self.page) } else { pages::search(self, &self.query) };
        let mut body = column![content].spacing(16).padding(iced::Padding { top: 20.0, right: 24.0, bottom: 24.0, left: 24.0 }).max_width(640);
        if let Some(error) = &self.error {
            body = body.push(text(error).size(11).color(style::rgb(0xFF3B30)));
        }
        let content = scrollable(container(body).center_x(Length::Fill)).direction(style::thin()).style(t.scroll()).height(Length::Fill);
        row![self.sidebar(), content].height(Length::Fill).into()
    }

    fn sidebar(&self) -> Element<'_, Message> {
        let t = self.tokens;
        let search = container(
            row![
                svg(pages::icon("magnifying-glass")).width(13).height(13).style(Tokens::tint(t.secondary())),
                text_input("Search", &self.query).id("search").on_input(Message::Search).size(13).padding(0).style(t.input(true)),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        )
        .padding([4, 8])
        .height(26)
        .align_y(Alignment::Center)
        .style(t.field());
        let user = button(
            row![
                svg(pages::icon("user-circle")).width(32).height(32).style(Tokens::tint(t.secondary())),
                column![text(&self.user.1).size(13).font(font(Weight::Semibold)), text("Noobsbot Desktop account").size(11).color(t.secondary())],
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .padding([4, 6])
        .width(Length::Fill)
        .style(t.sidebar_row(false));
        let mut list = column![search, user, container(space()).height(1).width(Length::Fill).style(t.rule())].spacing(8);
        let mut rows = column![].spacing(1);
        for page in Page::ALL {
            if page.gap_before() {
                rows = rows.push(space().height(8));
            }
            let selected = self.page == page && self.query.trim().is_empty();
            let item = row![pages::squircle(page, 20.0), text(page.name()).size(13)].spacing(8).align_y(Alignment::Center);
            rows = rows.push(button(item).padding([4, 6]).height(28).width(Length::Fill).style(t.sidebar_row(selected)).on_press(Message::Page(page)));
        }
        list = list.push(scrollable(rows).direction(style::thin()).style(t.scroll()).height(Length::Fill));
        container(container(list).padding(10).width(220).height(Length::Fill).style(t.sidebar())).padding(8).into()
    }
}
