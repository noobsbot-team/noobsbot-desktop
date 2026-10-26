//! Notification banners (E4.1) and Notification Center (E4.2): the store the server feeds, banner timers, where each
//! card and button sits, and what clicks, hovers and swipes do. Drawn by render/notify.rs from the same layouts.

use std::time::Duration;

use smithay::{
    reexports::calloop::{
        RegistrationToken,
        timer::{TimeoutAction, Timer},
    },
    utils::{Logical, Point, Rectangle, Size},
    wayland::{compositor::with_states, shell::xdg::XdgToplevelSurfaceData},
};

use super::{CalendarLayout, DROP, MARGIN, Popup, calendar_click, calendar_layout, grid};
pub use crate::services::notifications::{Notification, Reason};
use crate::{
    config::AppStyle,
    render::menubar,
    services::{Command, notifications},
    state::State,
    text::{Style, ts},
    wm::Rect,
};

pub const BANNER_W: i32 = 344;
/// Banners keep this far from the right edge and from the bar.
const BANNER_RIGHT: i32 = 12;
const BANNER_TOP: i32 = 8;
/// While the OSD shows, banners move down by this much (E2.7).
const OSD_SHIFT: i32 = 56;
const GAP: i32 = 8;
pub const MAX_BANNERS: usize = 3;
const BANNER_TIME: Duration = Duration::from_secs(5);
pub const ICON: i32 = 32;
const PAD: i32 = 12;
const MIN_H: i32 = 64;
/// Where text starts, from the card's left edge: past the icon.
const TEXT_X: i32 = PAD + ICON + 10;
pub const LINE: i32 = 16;
pub const BUTTON_H: i32 = 24;
pub const CLOSE: i32 = 20;
pub const CENTER_W: i32 = 360;
pub const CENTER_PAD: i32 = 12;
/// The "N more" plate under a collapsed group shows this much.
pub const PLATE: i32 = 16;
/// The Weather widget's height.
pub const WEATHER_H: i32 = 156;
/// Widgets from other apps (E8.8): full width (medium) or half, side by side (small).
pub const WIDGET_H: i32 = WEATHER_H;
/// Up Next: its title, then rows of a title and a time line.
const UP_NEXT_TOP: i32 = 36;
const UP_NEXT_ROW: i32 = 40;
/// A horizontal swipe this long dismisses a banner.
const SWIPE: f64 = 80.0;

/// Notification body text (Part D Body), measured when one arrives so layouts stay pure.
pub const BODY: Style = Style { size: 13, line_height: 16, weight: 400, color: [0, 0, 0, 255], tabular: false };
/// The body's width in a banner, the narrower of the two cards.
pub const BODY_W: i32 = BANNER_W - TEXT_X - PAD;

/// Every notification, banner and timer the frame loop holds.
#[derive(Default)]
pub struct Store {
    /// Oldest first; at most `notifications::HISTORY`.
    pub list: Vec<Notification>,
    /// Shown as banners, newest first.
    pub banners: Vec<u32>,
    timers: Vec<(u32, RegistrationToken)>,
    /// The card under the pointer (banner or Notification Center).
    pub hovered: Option<u32>,
    /// The Notification Center group opened out.
    pub expanded: Option<String>,
    /// Notification Center's scroll offset, px.
    pub scroll: i32,
    swipe: f64,
}

impl Store {
    /// Adds `n`, or replaces the one with its id (which moves to the top, as it changed).
    pub fn insert(&mut self, n: Notification) {
        self.list.retain(|o| o.id != n.id);
        self.list.push(n);
        if self.list.len() > notifications::HISTORY {
            let gone: Vec<u32> = self.list.drain(..self.list.len() - notifications::HISTORY).map(|n| n.id).collect();
            self.banners.retain(|id| !gone.contains(id));
        }
    }

    pub fn remove(&mut self, id: u32) -> Option<Notification> {
        self.banners.retain(|b| *b != id);
        let i = self.list.iter().position(|n| n.id == id)?;
        Some(self.list.remove(i))
    }

    pub fn get(&self, id: u32) -> Option<&Notification> {
        self.list.iter().find(|n| n.id == id)
    }

    /// By app, each newest first; the group with the newest notification first.
    pub fn groups(&self) -> Vec<(String, Vec<u32>)> {
        let mut groups: Vec<(String, Vec<u32>)> = Vec::new();
        for n in self.list.iter().rev() {
            match groups.iter_mut().find(|(app, _)| *app == n.app) {
                Some((_, ids)) => ids.push(n.id),
                None => groups.push((n.app.clone(), vec![n.id])),
            }
        }
        groups
    }

    /// The history file's text: everything but transient notifications.
    pub fn history_text(&self) -> String {
        let kept: Vec<Notification> = self.list.iter().filter(|n| !n.transient).cloned().collect();
        notifications::encode_history(&kept)
    }
}

/// How long `n`'s banner stays: `None` for the alert style (critical, or an app asking for no timeout), which stays
/// until acted on.
pub fn banner_time(n: &Notification) -> Option<Duration> {
    match n.expire {
        _ if n.urgency >= 2 => None,
        0 => None,
        ms if ms > 0 => Some(Duration::from_millis(ms as u64)),
        _ => Some(BANNER_TIME),
    }
}

/// The non-default actions, as buttons.
pub fn buttons(n: &Notification) -> impl Iterator<Item = &(String, String)> {
    n.actions.iter().filter(|(key, _)| key != "default")
}

/// One notification card's parts, output-relative.
#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    pub id: u32,
    pub rect: Rect,
    pub icon: Rect,
    /// The app name on the left of the first row, the time on its right.
    pub app: Rect,
    pub time: Rect,
    pub title: Rect,
    pub body: Rect,
    pub progress: Option<Rect>,
    /// The body is shown (`[notifications] previews`).
    pub preview: bool,
    /// The ✕ on the top-left corner, while hovered.
    pub close: Option<Rect>,
    /// Action buttons, while hovered: (rect, key).
    pub actions: Vec<(Rect, String)>,
}

/// A card for `n` at (`x`, `y`), `w` wide; hovered, it shows the ✕ (when `close`) and its buttons.
/// Without `preview` the body stays hidden (`[notifications] previews`).
pub fn card(n: &Notification, x: i32, y: i32, w: i32, hovered: bool, close: bool, preview: bool) -> Card {
    let r = |dx: i32, dy: i32, rw: i32, rh: i32| Rectangle::new((x + dx, y + dy).into(), (rw, rh).into());
    let text_w = w - TEXT_X - PAD;
    let lines = if preview { i32::from(n.body_lines.min(2)) } else { 0 };
    // Text rows grow with the text size; the icon and the side padding stay.
    let (line, top, button_h) = (ts(LINE), ts(26), ts(BUTTON_H));
    let mut bottom = top + line + lines * line;
    let progress = n.progress.map(|_| {
        bottom += 10;
        r(TEXT_X, bottom - 4, text_w, 4)
    });
    let mut actions = Vec::new();
    let keys: Vec<&String> = buttons(n).map(|(key, _)| key).collect();
    if hovered && !keys.is_empty() {
        let count = keys.len() as i32;
        let each = (w - 2 * PAD - (count - 1) * GAP) / count;
        bottom += 8;
        actions = keys.into_iter().enumerate().map(|(i, key)| (r(PAD + i as i32 * (each + GAP), bottom, each, button_h), key.clone())).collect();
        bottom += button_h;
    }
    Card {
        id: n.id,
        rect: r(0, 0, w, (bottom + PAD).max(MIN_H)),
        icon: r(PAD, PAD, ICON, ICON),
        app: r(TEXT_X, ts(10), text_w - ts(48), ts(14)),
        time: r(w - PAD - ts(48), ts(10), ts(48), ts(14)),
        title: r(TEXT_X, top, text_w, line),
        body: r(TEXT_X, top + line, text_w, lines * line),
        progress,
        preview,
        close: (hovered && close).then(|| r(-6, -6, CLOSE, CLOSE)),
        actions,
    }
}

/// The banners on screen (newest on top), and the "+N more" pill under them when there are more than 3.
pub fn banners(store: &Store, output_width: i32, osd: bool, preview: bool) -> (Vec<Card>, Option<(Rect, usize)>) {
    let x = output_width - BANNER_RIGHT - BANNER_W;
    let mut y = menubar::height() + BANNER_TOP + if osd { OSD_SHIFT } else { 0 };
    let mut cards = Vec::new();
    for n in store.banners.iter().filter_map(|id| store.get(*id)).take(MAX_BANNERS) {
        let card = card(n, x, y, BANNER_W, store.hovered == Some(n.id), true, preview);
        y += card.rect.size.h + GAP;
        cards.push(card);
    }
    let more = store.banners.len().saturating_sub(MAX_BANNERS);
    let pill = (more > 0).then(|| (Rectangle::new((x + (BANNER_W - ts(96)) / 2, y).into(), (ts(96), ts(BUTTON_H)).into()), more));
    (cards, pill)
}

/// A Notification Center group: one app's notifications, collapsed under its newest or opened out.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub app: String,
    pub count: usize,
    /// Opened out: the app's name and "Show Less" above the cards.
    pub header: Option<(Rect, Rect)>,
    pub cards: Vec<Card>,
    /// Collapsed with more than one: the plate peeking out under the top card.
    pub plate: Option<Rect>,
    /// Collapsed and hovered: the ✕ that clears the group.
    pub close: Option<Rect>,
}

/// Where everything in Notification Center sits, output-relative and already scrolled.
#[derive(Debug, Clone, PartialEq)]
pub struct Center {
    pub rect: Rect,
    pub date: Rect,
    pub settings: Rect,
    /// "Do Not Disturb is on" and its Turn Off button.
    pub dnd: Option<(Rect, Rect)>,
    /// "Notifications" and Clear All; `None` with nothing to show.
    pub header: Option<(Rect, Rect)>,
    pub groups: Vec<Group>,
    /// Up Next (E7.2): its card and one row per event; `None` with nothing coming up.
    pub up_next: Option<(Rect, Vec<Rect>)>,
    /// The widget card, and the month grid inside it.
    pub calendar: (Rect, CalendarLayout),
    /// The Batteries widget, with one slot per battery.
    pub batteries: Option<(Rect, Vec<Rect>)>,
    /// The Weather widget (E7.2), while it has a forecast.
    pub weather: Option<Rect>,
    /// Other apps' widgets, in `State::shown_widgets` order.
    pub widgets: Vec<Rect>,
    /// "Edit Widgets…" under them: Settings ▸ Notifications, like the ⚙.
    pub edit: Rect,
    pub max_scroll: i32,
}

impl Center {
    /// Inside the column's padding: what is drawn and hit (anything partly scrolled out is left out).
    pub fn visible(&self, r: Rect) -> bool {
        let inner = Rectangle::new((self.rect.loc.x, self.rect.loc.y + 8).into(), (self.rect.size.w, self.rect.size.h - 16).into());
        let r = Rectangle::new((r.loc.x.max(inner.loc.x), r.loc.y).into(), r.size);
        inner.contains_rect(r)
    }
}

/// Notification Center for `store` on an `output`-sized screen, with `rows` calendar weeks, `up_next` upcoming
/// events, `batteries` batteries, maybe the Weather widget and other apps' widgets (`true`: medium), bodies shown
/// with `preview`.
#[expect(clippy::too_many_arguments, reason = "one count per section")]
pub fn center(
    store: &Store,
    output: Size<i32, Logical>,
    dnd: bool,
    (rows, weeks): (usize, bool),
    up_next: usize,
    batteries: usize,
    weather: bool,
    widgets: &[bool],
    preview: bool,
) -> Center {
    let x = output.w - MARGIN - CENTER_W;
    let top = menubar::height() + DROP;
    let rect = Rectangle::new((x, top).into(), (CENTER_W, (output.h - top - MARGIN).max(100)).into());
    let inner_x = x + CENTER_PAD;
    let inner_w = CENTER_W - 2 * CENTER_PAD;
    let start = top + 16 - store.scroll;
    let mut y = start;
    let row = |x: i32, y: i32, w: i32, h: i32| Rectangle::new((x, y).into(), (w, h).into());
    // Rows that hold text grow with the text size (E8.4); widths, icons and gaps stay.
    let (head, button) = (ts(22), ts(76));
    let date = row(inner_x + 4, y, inner_w - 40, head);
    let settings = row(x + CENTER_W - CENTER_PAD - 4 - 22, y + (head - 22) / 2, 22, 22);
    y += head + 12;
    let dnd = dnd.then(|| {
        let banner = row(inner_x, y, inner_w, ts(40));
        y += banner.size.h + 12;
        (banner, row(inner_x + inner_w - 8 - button, banner.loc.y + ts(8), button, ts(BUTTON_H)))
    });
    let header = (!store.list.is_empty()).then(|| {
        let h = (row(inner_x + 4, y + ts(3), 160, ts(16)), row(inner_x + inner_w - button, y, button, head));
        y += head + 8;
        h
    });
    let mut groups = Vec::new();
    for (app, ids) in store.groups() {
        let open = ids.len() > 1 && store.expanded.as_ref() == Some(&app);
        let group_hovered = ids.iter().any(|id| store.hovered == Some(*id));
        let mut group = Group { app: app.clone(), count: ids.len(), header: None, cards: Vec::new(), plate: None, close: None };
        if open {
            group.header = Some((row(inner_x + 4, y + ts(3), 200, ts(16)), row(inner_x + inner_w - ts(84), y, ts(84), head)));
            y += head + 8;
            for n in ids.iter().filter_map(|id| store.get(*id)) {
                let card = card(n, inner_x, y, inner_w, store.hovered == Some(n.id), true, preview);
                y += card.rect.size.h + GAP;
                group.cards.push(card);
            }
        } else if let Some(n) = store.get(ids[0]) {
            let card = card(n, inner_x, y, inner_w, store.hovered == Some(n.id), false, preview);
            let bottom = card.rect.loc.y + card.rect.size.h;
            group.close = group_hovered.then(|| row(inner_x - 6, y - 6, CLOSE, CLOSE));
            group.plate = (ids.len() > 1).then(|| row(inner_x + 10, bottom - 8, inner_w - 20, 8 + ts(PLATE)));
            y = bottom + if ids.len() > 1 { ts(PLATE) } else { 0 } + GAP;
            group.cards.push(card);
        }
        groups.push(group);
    }
    y += 4;
    let up_next = (up_next > 0).then(|| {
        let (top, each) = (ts(UP_NEXT_TOP), ts(UP_NEXT_ROW));
        let card = row(inner_x, y, inner_w, top + up_next as i32 * each + 6);
        y += card.size.h + GAP;
        (card, (0..up_next as i32).map(|i| row(inner_x + 12, card.loc.y + top + i * each, inner_w - 24, each - 6)).collect())
    });
    let cal_h = super::calendar::height(rows);
    let calendar = (row(inner_x, y, inner_w, cal_h), calendar_layout(inner_x + (inner_w - super::calendar::WIDTH) / 2, y, rows, weeks));
    y += cal_h + GAP;
    let weather = weather.then(|| {
        let card = row(inner_x, y, inner_w, ts(WEATHER_H));
        y += card.size.h + GAP;
        card
    });
    let batteries = (batteries > 0).then(|| {
        let card = row(inner_x, y, inner_w, ts(68));
        let each = (inner_w - 24) / batteries.min(3) as i32;
        y += card.size.h + GAP;
        (card, (0..batteries.min(3) as i32).map(|i| row(inner_x + 12 + i * each, card.loc.y + ts(36), each, ts(20))).collect())
    });
    let (widgets, bottom) = tiles(widgets, inner_x, y, inner_w);
    let edit = row(inner_x + (inner_w - ts(110)) / 2, bottom, ts(110), ts(BUTTON_H));
    y = bottom + edit.size.h;
    let content = y - start + 8;
    Center {
        rect,
        date,
        settings,
        dnd,
        header,
        groups,
        up_next,
        calendar,
        batteries,
        weather,
        widgets,
        edit,
        max_scroll: (content - (rect.size.h - 16)).max(0),
    }
}

/// Widget cards from `y` down in a `w`-wide column: medium ones full width, small ones two to a row; and the y
/// below them.
pub fn tiles(medium: &[bool], x: i32, mut y: i32, w: i32) -> (Vec<Rect>, i32) {
    let (half, widget_h) = ((w - GAP) / 2, ts(WIDGET_H));
    // A small card waiting for a neighbour on its right.
    let mut open = false;
    let mut out = Vec::new();
    for &m in medium {
        if open && !m {
            out.push(Rectangle::new((x + w - half, y).into(), (half, widget_h).into()));
            open = false;
            y += widget_h + GAP;
            continue;
        }
        if open {
            y += widget_h + GAP;
        }
        out.push(Rectangle::new((x, y).into(), (if m { w } else { half }, widget_h).into()));
        open = !m;
        if m {
            y += widget_h + GAP;
        }
    }
    (out, if open { y + widget_h + GAP } else { y })
}

/// What a click in Notification Center hits.
#[derive(Debug, Clone, PartialEq)]
enum Hit {
    /// The ⚙ beside the date: Settings ▸ Notifications.
    Settings,
    ClearAll,
    DndOff,
    Collapse,
    ClearGroup(String),
    Expand(String),
    Close(u32),
    Action(u32, String),
    Card(u32),
    Calendar,
    /// Other apps' widget, by its place in `Center::widgets`.
    Widget(usize),
}

fn center_hit(center: &Center, location: Point<f64, Logical>) -> Option<Hit> {
    let on = |r: Rect| center.visible(r) && r.to_f64().contains(location);
    if on(center.settings) || on(center.edit) {
        return Some(Hit::Settings);
    }
    if center.header.is_some_and(|(_, clear)| on(clear)) {
        return Some(Hit::ClearAll);
    }
    if center.dnd.is_some_and(|(_, off)| on(off)) {
        return Some(Hit::DndOff);
    }
    for group in &center.groups {
        if group.header.is_some_and(|(_, less)| on(less)) {
            return Some(Hit::Collapse);
        }
        if group.close.is_some_and(on) {
            return Some(Hit::ClearGroup(group.app.clone()));
        }
        for card in &group.cards {
            if let Some((_, key)) = card.actions.iter().find(|(r, _)| on(*r)) {
                return Some(Hit::Action(card.id, key.clone()));
            }
            if card.close.is_some_and(on) {
                return Some(Hit::Close(card.id));
            }
            if on(card.rect) || group.plate.is_some_and(on) {
                return Some(if group.plate.is_some() { Hit::Expand(group.app.clone()) } else { Hit::Card(card.id) });
            }
        }
    }
    if let Some(i) = center.widgets.iter().position(|r| on(*r)) {
        return Some(Hit::Widget(i));
    }
    on(center.calendar.0).then_some(Hit::Calendar)
}

/// What a click on the banners hits: (id, the ✕, an action key); `None` id for the "+N more" pill.
fn banner_hit(cards: &[Card], more: Option<(Rect, usize)>, location: Point<f64, Logical>) -> Option<(Option<u32>, bool, Option<String>)> {
    if more.is_some_and(|(r, _)| r.to_f64().contains(location)) {
        return Some((None, false, None));
    }
    cards.iter().find_map(|card| {
        let close = card.close.is_some_and(|r| r.to_f64().contains(location));
        let action = card.actions.iter().find(|(r, _)| r.to_f64().contains(location)).map(|(_, k)| k.clone());
        (close || card.rect.to_f64().contains(location)).then_some((Some(card.id), close, action))
    })
}

impl State {
    fn center_open(&self) -> bool {
        matches!(self.popup, Some(Popup::Notifications { .. }))
    }

    /// The banners as laid out now; none while Notification Center is open.
    pub fn banner_layout(&self) -> (Vec<Card>, Option<(Rect, usize)>) {
        if self.center_open() {
            return (Vec::new(), None);
        }
        banners(&self.notifications, self.first_geometry().map_or(0, |g| g.size.w), self.osd.is_some(), self.previews())
    }

    /// Laptop battery, then Bluetooth devices that report one, then UPower's other peripherals: (name, percent).
    pub fn batteries(&self) -> Vec<(String, u8)> {
        let laptop = self.battery.as_ref().map(|b| ("Laptop".to_owned(), b.percent));
        let devices = self.bluetooth.iter().flat_map(|b| &b.devices).filter(|d| d.connected).filter_map(|d| Some((d.name.clone(), d.battery?)));
        let mut list: Vec<_> = laptop.into_iter().chain(devices).collect();
        // ponytail: a Bluetooth device in both lists is matched by name; UPower's model is usually BlueZ's alias.
        for p in &self.peripherals {
            if !list.iter().any(|(name, _)| name.eq_ignore_ascii_case(&p.name)) {
                list.push((p.name.clone(), p.percent));
            }
        }
        list
    }

    /// Other apps' widgets for Notification Center or (`desktop`) the desktop, in `[widgets]` order.
    pub fn shown_widgets(&self, desktop: bool) -> Vec<&crate::services::widgets::Widget> {
        crate::services::widgets::shown(&self.widgets, &self.config.widgets, desktop)
    }

    /// Tells the owner of a clicked widget.
    pub fn widget_activated(&self, desktop: bool, i: usize) {
        if let Some(w) = self.shown_widgets(desktop).get(i) {
            self.services.send(Command::WidgetActivated { owner: w.owner.clone(), id: w.id.clone() });
        }
    }

    pub fn center_layout(&self) -> Option<Center> {
        let Some(Popup::Notifications { year, month }) = self.popup else { return None };
        let rows = grid(year, month, self.config.clock.week_start.into()).len();
        let up_next = self.calendar_view(year, month, self.config.clock.week_start.into()).1.len();
        let widgets: Vec<bool> = self.shown_widgets(false).iter().map(|w| w.medium).collect();
        let (dnd, batteries, weather) = (self.config.notifications.dnd, self.batteries().len(), self.weather.is_some());
        Some(center(
            &self.notifications,
            self.popup_size(),
            dnd,
            (rows, self.config.clock.week_numbers),
            up_next,
            batteries,
            weather,
            &widgets,
            self.previews(),
        ))
    }

    /// Bodies shown: `[notifications] previews`, and whether the screen is locked.
    pub(crate) fn previews(&self) -> bool {
        self.config.notifications.previews.show(self.locked())
    }

    /// A notification from an app: dropped if its app is off (`[notification_apps]`), else stored, and shown as a
    /// banner unless its app's style is "none", Do Not Disturb is on, a full-screen app has focus (unless
    /// `[notifications] full_screen`), or Notification Center is open. A new one plays its app's sound unless Do Not
    /// Disturb or the full-screen app keeps it quiet; an app kept out of Notification Center loses it with its banner.
    pub fn notify(&mut self, mut n: Notification) {
        n.body_lines = self.body_lines(&n.body);
        let id = n.id;
        let rule = self.config.notification_apps.get(&n.app).copied().unwrap_or_default();
        let style = rule.style;
        if style == AppStyle::Off {
            return self.services.send(Command::NotificationClosed { id, reason: Reason::Dismissed });
        }
        if style == AppStyle::Alert {
            n.expire = 0;
        }
        if !rule.center {
            n.transient = true;
        }
        // Locked: no banner; listed on the lock screen if its app allows, else only counted (privacy).
        if let Some(lock) = self.lock.as_mut() {
            lock.arrived(crate::shell::lock::listed(&n, rule.lock, self.config.notifications.previews));
        }
        let dnd = self.config.notifications.dnd;
        let covered = !self.config.notifications.full_screen && self.menu_bar().is_none();
        if rule.sound && !n.silent && !dnd && !covered && self.notifications.get(id).is_none() {
            crate::bell::play("message-new-instant", 100);
        }
        let quiet = style == AppStyle::None || dnd || self.locked() || covered || self.center_open();
        let transient = n.transient;
        self.notifications.insert(n);
        self.notifications.banners.retain(|b| *b != id);
        if !quiet {
            self.notifications.banners.insert(0, id);
            self.arm(id);
        } else if transient {
            // Never seen and never kept: it expires at once.
            self.close_notification(id, Reason::Expired);
        }
        self.save_history();
        self.schedule_redraw();
    }

    /// The saved history, under anything that arrived meanwhile.
    pub fn restore_notifications(&mut self, mut list: Vec<Notification>) {
        for n in &mut list {
            n.body_lines = self.body_lines(&n.body);
        }
        list.retain(|n| self.notifications.get(n.id).is_none());
        list.append(&mut self.notifications.list);
        self.notifications.list = list;
        self.schedule_redraw();
    }

    fn body_lines(&mut self, body: &str) -> u8 {
        if body.is_empty() {
            return 0;
        }
        let lines = self.text.wrapped(body, BODY, BODY_W as u32, 2, 1.0).height / ts(BODY.line_height.into()) as u32;
        lines.clamp(1, 2) as u8
    }

    /// (Re)starts `id`'s banner timer; the alert style has none.
    fn arm(&mut self, id: u32) {
        self.disarm(id);
        let Some(time) = self.notifications.get(id).and_then(banner_time) else { return };
        let timer = self.loop_handle.insert_source(Timer::from_duration(time), move |_, _, state| {
            state.notifications.timers.retain(|(t, _)| *t != id);
            state.banner_expired(id);
            TimeoutAction::Drop
        });
        match timer {
            Ok(token) => self.notifications.timers.push((id, token)),
            Err(err) => tracing::warn!("banner timer: {err}"),
        }
    }

    fn disarm(&mut self, id: u32) {
        let (gone, kept) = std::mem::take(&mut self.notifications.timers).into_iter().partition(|(t, _)| *t == id);
        self.notifications.timers = kept;
        for (_, token) in gone {
            self.loop_handle.remove(token);
        }
    }

    /// Its banner goes; a transient notification goes with it.
    fn banner_expired(&mut self, id: u32) {
        self.notifications.banners.retain(|b| *b != id);
        if self.notifications.get(id).is_some_and(|n| n.transient) {
            self.close_notification(id, Reason::Expired);
        }
        self.schedule_redraw();
    }

    /// Removes `id` and tells its app why. Unknown ids are ignored.
    pub fn close_notification(&mut self, id: u32, reason: Reason) {
        self.disarm(id);
        if self.notifications.remove(id).is_some() {
            self.services.send(Command::NotificationClosed { id, reason });
            self.save_history();
            self.schedule_redraw();
        }
    }

    fn clear(&mut self, ids: Vec<u32>) {
        for id in ids {
            self.close_notification(id, Reason::Dismissed);
        }
    }

    pub(crate) fn invoke(&mut self, id: u32, key: String) {
        let resident = self.notifications.get(id).is_some_and(|n| n.resident);
        if id == crate::services::update::NOTICE_ID {
            if key == "install" {
                self.install_update();
            } else if let Some(url) = self.update.as_ref().map(|r| r.url.clone()) {
                self.spawn(&["xdg-open".into(), url]);
            }
        } else if id == crate::services::network::PORTAL_NOTICE {
            if let Some(url) = self.network.as_ref().and_then(|n| n.portal.clone()) {
                self.spawn(&["xdg-open".into(), url]);
            }
        } else if id == crate::housekeep::LOW_DISK {
            self.spawn(&["noobsbot-desktop-settings".into(), "--page".into(), "general".into()]);
        } else if !self.disk_action(id, &key) {
            self.services.send(Command::NotificationAction { id, key });
        }
        if !resident {
            self.close_notification(id, Reason::Dismissed);
        }
    }

    /// A click on the notification: its default action, else the app's window comes forward and the banner goes.
    pub(crate) fn activate(&mut self, id: u32) {
        let Some(n) = self.notifications.get(id) else { return };
        if n.actions.iter().any(|(key, _)| key == "default") {
            return self.invoke(id, "default".into());
        }
        let wanted = [n.desktop_entry.to_lowercase(), n.app.to_lowercase()];
        let window = self.space.elements().find(|w| {
            let id = w
                .toplevel()
                .and_then(|t| with_states(t.wl_surface(), |s| s.data_map.get::<XdgToplevelSurfaceData>().and_then(|d| d.lock().ok()?.app_id.clone())));
            id.is_some_and(|id| wanted.iter().any(|w| !w.is_empty() && id.to_lowercase().ends_with(w.as_str())))
        });
        if let Some(window) = window.cloned() {
            self.focus_window(Some(window), smithay::utils::SERIAL_COUNTER.next_serial());
        }
        self.disarm(id);
        self.notifications.banners.retain(|b| *b != id);
        self.schedule_redraw();
    }

    fn save_history(&self) {
        self.services.send(Command::SaveNotifications(self.notifications.history_text()));
    }

    /// A press on the banners. `true`: it hit one.
    pub fn banner_press(&mut self, location: Point<f64, Logical>) -> bool {
        let (cards, more) = self.banner_layout();
        let Some((id, close, action)) = banner_hit(&cards, more, location) else { return false };
        match (id, close, action) {
            (None, ..) => {
                let (year, month) = super::this_month();
                // On the banners' display, the first.
                self.bar_output = None;
                self.popup = Some(Popup::Notifications { year, month });
                self.schedule_redraw();
            }
            (Some(id), true, _) => self.close_notification(id, Reason::Dismissed),
            (Some(id), _, Some(key)) => self.invoke(id, key),
            (Some(id), ..) => self.activate(id),
        }
        true
    }

    pub(super) fn center_press(&mut self, location: Point<f64, Logical>) {
        let Some(center) = self.center_layout() else { return };
        match center_hit(&center, location) {
            Some(Hit::Settings) => {
                self.popup = None;
                self.spawn(&["noobsbot-desktop-settings".into(), "--page".into(), "notifications".into()]);
            }
            Some(Hit::ClearAll) => self.clear(self.notifications.list.iter().map(|n| n.id).collect()),
            Some(Hit::DndOff) => {
                self.config.notifications.dnd = false;
                crate::config::save_key("notifications", "dnd", "false");
            }
            Some(Hit::Collapse) => self.notifications.expanded = None,
            Some(Hit::Expand(app)) => self.notifications.expanded = Some(app),
            Some(Hit::ClearGroup(app)) => self.clear(self.notifications.list.iter().filter(|n| n.app == app).map(|n| n.id).collect()),
            Some(Hit::Close(id)) => self.close_notification(id, Reason::Dismissed),
            Some(Hit::Action(id, key)) => self.invoke(id, key),
            Some(Hit::Card(id)) => {
                self.activate(id);
                self.popup = None;
            }
            Some(Hit::Calendar) => {
                if let Some(Popup::Notifications { year, month }) = self.popup
                    && let Some((year, month)) = calendar_click(&center.calendar.1, location, (year, month))
                {
                    self.popup = Some(Popup::Notifications { year, month });
                }
            }
            Some(Hit::Widget(i)) => {
                self.widget_activated(false, i);
                self.popup = None;
            }
            None => {}
        }
        // Cards grow and go: keep the scroll in range.
        if let Some(center) = self.center_layout() {
            self.notifications.scroll = self.notifications.scroll.min(center.max_scroll);
        }
        self.schedule_redraw();
    }

    /// The pointer at `location`: the card under it shows its ✕ and buttons, and a hovered banner's timer waits.
    pub fn notify_motion(&mut self, location: Point<f64, Logical>) {
        let at = self.to_popup(location);
        let hovered = match self.center_layout() {
            Some(center) if center.rect.to_f64().contains(at) => {
                let location = at;
                center
                    .groups
                    .iter()
                    .flat_map(|g| g.cards.iter().map(move |c| (c, g.plate)))
                    .find(|(c, plate)| {
                        let grown = Rectangle::new(c.rect.loc - Point::from((6, 6)), c.rect.size + Size::from((6, 6)));
                        center.visible(c.rect) && (grown.to_f64().contains(location) || plate.is_some_and(|p| p.to_f64().contains(location)))
                    })
                    .map(|(c, _)| c.id)
            }
            Some(_) => None,
            None => {
                let (cards, _) = self.banner_layout();
                cards
                    .iter()
                    .find(|c| Rectangle::new(c.rect.loc - Point::from((6, 6)), c.rect.size + Size::from((6, 6))).to_f64().contains(location))
                    .map(|c| c.id)
            }
        };
        let old = self.notifications.hovered;
        if hovered == old {
            return;
        }
        self.notifications.hovered = hovered;
        self.notifications.swipe = 0.0;
        if let Some(id) = old.filter(|id| self.notifications.banners.contains(id)) {
            self.arm(id);
        }
        if let Some(id) = hovered {
            self.disarm(id);
        }
        self.schedule_redraw();
    }

    /// A scroll at `location`: scrolls Notification Center, or swipes a banner away. `true`: the shell took it.
    pub fn notify_scroll(&mut self, location: Point<f64, Logical>, dx: f64, dy: f64) -> bool {
        if let Some(center) = self.center_layout() {
            if !center.rect.to_f64().contains(self.to_popup(location)) {
                return false;
            }
            let scroll = (self.notifications.scroll + dy.round() as i32).clamp(0, center.max_scroll);
            if scroll != self.notifications.scroll {
                self.notifications.scroll = scroll;
                self.notifications.hovered = None;
                self.schedule_redraw();
            }
            return true;
        }
        let Some(id) = self.notifications.hovered.filter(|id| self.notifications.banners.contains(id)) else { return false };
        // ponytail: dismissed at the threshold, without following the finger; that comes with the spring module.
        self.notifications.swipe += dx;
        if self.notifications.swipe.abs() > SWIPE {
            self.notifications.swipe = 0.0;
            self.notifications.hovered = None;
            self.close_notification(id, Reason::Dismissed);
        }
        true
    }

    /// Whether `location` is on a banner.
    pub fn on_banner(&self, location: Point<f64, Logical>) -> bool {
        let (cards, more) = self.banner_layout();
        banner_hit(&cards, more, location).is_some()
    }
}

/// Sample notifications for `--show notification` / `notification-center`.
pub fn samples() -> Vec<Notification> {
    let now = chrono::Local::now().timestamp();
    let n = |id: u32, app: &str, icon: &str, summary: &str, body: &str, minutes: i64| Notification {
        id,
        app: app.into(),
        icon_name: icon.into(),
        icon: notifications::load_icon(icon).map(std::sync::Arc::new),
        summary: summary.into(),
        body: body.into(),
        urgency: 1,
        expire: 0,
        time: now - minutes * 60,
        ..Default::default()
    };
    vec![
        n(1, "System", "drive-removable-media", "USB drive connected", "SanDisk Ultra (32 GB) is ready.", 90),
        n(2, "Helium", "helium-browser", "Download complete", "invoice-october.pdf (180 KB)", 40),
        n(3, "Helium", "helium-browser", "Download complete", "photos.zip (48 MB)", 20),
        Notification {
            actions: vec![("default".into(), "Open".into()), ("open".into(), "Open".into()), ("show".into(), "Show in Folder".into())],
            ..n(4, "Helium", "helium-browser", "Download complete", "report.pdf (2.4 MB) was saved to your Downloads folder.", 0)
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(id: u32, app: &str) -> Notification {
        Notification { id, app: app.into(), summary: "Title".into(), body: "Body".into(), body_lines: 1, urgency: 1, expire: -1, ..Default::default() }
    }

    #[test]
    fn replacing_keeps_one_and_moves_it_up() {
        let mut store = Store::default();
        for (id, app) in [(1, "A"), (2, "B"), (3, "A")] {
            store.insert(note(id, app));
        }
        store.insert(Notification { summary: "New".into(), ..note(1, "A") });
        assert_eq!(store.list.iter().map(|n| n.id).collect::<Vec<_>>(), vec![2, 3, 1]);
        assert_eq!(store.get(1).map(|n| n.summary.as_str()), Some("New"));
        for id in 10..120 {
            store.insert(note(id, "C"));
        }
        assert_eq!(store.list.len(), notifications::HISTORY);
        assert!(store.get(2).is_none() && store.get(119).is_some());
        assert_eq!(store.remove(119).map(|n| n.id), Some(119));
        assert!(store.remove(119).is_none());
    }

    #[test]
    fn groups_by_app_newest_first() {
        let mut store = Store::default();
        for (id, app) in [(1, "A"), (2, "B"), (3, "A"), (4, "C"), (5, "B")] {
            store.insert(note(id, app));
        }
        assert_eq!(store.groups(), vec![("B".into(), vec![5, 2]), ("C".into(), vec![4]), ("A".into(), vec![3, 1])]);
        store.list[0].transient = true;
        assert_eq!(store.history_text().lines().count(), 4);
    }

    #[test]
    fn previews_off_hide_the_body() {
        let n = Notification { body_lines: 2, ..note(1, "Mail") };
        let (shown, hidden) = (card(&n, 0, 0, BANNER_W, false, false, true), card(&n, 0, 0, BANNER_W, false, false, false));
        assert!(shown.preview && shown.body.size.h > 0);
        assert!(!hidden.preview && hidden.body.size.h == 0 && hidden.rect.size.h < shown.rect.size.h);
    }

    #[test]
    fn banner_times() {
        assert_eq!(banner_time(&note(1, "A")), Some(BANNER_TIME));
        assert_eq!(banner_time(&Notification { expire: 1500, ..note(1, "A") }), Some(Duration::from_millis(1500)));
        assert_eq!(banner_time(&Notification { expire: 0, ..note(1, "A") }), None);
        assert_eq!(banner_time(&Notification { urgency: 2, ..note(1, "A") }), None);
    }

    #[test]
    fn banners_stack_under_the_bar_and_dodge_the_osd() {
        let mut store = Store::default();
        for id in 1..=5 {
            store.insert(note(id, "A"));
            store.banners.insert(0, id);
        }
        let (cards, more) = banners(&store, 1280, false, true);
        assert_eq!(cards.len(), 3);
        assert_eq!(cards[0].id, 5);
        assert_eq!(cards[0].rect, Rectangle::new((1280 - 12 - 344, 32).into(), (344, 70).into()));
        assert_eq!(cards[1].rect.loc.y, 32 + 70 + 8);
        assert_eq!(more.map(|(_, n)| n), Some(2));
        assert!(cards.iter().all(|c| c.rect.contains_rect(c.icon) && c.rect.contains_rect(c.body) && c.close.is_none()));
        // The OSD (top 28, 48 tall) never overlaps the first banner.
        let (moved, _) = banners(&store, 1280, true, true);
        assert_eq!(moved[0].rect.loc.y, 32 + 56);
        assert!(!moved[0].rect.overlaps(super::super::osd_rect(1280)));
        // Hovered: the ✕ and the buttons appear, and the card grows to hold them.
        store.list[4].actions = vec![("default".into(), "Open".into()), ("reply".into(), "Reply".into())];
        store.hovered = Some(5);
        let (cards, _) = banners(&store, 1280, false, true);
        assert_eq!(cards[0].actions.len(), 1);
        assert!(cards[0].close.is_some() && cards[0].rect.contains_rect(cards[0].actions[0].0));
        assert_eq!(banner_hit(&cards, more, cards[0].actions[0].0.to_f64().loc + Point::from((2.0, 2.0))), Some((Some(5), false, Some("reply".into()))));
    }

    #[test]
    fn text_rows_grow_with_the_text_size() {
        let mut store = Store::default();
        for (id, app) in [(1, "A"), (2, "A"), (3, "B")] {
            store.insert(Notification { body_lines: 2, actions: vec![("default".into(), "Open".into()), ("x".into(), "Reply".into())], ..note(id, app) });
            store.banners.insert(0, id);
        }
        store.hovered = Some(3);
        let lay = || (banners(&store, 1280, false, true).0, center(&store, Size::from((1280, 3000)), true, (6, false), 2, 2, true, &[true, false], true));
        let (small, small_c) = lay();
        let (big, big_c) = crate::text::with_scale(1.5, lay);
        // Line heights at 1.5: headline and body 24, subheadline 21, caption 20 (19.5 rounded), title3 30.
        let card = &big[0];
        assert!(card.title.size.h >= 24 && card.body.size.h >= 48 && card.app.size.h >= 21 && card.time.size.h >= 20);
        assert!(card.actions.iter().all(|(r, _)| r.size.h >= 36 && card.rect.contains_rect(*r)));
        assert!([card.app, card.time, card.title, card.body, card.icon].iter().all(|r| card.rect.contains_rect(*r)));
        assert!(card.body.loc.y >= card.title.loc.y + card.title.size.h && card.icon.size.h == ICON);
        assert!(card.rect.size.h > small[0].rect.size.h && card.rect.size.w == small[0].rect.size.w);
        let c = &big_c;
        assert!(c.date.size.h >= 33 && c.header.unwrap().0.size.h >= 21 && c.header.unwrap().1.size.h >= 22);
        let (strip, off) = c.dnd.unwrap();
        assert!(strip.size.h >= 36 && strip.contains_rect(off) && off.size.h >= 22);
        let plate = c.groups.iter().find_map(|g| g.plate).unwrap();
        assert!(plate.size.h - 8 >= 20);
        let (next, rows) = c.up_next.clone().unwrap();
        assert!(rows.iter().all(|r| next.contains_rect(*r) && r.size.h >= ts(19) + 21) && rows[0].loc.y >= next.loc.y + ts(10) + 30);
        let weather = c.weather.unwrap();
        assert!(weather.size.h >= ts(132) + 24 && c.widgets[0].size.h == weather.size.h);
        let (bat, slots) = c.batteries.clone().unwrap();
        assert!(slots.iter().all(|s| bat.contains_rect(*s) && s.size.h >= 24));
        assert!(c.calendar.0.contains_rect(c.calendar.1.rect) && c.max_scroll == 0);
        assert!(c.calendar.0.size.h > small_c.calendar.0.size.h && c.widgets[1].loc.y > small_c.widgets[1].loc.y);
    }

    #[test]
    fn small_widgets_pair_up() {
        let (rects, bottom) = tiles(&[false, true, false, false, false], 0, 0, 100);
        let at = |r: Rect| (r.loc.x, r.loc.y, r.size.w);
        let row = WIDGET_H + GAP;
        assert_eq!(rects.iter().map(|r| at(*r)).collect::<Vec<_>>(), vec![(0, 0, 46), (0, row, 100), (0, 2 * row, 46), (54, 2 * row, 46), (0, 3 * row, 46)]);
        assert_eq!(bottom, 4 * row);
        assert_eq!(tiles(&[], 0, 5, 100), (Vec::new(), 5));
    }

    #[test]
    fn center_collapses_groups_and_scrolls() {
        let mut store = Store::default();
        for (id, app) in [(1, "A"), (2, "B"), (3, "A"), (4, "A")] {
            store.insert(note(id, app));
        }
        let size = Size::from((1280, 720));
        let center = center(&store, size, true, (5, false), 0, 1, true, &[true], true);
        assert_eq!(center.rect, Rectangle::new((1280 - 8 - 360, 28).into(), (360, 720 - 28 - 8).into()));
        assert_eq!(center.groups.iter().map(|g| (g.app.as_str(), g.count, g.cards.len())).collect::<Vec<_>>(), vec![("A", 3, 1), ("B", 1, 1)]);
        assert!(center.groups[0].plate.is_some() && center.groups[1].plate.is_none());
        let inside = |r: Rect| center.rect.contains_rect(r);
        assert!(inside(center.calendar.0) && center.calendar.0.contains_rect(center.calendar.1.rect) && center.dnd.is_some());
        let weather = center.weather.unwrap();
        assert!(weather.loc.y > center.calendar.0.loc.y && center.batteries.as_ref().unwrap().0.loc.y == weather.loc.y + WEATHER_H + GAP);
        let at = |r: Rect| r.to_f64().loc + Point::from((3.0, 3.0));
        assert_eq!(center.widgets[0].loc.y, center.batteries.as_ref().unwrap().0.loc.y + 68 + GAP);
        let tall = super::center(&store, Size::from((1280, 2000)), true, (5, false), 0, 1, true, &[true], true);
        assert_eq!(center_hit(&tall, at(tall.widgets[0])), Some(Hit::Widget(0)));
        assert_eq!(center_hit(&tall, at(tall.edit)), Some(Hit::Settings), "Edit Widgets… opens Settings too");
        assert_eq!(tall.edit.loc.y, tall.widgets[0].loc.y + ts(WIDGET_H) + GAP);
        assert_eq!(center_hit(&center, at(center.groups[0].cards[0].rect)), Some(Hit::Expand("A".into())));
        assert_eq!(center_hit(&center, at(center.groups[1].cards[0].rect)), Some(Hit::Card(2)));
        assert_eq!(center_hit(&center, at(center.header.unwrap().1)), Some(Hit::ClearAll));
        assert_eq!(center_hit(&center, at(center.settings)), Some(Hit::Settings));
        // Opened out: every card, newest first, and Show Less.
        store.expanded = Some("A".into());
        let open = super::center(&store, size, false, (5, false), 0, 0, false, &[], true);
        assert_eq!(open.groups[0].cards.iter().map(|c| c.id).collect::<Vec<_>>(), vec![4, 3, 1]);
        assert_eq!(center_hit(&open, at(open.groups[0].header.unwrap().1)), Some(Hit::Collapse));
        // Too tall for a short screen: it scrolls, and what scrolled out is not hit.
        let short = super::center(&store, Size::from((1280, 400)), false, (5, false), 0, 0, false, &[], true);
        assert!(short.max_scroll > 0);
        store.scroll = short.max_scroll;
        let scrolled = super::center(&store, Size::from((1280, 400)), false, (5, false), 0, 0, false, &[], true);
        assert!(!scrolled.visible(scrolled.date));
        assert!(scrolled.visible(scrolled.calendar.0));
        // Up Next sits above the calendar, its rows inside its card.
        let next = super::center(&store, size, false, (5, false), 2, 0, false, &[], true);
        let (card, rows) = next.up_next.clone().unwrap();
        assert!(rows.len() == 2 && rows.iter().all(|r| card.contains_rect(*r)) && !rows[0].overlaps(rows[1]));
        assert!(card.loc.y + card.size.h < next.calendar.0.loc.y);
        assert!(center.up_next.is_none());
    }
}
