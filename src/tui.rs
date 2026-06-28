use std::{
    collections::{HashMap, VecDeque},
    io,
    time::{Duration, Instant},
};

use anyhow::Context;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols,
    text::{Line, Span},
    widgets::{Axis, Block, Borders, Cell, Chart, Dataset, GraphType, Paragraph, Row, Table},
};
use tokio::sync::watch;

use crate::{
    domain::{BboTick, Fixed, Venue},
    state::{BboSnapshot, SharedBboState},
};

#[derive(Debug, Default)]
struct TuiSelection {
    bbo_market: usize,
    bbo_venue: usize,
    spread_market: usize,
    spread_venue_a: usize,
    spread_venue_b: usize,
    focus: FocusPanel,
    spread_leg: SpreadLeg,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum FocusPanel {
    #[default]
    Bbo,
    Spread,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum SpreadLeg {
    #[default]
    First,
    Second,
}

struct TerminalGuard;

const SPREAD_HISTORY_WINDOW: Duration = Duration::from_secs(90);
const SPREAD_SAMPLE_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SpreadKey {
    market: String,
    venue_a: Venue,
    venue_b: Venue,
}

#[derive(Debug, Clone, Copy)]
struct SpreadSample {
    at: Instant,
    a_sell_b_buy: f64,
    b_sell_a_buy: f64,
}

#[derive(Debug, Default)]
struct SpreadHistory {
    samples: HashMap<SpreadKey, VecDeque<SpreadSample>>,
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

pub fn run(
    state: SharedBboState,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
    refresh_ms: u64,
) -> anyhow::Result<()> {
    enable_raw_mode().context("enable terminal raw mode")?;
    execute!(io::stdout(), EnterAlternateScreen).context("enter alternate screen")?;
    let _guard = TerminalGuard;

    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend).context("create terminal")?;
    terminal.clear()?;

    let mut selection = TuiSelection::default();
    let mut spread_history = SpreadHistory::default();
    let mut last_draw = Instant::now() - Duration::from_millis(refresh_ms);

    loop {
        if *shutdown_rx.borrow() {
            break;
        }

        let snapshot = read_snapshot(&state);
        selection.clamp(&snapshot);
        spread_history.record_selected(&snapshot, &selection);

        if last_draw.elapsed() >= Duration::from_millis(refresh_ms) {
            terminal.draw(|frame| draw(frame, &snapshot, &selection, &spread_history))?;
            last_draw = Instant::now();
        }

        if event::poll(Duration::from_millis(50))? {
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }

            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => {
                    let _ = shutdown_tx.send(true);
                    break;
                }
                KeyCode::Tab => selection.toggle_focus(),
                KeyCode::Left => selection.prev_market(&snapshot),
                KeyCode::Right => selection.next_market(&snapshot),
                KeyCode::Up => selection.prev_venue(&snapshot),
                KeyCode::Down => selection.next_venue(&snapshot),
                KeyCode::Char('1') => {
                    selection.focus = FocusPanel::Spread;
                    selection.spread_leg = SpreadLeg::First;
                }
                KeyCode::Char('2') => {
                    selection.focus = FocusPanel::Spread;
                    selection.spread_leg = SpreadLeg::Second;
                }
                _ => {}
            }
        }
    }

    terminal.show_cursor()?;
    Ok(())
}

fn read_snapshot(state: &SharedBboState) -> BboSnapshot {
    state
        .read()
        .map(|state| state.snapshot())
        .unwrap_or_default()
}

fn draw(
    frame: &mut Frame<'_>,
    snapshot: &BboSnapshot,
    selection: &TuiSelection,
    spread_history: &SpreadHistory,
) {
    let areas = main_areas(frame.area());

    draw_header(frame, areas.header);
    draw_bbo_panel(frame, areas.bbo, snapshot, selection);
    draw_spread_panel(frame, areas.spread, snapshot, selection, spread_history);
    draw_footer(frame, areas.footer, selection);
}

#[derive(Debug, Clone, Copy)]
struct MainAreas {
    header: Rect,
    bbo: Rect,
    spread: Rect,
    footer: Rect,
}

fn main_areas(area: Rect) -> MainAreas {
    let header_h = if area.height >= 8 { 3 } else { 1 };
    let footer_h = if area.height >= 8 { 3 } else { 1 };
    let body_h = area.height.saturating_sub(header_h + footer_h);
    let bbo_h = bbo_height(body_h);
    let spread_h = body_h.saturating_sub(bbo_h);

    let header = Rect::new(area.x, area.y, area.width, header_h.min(area.height));
    let bbo_y = area.y.saturating_add(header_h);
    let bbo = Rect::new(area.x, bbo_y, area.width, bbo_h);
    let spread_y = bbo_y.saturating_add(bbo_h);
    let spread = Rect::new(area.x, spread_y, area.width, spread_h);
    let footer_y = area.y.saturating_add(area.height.saturating_sub(footer_h));
    let footer = Rect::new(area.x, footer_y, area.width, footer_h.min(area.height));

    MainAreas {
        header,
        bbo,
        spread,
        footer,
    }
}

fn bbo_height(body_h: u16) -> u16 {
    if body_h == 0 {
        0
    } else if body_h >= 28 {
        10
    } else if body_h >= 18 {
        8
    } else if body_h >= 12 {
        7
    } else {
        (body_h / 2).max(4).min(body_h)
    }
}

fn draw_header(frame: &mut Frame<'_>, area: Rect) {
    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            "Exchange Spread Log",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled("BBO monitor", Style::default().fg(Color::Gray)),
    ]))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(header, area);
}

fn draw_bbo_panel(
    frame: &mut Frame<'_>,
    area: Rect,
    snapshot: &BboSnapshot,
    selection: &TuiSelection,
) {
    let market = selected_market(snapshot, selection.bbo_market);
    let title = market
        .map(|market| format!("BBO by venue: {market}"))
        .unwrap_or_else(|| "BBO by venue: waiting for data".to_string());
    let rows = market
        .map(|market| snapshot.rows_for_market(market))
        .unwrap_or_default()
        .into_iter()
        .enumerate()
        .map(|(index, tick)| {
            let style = if selection.focus == FocusPanel::Bbo && index == selection.bbo_venue {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            Row::new(vec![
                Cell::from(tick.venue.as_str()),
                Cell::from(level_price(tick.bid.as_ref())),
                Cell::from(level_price(tick.ask.as_ref())),
                Cell::from(level_size(tick.bid.as_ref())),
                Cell::from(level_size(tick.ask.as_ref())),
                Cell::from(tick_spread(tick)),
                Cell::from(tick_age(tick)),
            ])
            .style(style)
        });

    let table = Table::new(
        rows,
        [
            Constraint::Length(13),
            Constraint::Length(14),
            Constraint::Length(14),
            Constraint::Length(12),
            Constraint::Length(12),
            Constraint::Length(12),
            Constraint::Min(8),
        ],
    )
    .header(
        Row::new([
            "venue", "bid", "ask", "bid size", "ask size", "spread", "age",
        ])
        .style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
    )
    .column_spacing(2)
    .block(panel_block(&title, selection.focus == FocusPanel::Bbo));

    frame.render_widget(table, area);
}

fn draw_spread_panel(
    frame: &mut Frame<'_>,
    area: Rect,
    snapshot: &BboSnapshot,
    selection: &TuiSelection,
    spread_history: &SpreadHistory,
) {
    let market = selected_market(snapshot, selection.spread_market);
    let venue_a = selected_venue(snapshot, selection.spread_venue_a);
    let venue_b = selected_venue(snapshot, selection.spread_venue_b);
    let title = match (market, venue_a, venue_b) {
        (Some(market), Some(a), Some(b)) => {
            format!("Spread: {} vs {} / {market}", a.as_str(), b.as_str())
        }
        _ => "Spread: waiting for two venues and a shared market".to_string(),
    };

    let block = panel_block(&title, selection.focus == FocusPanel::Spread);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let summary_h = spread_summary_height(inner.height);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(summary_h), Constraint::Min(3)])
        .split(inner);

    if let (Some(market), Some(venue_a), Some(venue_b)) = (market, venue_a, venue_b) {
        let first = snapshot.find(venue_a, market);
        let second = snapshot.find(venue_b, market);
        draw_spread_summary_table(frame, chunks[0], venue_a, first, venue_b, second);
    } else {
        frame.render_widget(Paragraph::new("No comparable BBO yet."), chunks[0]);
    }

    if let Some(key) = selected_spread_key(snapshot, selection) {
        draw_spread_chart(frame, chunks[1], spread_history, &key);
    } else {
        let empty = Paragraph::new("Waiting for comparable spread samples...");
        frame.render_widget(empty, chunks[1]);
    }
}

fn spread_summary_height(inner_h: u16) -> u16 {
    if inner_h >= 18 {
        5
    } else if inner_h >= 12 {
        5
    } else if inner_h >= 8 {
        4
    } else {
        inner_h.min(3)
    }
}

fn draw_footer(frame: &mut Frame<'_>, area: Rect, selection: &TuiSelection) {
    let focus = match selection.focus {
        FocusPanel::Bbo => "BBO",
        FocusPanel::Spread => "Spread",
    };
    let leg = match selection.spread_leg {
        SpreadLeg::First => "1st venue",
        SpreadLeg::Second => "2nd venue",
    };
    let footer = Paragraph::new(Line::from(vec![
        Span::styled("q", Style::default().fg(Color::Yellow)),
        Span::raw(" quit  "),
        Span::styled("tab", Style::default().fg(Color::Yellow)),
        Span::raw(format!(" focus={focus}  ")),
        Span::styled("left/right", Style::default().fg(Color::Yellow)),
        Span::raw(" market  "),
        Span::styled("up/down", Style::default().fg(Color::Yellow)),
        Span::raw(format!(" venue ({leg})  ")),
        Span::styled("1/2", Style::default().fg(Color::Yellow)),
        Span::raw(" spread venue leg  "),
        Span::styled("chart", Style::default().fg(Color::Yellow)),
        Span::raw(" last 90s"),
    ]))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(footer, area);
}

fn panel_block(title: &str, focused: bool) -> Block<'_> {
    let color = if focused {
        Color::Cyan
    } else {
        Color::DarkGray
    };
    Block::default()
        .title(title.to_string())
        .borders(Borders::ALL)
        .border_style(Style::default().fg(color))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SpreadSummaryRow {
    direction: String,
    formula: String,
    value: String,
    bp: String,
    meaning: String,
}

fn spread_summary_rows(
    venue_a: Venue,
    first: Option<&BboTick>,
    venue_b: Venue,
    second: Option<&BboTick>,
) -> Vec<SpreadSummaryRow> {
    let data_status = missing_data_status(venue_a, first, venue_b, second);
    vec![
        SpreadSummaryRow {
            direction: "green A->B".to_string(),
            formula: format!("{} bid - {} ask", venue_a.as_str(), venue_b.as_str()),
            value: cross_spread(first, second),
            bp: spread_bp(first, second),
            meaning: data_status
                .clone()
                .unwrap_or_else(|| "sell A, buy B; >0 before fees".to_string()),
        },
        SpreadSummaryRow {
            direction: "magenta B->A".to_string(),
            formula: format!("{} bid - {} ask", venue_b.as_str(), venue_a.as_str()),
            value: cross_spread(second, first),
            bp: spread_bp(second, first),
            meaning: data_status
                .clone()
                .unwrap_or_else(|| "sell B, buy A; >0 before fees".to_string()),
        },
        SpreadSummaryRow {
            direction: "mid diff".to_string(),
            formula: format!("{} mid - {} mid", venue_a.as_str(), venue_b.as_str()),
            value: diff_fixed(
                first.and_then(|tick| tick.mid),
                second.and_then(|tick| tick.mid),
            ),
            bp: "-".to_string(),
            meaning: data_status
                .unwrap_or_else(|| "-90s left -> now right; zero = break-even".to_string()),
        },
    ]
}

fn missing_data_status(
    venue_a: Venue,
    first: Option<&BboTick>,
    venue_b: Venue,
    second: Option<&BboTick>,
) -> Option<String> {
    match (first.is_some(), second.is_some()) {
        (true, true) => None,
        (false, true) => Some(format!("missing {} BBO for this market", venue_a.as_str())),
        (true, false) => Some(format!("missing {} BBO for this market", venue_b.as_str())),
        (false, false) => Some("missing both venues for this market".to_string()),
    }
}

fn draw_spread_summary_table(
    frame: &mut Frame<'_>,
    area: Rect,
    venue_a: Venue,
    first: Option<&BboTick>,
    venue_b: Venue,
    second: Option<&BboTick>,
) {
    let rows = spread_summary_rows(venue_a, first, venue_b, second)
        .into_iter()
        .map(|row| {
            let style = if row.direction.starts_with("green") {
                Style::default().fg(Color::Green)
            } else if row.direction.starts_with("magenta") {
                Style::default().fg(Color::Magenta)
            } else {
                Style::default().fg(Color::Gray)
            };
            Row::new(vec![
                Cell::from(row.direction),
                Cell::from(row.formula),
                Cell::from(row.value),
                Cell::from(row.bp),
                Cell::from(row.meaning),
            ])
            .style(style)
        });

    let table = Table::new(
        rows,
        [
            Constraint::Length(15),
            Constraint::Length(34),
            Constraint::Length(12),
            Constraint::Length(10),
            Constraint::Min(28),
        ],
    )
    .header(
        Row::new(["line", "formula", "now", "profit bp", "meaning"]).style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
    )
    .column_spacing(2);

    frame.render_widget(table, area);
}

fn draw_spread_chart(
    frame: &mut Frame<'_>,
    area: Rect,
    spread_history: &SpreadHistory,
    key: &SpreadKey,
) {
    let Some(samples) = spread_history.samples.get(key) else {
        let empty = Paragraph::new("Waiting for spread history...");
        frame.render_widget(empty, area);
        return;
    };

    if samples.len() < 2 {
        let empty = Paragraph::new("Collecting spread samples...");
        frame.render_widget(empty, area);
        return;
    }

    let now = Instant::now();
    let a_to_b = samples
        .iter()
        .map(|sample| {
            (
                -(now.duration_since(sample.at).as_secs_f64()),
                sample.a_sell_b_buy,
            )
        })
        .collect::<Vec<_>>();
    let b_to_a = samples
        .iter()
        .map(|sample| {
            (
                -(now.duration_since(sample.at).as_secs_f64()),
                sample.b_sell_a_buy,
            )
        })
        .collect::<Vec<_>>();
    let zero_line = vec![(-SPREAD_HISTORY_WINDOW.as_secs_f64(), 0.0), (0.0, 0.0)];
    let (y_min, y_max) = chart_bounds(&a_to_b, &b_to_a);

    let datasets = vec![
        Dataset::default()
            .name(format!(
                "{} bid - {} ask",
                key.venue_a.as_str(),
                key.venue_b.as_str()
            ))
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(Color::Green))
            .data(&a_to_b),
        Dataset::default()
            .name(format!(
                "{} bid - {} ask",
                key.venue_b.as_str(),
                key.venue_a.as_str()
            ))
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(Color::Magenta))
            .data(&b_to_a),
        Dataset::default()
            .name("zero")
            .marker(symbols::Marker::Dot)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(Color::DarkGray))
            .data(&zero_line),
    ];

    let chart = Chart::new(datasets)
        .x_axis(
            Axis::default()
                .title("seconds ago")
                .style(Style::default().fg(Color::Gray))
                .bounds([-SPREAD_HISTORY_WINDOW.as_secs_f64(), 0.0])
                .labels(vec![Span::raw("-90s"), Span::raw("-45s"), Span::raw("now")]),
        )
        .y_axis(
            Axis::default()
                .title("spread")
                .style(Style::default().fg(Color::Gray))
                .bounds([y_min, y_max])
                .labels(vec![
                    Span::raw(format_chart_value(y_min)),
                    Span::raw("0"),
                    Span::raw(format_chart_value(y_max)),
                ]),
        );

    frame.render_widget(chart, area);
}

fn chart_bounds(first: &[(f64, f64)], second: &[(f64, f64)]) -> (f64, f64) {
    let mut min = 0.0_f64;
    let mut max = 0.0_f64;
    for value in first
        .iter()
        .chain(second.iter())
        .map(|(_, value)| *value)
        .filter(|value| value.is_finite())
    {
        min = min.min(value);
        max = max.max(value);
    }

    if (max - min).abs() < f64::EPSILON {
        return (-1.0, 1.0);
    }

    let padding = ((max - min) * 0.12).max(0.0001);
    (min - padding, max + padding)
}

fn format_chart_value(value: f64) -> String {
    if value.abs() >= 10.0 {
        format!("{value:.2}")
    } else if value.abs() >= 1.0 {
        format!("{value:.4}")
    } else {
        format!("{value:.6}")
    }
}

fn cross_spread(sell_venue: Option<&BboTick>, buy_venue: Option<&BboTick>) -> String {
    cross_spread_value(sell_venue, buy_venue)
        .map(|spread| spread.to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn cross_spread_value(sell_venue: Option<&BboTick>, buy_venue: Option<&BboTick>) -> Option<Fixed> {
    let Some(sell_bid) = sell_venue.and_then(|tick| tick.bid.as_ref()) else {
        return None;
    };
    let Some(buy_ask) = buy_venue.and_then(|tick| tick.ask.as_ref()) else {
        return None;
    };

    sell_bid.price.checked_sub(buy_ask.price).ok()
}

fn spread_bp(sell_venue: Option<&BboTick>, buy_venue: Option<&BboTick>) -> String {
    let Some(spread) = cross_spread_value(sell_venue, buy_venue) else {
        return "-".to_string();
    };
    let Some(buy_ask) = buy_venue.and_then(|tick| tick.ask.as_ref()) else {
        return "-".to_string();
    };

    format_bp(spread.to_f64(), buy_ask.price.to_f64())
}

fn format_bp(numerator: f64, denominator: f64) -> String {
    if !numerator.is_finite() || !denominator.is_finite() || denominator.abs() < f64::EPSILON {
        return "-".to_string();
    }
    format!("{:.2}", numerator / denominator * 10_000.0)
}

fn diff_fixed(lhs: Option<Fixed>, rhs: Option<Fixed>) -> String {
    let (Some(lhs), Some(rhs)) = (lhs, rhs) else {
        return "-".to_string();
    };
    lhs.checked_sub(rhs)
        .map(|diff| diff.to_string())
        .unwrap_or_else(|_| "-".to_string())
}

fn level_price(level: Option<&crate::domain::BestLevel>) -> String {
    level
        .map(|level| level.price.to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn level_size(level: Option<&crate::domain::BestLevel>) -> String {
    level
        .map(|level| level.size.to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn tick_spread(tick: &BboTick) -> String {
    tick.spread
        .map(|spread| spread.to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn tick_age(tick: &BboTick) -> String {
    let now_ns = crate::ingest::time::unix_time_ns();
    let age_ms = now_ns.saturating_sub(tick.recv_ts_ns) / 1_000_000;
    if age_ms < 1_000 {
        format!("{age_ms}ms")
    } else {
        format!("{:.1}s", age_ms as f64 / 1_000.0)
    }
}

fn selected_market(snapshot: &BboSnapshot, index: usize) -> Option<&str> {
    snapshot.markets.get(index).map(String::as_str)
}

fn selected_venue(snapshot: &BboSnapshot, index: usize) -> Option<Venue> {
    snapshot.venues.get(index).copied()
}

fn selected_spread_key(snapshot: &BboSnapshot, selection: &TuiSelection) -> Option<SpreadKey> {
    Some(SpreadKey {
        market: selected_market(snapshot, selection.spread_market)?.to_string(),
        venue_a: selected_venue(snapshot, selection.spread_venue_a)?,
        venue_b: selected_venue(snapshot, selection.spread_venue_b)?,
    })
}

impl SpreadHistory {
    fn record_selected(&mut self, snapshot: &BboSnapshot, selection: &TuiSelection) {
        let Some(key) = selected_spread_key(snapshot, selection) else {
            return;
        };
        if key.venue_a == key.venue_b {
            return;
        }

        let first = snapshot.find(key.venue_a, &key.market);
        let second = snapshot.find(key.venue_b, &key.market);
        let Some(a_sell_b_buy) = cross_spread_value(first, second).map(Fixed::to_f64) else {
            return;
        };
        let Some(b_sell_a_buy) = cross_spread_value(second, first).map(Fixed::to_f64) else {
            return;
        };

        let now = Instant::now();
        let samples = self.samples.entry(key).or_default();
        if samples
            .back()
            .is_some_and(|sample| now.duration_since(sample.at) < SPREAD_SAMPLE_INTERVAL)
        {
            return;
        }

        samples.push_back(SpreadSample {
            at: now,
            a_sell_b_buy,
            b_sell_a_buy,
        });
        while samples
            .front()
            .is_some_and(|sample| now.duration_since(sample.at) > SPREAD_HISTORY_WINDOW)
        {
            samples.pop_front();
        }
    }
}

impl TuiSelection {
    fn clamp(&mut self, snapshot: &BboSnapshot) {
        self.bbo_market = clamp_index(self.bbo_market, snapshot.markets.len());
        self.spread_market = clamp_index(self.spread_market, snapshot.markets.len());
        self.bbo_venue = clamp_index(
            self.bbo_venue,
            selected_market(snapshot, self.bbo_market)
                .map(|market| snapshot.rows_for_market(market).len())
                .unwrap_or_default(),
        );
        self.spread_venue_a = clamp_index(self.spread_venue_a, snapshot.venues.len());
        self.spread_venue_b = clamp_index(self.spread_venue_b, snapshot.venues.len());
        if snapshot.venues.len() > 1 && self.spread_venue_a == self.spread_venue_b {
            self.spread_venue_b = (self.spread_venue_a + 1) % snapshot.venues.len();
        }
    }

    fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            FocusPanel::Bbo => FocusPanel::Spread,
            FocusPanel::Spread => FocusPanel::Bbo,
        };
    }

    fn prev_market(&mut self, snapshot: &BboSnapshot) {
        let len = snapshot.markets.len();
        if len == 0 {
            return;
        }
        match self.focus {
            FocusPanel::Bbo => self.bbo_market = wrap_prev(self.bbo_market, len),
            FocusPanel::Spread => self.spread_market = wrap_prev(self.spread_market, len),
        }
    }

    fn next_market(&mut self, snapshot: &BboSnapshot) {
        let len = snapshot.markets.len();
        if len == 0 {
            return;
        }
        match self.focus {
            FocusPanel::Bbo => self.bbo_market = (self.bbo_market + 1) % len,
            FocusPanel::Spread => self.spread_market = (self.spread_market + 1) % len,
        }
    }

    fn prev_venue(&mut self, snapshot: &BboSnapshot) {
        match self.focus {
            FocusPanel::Bbo => {
                let len = selected_market(snapshot, self.bbo_market)
                    .map(|market| snapshot.rows_for_market(market).len())
                    .unwrap_or_default();
                if len > 0 {
                    self.bbo_venue = wrap_prev(self.bbo_venue, len);
                }
            }
            FocusPanel::Spread => self.prev_spread_venue(snapshot),
        }
    }

    fn next_venue(&mut self, snapshot: &BboSnapshot) {
        match self.focus {
            FocusPanel::Bbo => {
                let len = selected_market(snapshot, self.bbo_market)
                    .map(|market| snapshot.rows_for_market(market).len())
                    .unwrap_or_default();
                if len > 0 {
                    self.bbo_venue = (self.bbo_venue + 1) % len;
                }
            }
            FocusPanel::Spread => self.next_spread_venue(snapshot),
        }
    }

    fn prev_spread_venue(&mut self, snapshot: &BboSnapshot) {
        let len = snapshot.venues.len();
        if len == 0 {
            return;
        }
        match self.spread_leg {
            SpreadLeg::First => self.spread_venue_a = wrap_prev(self.spread_venue_a, len),
            SpreadLeg::Second => self.spread_venue_b = wrap_prev(self.spread_venue_b, len),
        }
    }

    fn next_spread_venue(&mut self, snapshot: &BboSnapshot) {
        let len = snapshot.venues.len();
        if len == 0 {
            return;
        }
        match self.spread_leg {
            SpreadLeg::First => self.spread_venue_a = (self.spread_venue_a + 1) % len,
            SpreadLeg::Second => self.spread_venue_b = (self.spread_venue_b + 1) % len,
        }
    }
}

fn clamp_index(index: usize, len: usize) -> usize {
    if len == 0 { 0 } else { index.min(len - 1) }
}

fn wrap_prev(index: usize, len: usize) -> usize {
    if index == 0 { len - 1 } else { index - 1 }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use crate::{
        domain::{BboTick, BestLevel, Fixed, MarketRef, SourceKind, Venue},
        pipeline::normalizer,
        state::BboStore,
        tui::{SpreadHistory, TuiSelection, cross_spread, main_areas, spread_summary_rows},
    };

    fn tick(venue: Venue, bid: &str, ask: &str) -> BboTick {
        normalizer::normalize(BboTick::new(
            venue,
            MarketRef::new("ETH", Some("ETH".to_string())),
            123,
            Some(456),
            None,
            Some(BestLevel::new(
                Fixed::from_str(bid).unwrap(),
                Fixed::from_str("1").unwrap(),
                None,
            )),
            Some(BestLevel::new(
                Fixed::from_str(ask).unwrap(),
                Fixed::from_str("2").unwrap(),
                None,
            )),
            SourceKind::Bbo,
        ))
    }

    #[test]
    fn calculates_cross_spread() {
        let first = tick(Venue::Hyperliquid, "101", "102");
        let second = tick(Venue::Lighter, "100", "100.5");

        assert_eq!(cross_spread(Some(&first), Some(&second)), "0.5");
        assert_eq!(cross_spread(Some(&second), Some(&first)), "-2");
    }

    #[test]
    fn calculates_spread_profit_bp_from_buy_ask() {
        let first = tick(Venue::Hyperliquid, "101", "102");
        let second = tick(Venue::Lighter, "100", "100.5");

        assert_eq!(super::spread_bp(Some(&first), Some(&second)), "49.75");
        assert_eq!(super::spread_bp(Some(&second), Some(&first)), "-196.08");
    }

    #[test]
    fn selection_clamps_to_available_snapshot() {
        let mut store = BboStore::default();
        store.update(tick(Venue::Hyperliquid, "101", "102"));
        let snapshot = store.snapshot();
        let mut selection = TuiSelection {
            bbo_market: 99,
            bbo_venue: 99,
            spread_market: 99,
            spread_venue_a: 99,
            spread_venue_b: 99,
            ..TuiSelection::default()
        };

        selection.clamp(&snapshot);
        assert_eq!(selection.bbo_market, 0);
        assert_eq!(selection.bbo_venue, 0);
        assert_eq!(selection.spread_market, 0);
        assert_eq!(selection.spread_venue_a, 0);
        assert_eq!(selection.spread_venue_b, 0);
    }

    #[test]
    fn spread_history_keeps_recent_samples_for_selected_pair() {
        let mut store = BboStore::default();
        store.update(tick(Venue::Hyperliquid, "101", "102"));
        store.update(tick(Venue::Lighter, "100", "100.5"));
        let snapshot = store.snapshot();
        let mut selection = TuiSelection {
            spread_market: 0,
            spread_venue_a: 0,
            spread_venue_b: 1,
            ..TuiSelection::default()
        };
        selection.clamp(&snapshot);

        let mut history = SpreadHistory::default();
        history.record_selected(&snapshot, &selection);

        let key = super::selected_spread_key(&snapshot, &selection).unwrap();
        let samples = history.samples.get(&key).unwrap();
        assert_eq!(samples.len(), 1);
        assert!(samples[0].a_sell_b_buy.is_finite());
        assert!(samples[0].b_sell_a_buy.is_finite());
    }

    #[test]
    fn spread_summary_explains_chart_rows() {
        let first = tick(Venue::Hyperliquid, "101", "102");
        let second = tick(Venue::Lighter, "100", "100.5");

        let rows = spread_summary_rows(
            Venue::Hyperliquid,
            Some(&first),
            Venue::Lighter,
            Some(&second),
        );

        assert_eq!(rows[0].bp, "49.75");
        assert_eq!(rows[1].bp, "-196.08");
        assert_eq!(rows[2].bp, "-");

        let joined = rows
            .iter()
            .map(|row| {
                format!(
                    "{} {} {} {} {}",
                    row.direction, row.formula, row.value, row.bp, row.meaning
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(joined.contains("hyperliquid bid - lighter ask"));
        assert!(joined.contains("zero = break-even"));
        assert!(joined.contains("-90s"));
    }

    #[test]
    fn vertical_layout_keeps_bbo_visible_on_small_terminal() {
        let areas = main_areas(ratatui::layout::Rect::new(0, 0, 100, 24));

        assert!(areas.bbo.height >= 7);
        assert!(areas.spread.height >= 8);
        assert!(areas.footer.y > areas.spread.y);
    }
}
