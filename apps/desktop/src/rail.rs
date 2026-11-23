//! The rail beside an open message: its thread as a graph, as `git log --graph` draws commits,
//! one row per message. Open, each row says who wrote it, when, and how it begins; as a strip,
//! only the graph shows, each dot saying the same in its tooltip. Either way a row opens its
//! message.

use jiff::ZonedDateTime;
use postden_app::{ThreadOrder, ThreadRail};
use postden_core::message::Role;
use postden_store::StoredMessage;
use rune::{Color, El, Icon, ScrollSpec, Shape, Transition, Tx, canvas, col, el, oklch, row, text};
use rune_kit::{IconButton, IconButtonVariant, Kit};

use crate::commands::Command;
use crate::state::{Mail, Reader, message_key, row_key};
use crate::thread::{Row, Segment, Thread};

/// How tall each row is, open or as a strip, so the graph's lines meet from one to the next.
const ROW_HEIGHT: f32 = 44.0;
/// How far apart lanes are.
const LANE_WIDTH: f32 = 14.0;
/// The most lanes drawn side by side: a thread branching further, as one message answered by
/// dozens, draws its later lanes over the last, so the rail never crowds out the message.
const MAX_LANES: usize = 6;
/// The room left of the first lane, and right of the last.
const GRAPH_PADDING: f32 = 11.0;
/// How wide the rail is open.
const OPEN_WIDTH: f32 = 272.0;
/// How wide the rail is as a strip, at least: wider for a thread of many lanes.
const STRIP_WIDTH: f32 = 36.0;
/// How tall the rail's header is.
const HEADER_HEIGHT: f32 = 36.0;
/// How thick the graph's lines are.
const LINE_WIDTH: f32 = 2.0;
/// A dot's radius, and the open message's.
const DOT: f32 = 4.5;
const DOT_OPEN: f32 = 5.5;
/// The hues of the lanes after the first, which takes the accent: in turn, as `git log --graph`
/// colours its branches.
const LANE_HUES: [f32; 5] = [45.0, 150.0, 320.0, 85.0, 200.0];

/// The thread toggle: a branching graph.
static THREAD: Icon = Icon {
    name: "postden-thread",
    view: 16.0,
    body: r#"<circle cx="5" cy="3.5" r="1.6" stroke="currentColor" stroke-width="1.2" fill="none"/><circle cx="5" cy="12.5" r="1.6" stroke="currentColor" stroke-width="1.2" fill="none"/><circle cx="11.5" cy="8" r="1.6" stroke="currentColor" stroke-width="1.2" fill="none"/><path d="M5 5.1V10.9M5 5.6C5 7.6 11.5 5.6 11.5 6.4" stroke="currentColor" stroke-width="1.2" fill="none" stroke-linecap="round"/>"#,
};

/// Shrinking the rail: a chevron pointing left.
static SHRINK: Icon = Icon {
    name: "postden-shrink",
    view: 16.0,
    body: r#"<path d="M9.75 4.5L6.25 8L9.75 11.5" stroke="currentColor" stroke-width="1.2" fill="none" stroke-linecap="round" stroke-linejoin="round"/>"#,
};
/// Showing the rail whole: a chevron pointing right.
static GROW: Icon = Icon {
    name: "postden-grow",
    view: 16.0,
    body: r#"<path d="M6.25 4.5L9.75 8L6.25 11.5" stroke="currentColor" stroke-width="1.2" fill="none" stroke-linecap="round" stroke-linejoin="round"/>"#,
};

/// The open message, `content`, with its thread's rail beside it when it's in one and the rail
/// isn't hidden.
pub(crate) fn with_rail(s: &Mail, reader: &Reader, content: El, k: &Kit) -> El {
    let rail = s.settings.thread_rail;
    if !s.has_thread() || rail == ThreadRail::Hidden {
        return content;
    }
    row()
        .key("reader-with-rail")
        .transition(Transition::none())
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .child(graph_rail(s, reader, rail == ThreadRail::Open, k))
        .child(content)
}

/// Scrolls the rail to the open message's row, as one opened from it, or with N and P, can be
/// past what it shows.
pub(crate) fn reveal_open(s: &Mail, tx: &mut Tx) {
    if let Some(reader) = &s.reader {
        tx.scroll_into_view(rail_key(&message_key(&reader.message)));
    }
}

/// A rail row's key, by the message it's of.
fn rail_key(key: &crate::search::MessageKey) -> String {
    format!("rail-{}", row_key(key))
}

/// The toolbar's button for the rail, while the open message is in a thread: it shrinks the
/// rail to its graph or shows it whole, and with ⌥ hides it.
pub(crate) fn toggle(s: &Mail, k: &Kit) -> Option<El> {
    if !s.has_thread() {
        return None;
    }
    let command = Command::ToggleThread;
    let label = format!("{} ({})", command.title(s), command.hint(s));
    let open = s.settings.thread_rail == ThreadRail::Open;
    Some(
        IconButton::new(&THREAD)
            .variant(if open {
                IconButtonVariant::Neutral
            } else {
                IconButtonVariant::Ghost
            })
            .label(label.clone())
            .help(format!("{label}. ⌥-click to hide it"))
            .on_press_tx(|s: &mut Mail, tx| {
                if tx.mods().alt {
                    s.set_thread_rail(ThreadRail::Hidden);
                } else {
                    s.toggle_thread_rail();
                }
                tx.instant();
            })
            .build(k)
            .key("thread-toggle")
            .transition(Transition::none()),
    )
}

/// The rail: its header, then a row per message, open with their words or only the graph.
fn graph_rail(s: &Mail, reader: &Reader, open: bool, k: &Kit) -> El {
    let thread = &reader.thread;
    let graph_width = graph_width(thread);
    let width = if open {
        OPEN_WIDTH.max(graph_width + 120.0)
    } else {
        STRIP_WIDTH.max(graph_width)
    };
    let open_key = message_key(&reader.message);
    let newest_first = s.settings.thread_order == ThreadOrder::NewestFirst;
    let now = ZonedDateTime::now();
    let order: Box<dyn Iterator<Item = &Row>> = if newest_first {
        Box::new(thread.rows.iter().rev())
    } else {
        Box::new(thread.rows.iter())
    };
    let rows = order.map(|r| {
        let key = message_key(&r.message);
        let shown = Shown {
            open: key == open_key,
            mine: is_mine(s, &r.message),
            flipped: newest_first,
        };
        let cell = graph_cell(r, graph_width, shown, k);
        let line = if open {
            words(s, (thread, r), cell, shown, &now, k)
        } else {
            row().child(cell)
        };
        let help = describe(s, (thread, r), shown.mine, &now);
        line.key(rail_key(&key))
            .transition(Transition::none())
            .role("button")
            .label(help.clone())
            .h(ROW_HEIGHT)
            .w_full()
            .center_y()
            .fill(if shown.open {
                k.c.text_accent.alpha(0.12)
            } else {
                Color::TRANSPARENT
            })
            .hover(k.c.hover)
            .help(if open { String::new() } else { help })
            .on_press_tx(move |s: &mut Mail, tx: &mut Tx| {
                s.open_in_thread(&key);
                reveal_open(s, tx);
                tx.instant();
            })
    });
    col()
        .key("thread-rail")
        .transition(Transition::none())
        .w(width)
        .h_full()
        .min_h(0.0)
        .border_sides([0.0, 1.0, 0.0, 0.0], k.c.border_weak)
        .child(header(thread, open, k))
        .child(
            col()
                // One per message open, so each opens scrolled to its own row.
                .key(format!("thread-rail-rows-{}", row_key(&open_key)))
                .on_visible(|s: &mut Mail, visible, tx| {
                    if visible {
                        reveal_open(s, tx);
                    }
                })
                .w_full()
                .py(6.0)
                .children(rows)
                .scroll(ScrollSpec {
                    stick_bottom: false,
                })
                .grow(1.0)
                .min_h(0.0),
        )
}

/// How a row's message shows.
#[derive(Clone, Copy)]
struct Shown {
    /// It's the one open.
    open: bool,
    /// It's from the account it's in, as one in Sent is.
    mine: bool,
    /// The newest is at the top, so the graph is drawn upside down.
    flipped: bool,
}

/// The rail's header: how many messages and people, and the button that opens or shrinks it.
fn header(thread: &Thread, open: bool, k: &Kit) -> El {
    let (icon, label) = if open {
        (&SHRINK, "Shrink the thread to its graph")
    } else {
        (&GROW, "Show the thread")
    };
    let button = IconButton::new(icon)
        .variant(IconButtonVariant::GhostMuted)
        .label(label)
        .help(label)
        .on_press_tx(|s: &mut Mail, tx| {
            s.toggle_thread_rail();
            tx.instant();
        })
        .build(k)
        .key("thread-rail-toggle");
    let mut header = row()
        .w_full()
        .h(HEADER_HEIGHT)
        .px(if open { 12.0 } else { 0.0 })
        .gap(6.0)
        .center_y()
        .border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_weak);
    if open {
        let people = {
            let mut from: Vec<String> = thread
                .rows
                .iter()
                .map(|r| r.message.envelope.from.label().to_lowercase())
                .collect();
            from.sort();
            from.dedup();
            from.len()
        };
        let summary = format!(
            "{} messages · {} {}",
            thread.rows.len(),
            people,
            if people == 1 { "person" } else { "people" }
        );
        header = header.child(
            text(summary)
                .font(k.ty.small_medium)
                .color(k.c.text_muted)
                .truncate()
                .grow(1.0)
                .min_w(0.0),
        );
    } else {
        header = header.justify_center();
    }
    header.child(button)
}

/// A row's words beside its graph cell: who wrote it and when, then how it begins.
fn words(
    s: &Mail,
    (thread, r): (&Thread, &Row),
    cell: El,
    shown: Shown,
    now: &ZonedDateTime,
    k: &Kit,
) -> El {
    let e = &r.message.envelope;
    let name = if shown.mine {
        "You".to_owned()
    } else {
        e.from.label().to_owned()
    };
    let unread = !e.seen;
    let mut first = row().w_full().gap(6.0).center_y().child(
        text(name)
            .font(if unread || shown.open {
                k.ty.small_medium
            } else {
                k.ty.small
            })
            .color(k.c.text_base)
            .truncate()
            .shrink()
            .min_w(0.0),
    );
    if let Some(mailbox) = elsewhere(s, &r.message) {
        first = first.child(
            text(mailbox)
                .font(k.ty.small)
                .color(k.c.text_faint)
                .truncate()
                .shrink()
                .min_w(0.0),
        );
    }
    first = first.child(el().grow(1.0)).child(
        text(postden_app::format_date(e.date, now))
            .font(k.ty.small)
            .color(k.c.text_muted),
    );
    let second = gist(thread, r);
    row()
        .gap(0.0)
        .pr(10.0)
        .child(cell)
        .child(
            col().grow(1.0).min_w(0.0).gap(1.0).child(first).child(
                text(second)
                    .font(k.ty.small)
                    .color(k.c.text_muted)
                    .truncate()
                    .w_full(),
            ),
        )
        .children(unread.then(|| {
            el().size(6.0, 6.0)
                .ml(6.0)
                .radius(3.0)
                .fill(k.c.icon_accent)
        }))
}

/// What a row says to a screen reader, and in its tooltip as a strip: who, when, and how it
/// begins.
fn describe(s: &Mail, (thread, r): (&Thread, &Row), mine: bool, now: &ZonedDateTime) -> String {
    let e = &r.message.envelope;
    let who = if mine { "You" } else { e.from.label() };
    let mut parts = vec![who.to_owned(), postden_app::format_date(e.date, now)];
    if let Some(mailbox) = elsewhere(s, &r.message) {
        parts.push(mailbox);
    }
    parts.push(gist(thread, r));
    if !e.seen {
        parts.push("unread".into());
    }
    parts.join(" · ")
}

/// What a row says of its message under who wrote it: how its text begins, where that's
/// downloaded; else its subject, where it changed from the thread's; else who it went to, as a
/// side thread goes to fewer people.
fn gist(thread: &Thread, r: &Row) -> String {
    if let Some(snippet) = &r.snippet {
        return snippet.clone();
    }
    let e = &r.message.envelope;
    let first = thread
        .rows
        .first()
        .and_then(|f| f.message.envelope.subject.as_deref());
    let subject = e.subject.as_deref().map(topic);
    if let Some(subject) = subject.filter(|s| !s.is_empty() && Some(*s) != first.map(topic)) {
        return subject.to_owned();
    }
    let to: Vec<&str> =
        e.to.iter()
            .map(|a| {
                let you = a
                    .email
                    .as_deref()
                    .is_some_and(|email| email.eq_ignore_ascii_case(&r.message.account));
                if you { "you" } else { first_name(a.label()) }
            })
            .collect();
    if to.is_empty() {
        String::new()
    } else {
        format!("to {}", to.join(", "))
    }
}

/// A subject without the "Re:" and "Fwd:" replies and forwards put before it.
fn topic(subject: &str) -> &str {
    let mut rest = subject.trim();
    loop {
        let lower = rest.to_ascii_lowercase();
        let Some(prefix) = ["re:", "fwd:", "fw:", "aw:", "sv:"]
            .into_iter()
            .find(|p| lower.starts_with(p))
        else {
            return rest;
        };
        rest = rest.get(prefix.len()..).unwrap_or_default().trim_start();
    }
}

/// A person's first name, from how they're named: "Priya" of "Priya Natarajan", or an address
/// whole.
fn first_name(label: &str) -> &str {
    if label.contains('@') {
        return label;
    }
    label.split_whitespace().next().unwrap_or(label)
}

/// The mailbox a message of the thread is in, when that's worth saying: anywhere but an inbox
/// or Sent.
fn elsewhere(s: &Mail, m: &StoredMessage) -> Option<String> {
    let role = s
        .mailboxes
        .get(&m.account)?
        .iter()
        .find(|b| b.name == m.mailbox)
        .map_or(Role::Other, |b| b.role);
    (!matches!(role, Role::Inbox | Role::Sent)).then(|| s.mailbox_label(&m.account, &m.mailbox))
}

/// Whether `m` was sent from the account it's in: from its address, or in its Sent.
fn is_mine(s: &Mail, m: &StoredMessage) -> bool {
    let from_account = m
        .envelope
        .from
        .email
        .as_deref()
        .is_some_and(|email| email.eq_ignore_ascii_case(&m.account));
    from_account
        || s.mailboxes.get(&m.account).is_some_and(|list| {
            list.iter()
                .any(|b| b.name == m.mailbox && b.role == Role::Sent)
        })
}

/// A count of lanes as a length's factor.
fn lanes_f32(lanes: usize) -> f32 {
    f32::from(u16::try_from(lanes).unwrap_or(u16::MAX))
}

/// How wide the graph is, for all its lanes.
fn graph_width(thread: &Thread) -> f32 {
    GRAPH_PADDING.mul_add(
        2.0,
        lanes_f32(thread.lanes.clamp(1, MAX_LANES)) * LANE_WIDTH,
    )
}

/// Where lane `lane`'s line runs, across.
fn lane_x(lane: usize) -> f32 {
    let lane = lane.min(MAX_LANES - 1);
    GRAPH_PADDING + LANE_WIDTH.mul_add(lanes_f32(lane), LANE_WIDTH / 2.0)
}

/// Lane `lane`'s colour: the accent for the first, as the thread's main line.
fn lane_color(lane: usize, k: &Kit) -> Color {
    lane.checked_sub(1)
        .and_then(|i| LANE_HUES.get(i % LANE_HUES.len()))
        .map_or(k.c.icon_accent, |&hue| {
            oklch(if k.dark { 0.74 } else { 0.62 }, 0.14, hue)
        })
}

/// A row's part of the graph: the lines through it and its message's dot.
fn graph_cell(message: &Row, width: f32, shown: Shown, k: &Kit) -> El {
    let mid = ROW_HEIGHT / 2.0;
    // Drawn oldest first, from the top down; upside down when the newest is at the top.
    let flip = |down: f32| {
        if shown.flipped {
            ROW_HEIGHT - down
        } else {
            down
        }
    };
    let mut shape = Shape::new();
    for segment in &message.segments {
        let (lane, dashed, points) = match *segment {
            Segment::Through { lane, dashed } => {
                let across = lane_x(lane);
                (lane, dashed, vec![[across, 0.0], [across, ROW_HEIGHT]])
            }
            Segment::Top { lane, dashed } => {
                let across = lane_x(lane);
                (lane, dashed, vec![[across, 0.0], [across, mid]])
            }
            Segment::Bottom { lane, dashed } => {
                let across = lane_x(lane);
                (lane, dashed, vec![[across, mid], [across, ROW_HEIGHT]])
            }
            Segment::Fork { from, to, dashed } => {
                let (start, end) = (lane_x(from), lane_x(to));
                let bend = (ROW_HEIGHT - mid) * 0.6;
                (
                    to,
                    dashed,
                    cubic(
                        [start, mid],
                        [start, mid + bend],
                        [end, ROW_HEIGHT - bend],
                        [end, ROW_HEIGHT],
                    ),
                )
            }
        };
        let points: Vec<[f32; 2]> = points
            .into_iter()
            .map(|[across, down]| [across, flip(down)])
            .collect();
        let color = lane_color(lane, k);
        if dashed {
            for dash in dashes(&points) {
                shape = shape.stroke(dash, LINE_WIDTH, color);
            }
        } else {
            shape = shape.stroke(points, LINE_WIDTH, color);
        }
    }
    let center = [lane_x(message.lane), mid];
    let color = lane_color(message.lane, k);
    let radius = if shown.open { DOT_OPEN } else { DOT };
    if shown.open {
        shape = shape.fill(circle(center, radius + 4.0), color.alpha(0.22));
    }
    if shown.mine {
        // Hollow, as `git log --graph` would draw a commit of one's own.
        shape = shape.fill(circle(center, radius), k.c.bg_base).stroke_with(
            circle(center, radius - 1.0),
            true,
            LINE_WIDTH,
            rune::LineCap::Round,
            color,
        );
    } else {
        shape = shape
            .fill(circle(center, radius + 1.5), k.c.bg_base)
            .fill(circle(center, radius), color);
    }
    // Unread: a hole in a filled dot, or a filled centre in a hollow one.
    if !message.message.envelope.seen {
        let mark = if shown.mine { color } else { k.c.bg_base };
        shape = shape.fill(circle(center, 1.8), mark);
    }
    canvas(shape).size(width, ROW_HEIGHT).a11y_hidden()
}

/// A point along the line from `from` to `to`, `t` of the way.
fn lerp(from: [f32; 2], to: [f32; 2], t: f32) -> [f32; 2] {
    let [x0, y0] = from;
    let [x1, y1] = to;
    [(x1 - x0).mul_add(t, x0), (y1 - y0).mul_add(t, y0)]
}

/// The points along a cubic Bézier curve from `start` to `end`, bent toward `c1` then `c2`,
/// close enough to draw it.
fn cubic(start: [f32; 2], c1: [f32; 2], c2: [f32; 2], end: [f32; 2]) -> Vec<[f32; 2]> {
    const STEPS: u8 = 16;
    (0..=STEPS)
        .map(|i| {
            let t = f32::from(i) / f32::from(STEPS);
            // De Casteljau: the curve's point is a blend of blends.
            let (ab, bc, cd) = (lerp(start, c1, t), lerp(c1, c2, t), lerp(c2, end, t));
            lerp(lerp(ab, bc, t), lerp(bc, cd, t), t)
        })
        .collect()
}

/// How long a dash is, and the gap after it, in points.
const DASH: f32 = 3.0;

/// `points` cut into dashes, [`DASH`] long with as long a gap between.
fn dashes(points: &[[f32; 2]]) -> Vec<Vec<[f32; 2]>> {
    let mut dashes = Vec::new();
    let mut current: Vec<[f32; 2]> = Vec::new();
    // Whether the walk is in a dash or a gap, and how much of it is left.
    let (mut drawing, mut left) = (true, DASH);
    for (&from, &to) in points.iter().zip(points.iter().skip(1)) {
        let ([x0, y0], [x1, y1]) = (from, to);
        let length = (x1 - x0).hypot(y1 - y0);
        if length <= f32::EPSILON {
            continue;
        }
        let mut along = 0.0_f32;
        loop {
            let rest = length - along;
            if rest <= f32::EPSILON {
                break;
            }
            let step = left.min(rest);
            if drawing {
                if current.is_empty() {
                    current.push(lerp(from, to, along / length));
                }
                current.push(lerp(from, to, (along + step) / length));
            }
            along += step;
            left -= step;
            if left <= f32::EPSILON {
                if drawing && current.len() >= 2 {
                    dashes.push(std::mem::take(&mut current));
                }
                current.clear();
                drawing = !drawing;
                left = DASH;
            }
        }
    }
    if current.len() >= 2 {
        dashes.push(current);
    }
    dashes
}

/// A circle as a polygon, for a dot.
fn circle([x, y]: [f32; 2], radius: f32) -> Vec<[f32; 2]> {
    const SIDES: u8 = 20;
    (0..SIDES)
        .map(|i| {
            let angle = f32::from(i) / f32::from(SIDES) * std::f32::consts::TAU;
            [
                radius.mul_add(angle.cos(), x),
                radius.mul_add(angle.sin(), y),
            ]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dashed_line_is_cut_into_dashes_along_its_length() {
        let dashes = dashes(&[[0.0, 0.0], [0.0, 15.0]]);
        // On 0-3, 6-9, 12-15.
        assert_eq!(dashes.len(), 3);
        assert_eq!(dashes[1].first(), Some(&[0.0, 6.0]));
        assert_eq!(dashes[2].last(), Some(&[0.0, 15.0]));
    }

    #[test]
    fn replies_and_forwards_share_their_topic() {
        assert_eq!(topic("Re: RE: Fwd: cabin weekend"), "cabin weekend");
        assert_eq!(topic("cabin weekend"), "cabin weekend");
        assert_eq!(topic("Reply needed"), "Reply needed");
    }

    #[test]
    fn a_curve_starts_and_ends_where_asked() {
        let points = cubic([0.0, 0.0], [0.0, 5.0], [10.0, 5.0], [10.0, 10.0]);
        assert_eq!(points.first(), Some(&[0.0, 0.0]));
        assert_eq!(points.last(), Some(&[10.0, 10.0]));
    }

    #[test]
    fn a_thread_branching_widely_keeps_the_rail_narrow() {
        let wide = Thread {
            rows: Vec::new(),
            lanes: 100,
        };
        assert!(
            graph_width(&wide) <= GRAPH_PADDING.mul_add(2.0, lanes_f32(MAX_LANES) * LANE_WIDTH)
        );
        assert!((lane_x(99) - lane_x(MAX_LANES - 1)).abs() < f32::EPSILON);
    }
}
