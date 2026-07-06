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
    domain::{BboTick, Fixed, InstrumentCatalog},
    state::{BboSnapshot, SharedBboState},
};

#[derive(Debug, Default)]
struct TuiSelection {
    bbo_market: usize,
    bbo_row: usize,
    spread_market: usize,
    spread_row_a: usize,
    spread_row_b: usize,
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
    instrument_a: String,
    instrument_b: String,
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
        .map(|(index, row)| {
            let style = if selection.focus == FocusPanel::Bbo && index == selection.bbo_row {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let catalog = row.catalog;
            let tick = row.tick;
            Row::new(vec![
                Cell::from(instrument_venue(tick, catalog)),
                Cell::from(instrument_symbol(tick, catalog)),
                Cell::from(instrument_quote(catalog)),
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
            Constraint::Length(8),
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
            "venue", "symbol", "quote", "bid", "ask", "bid size", "ask size", "spread", "age",
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
    let first = market.and_then(|market| snapshot.row_for_market(market, selection.spread_row_a));
    let second = market.and_then(|market| snapshot.row_for_market(market, selection.spread_row_b));
    let title = match (market, first.as_ref(), second.as_ref()) {
        (Some(market), Some(a), Some(b)) => {
            format!(
                "Spread: {} vs {} / {market}",
                instrument_short_label(a.tick, a.catalog),
                instrument_short_label(b.tick, b.catalog)
            )
        }
        _ => "Spread: waiting for two instruments and a shared asset".to_string(),
    };

    let block = panel_block(&title, selection.focus == FocusPanel::Spread);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let summary_h = spread_summary_height(inner.height);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(summary_h), Constraint::Min(3)])
        .split(inner);

    if let (Some(first), Some(second)) = (first.as_ref(), second.as_ref()) {
        draw_spread_summary_table(frame, chunks[0], snapshot, first, second);
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
        SpreadLeg::First => "1st instrument",
        SpreadLeg::Second => "2nd instrument",
    };
    let footer = Paragraph::new(Line::from(vec![
        Span::styled("q", Style::default().fg(Color::Yellow)),
        Span::raw(" quit  "),
        Span::styled("tab", Style::default().fg(Color::Yellow)),
        Span::raw(format!(" focus={focus}  ")),
        Span::styled("left/right", Style::default().fg(Color::Yellow)),
        Span::raw(" market  "),
        Span::styled("up/down", Style::default().fg(Color::Yellow)),
        Span::raw(format!(" row ({leg})  ")),
        Span::styled("1/2", Style::default().fg(Color::Yellow)),
        Span::raw(" spread leg  "),
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
    snapshot: &BboSnapshot,
    first: &crate::state::BboRow<'_>,
    second: &crate::state::BboRow<'_>,
) -> Vec<SpreadSummaryRow> {
    let label_a = instrument_short_label(first.tick, first.catalog);
    let label_b = instrument_short_label(second.tick, second.catalog);
    let data_status = comparability_status(snapshot, first.catalog, second.catalog);
    vec![
        SpreadSummaryRow {
            direction: "green A->B".to_string(),
            formula: format!("{label_a} bid - {label_b} ask"),
            value: cross_spread(snapshot, first, second),
            bp: spread_bp(snapshot, first, second),
            meaning: data_status
                .clone()
                .unwrap_or_else(|| "sell A, buy B; >0 before fees".to_string()),
        },
        SpreadSummaryRow {
            direction: "magenta B->A".to_string(),
            formula: format!("{label_b} bid - {label_a} ask"),
            value: cross_spread(snapshot, second, first),
            bp: spread_bp(snapshot, second, first),
            meaning: data_status
                .clone()
                .unwrap_or_else(|| "sell B, buy A; >0 before fees".to_string()),
        },
        SpreadSummaryRow {
            direction: "mid diff".to_string(),
            formula: format!("{label_a} mid - {label_b} mid"),
            value: diff_mid(snapshot, first, second),
            bp: "-".to_string(),
            meaning: data_status
                .unwrap_or_else(|| "-90s left -> now right; zero = break-even".to_string()),
        },
    ]
}

fn comparability_status(
    snapshot: &BboSnapshot,
    first: Option<&InstrumentCatalog>,
    second: Option<&InstrumentCatalog>,
) -> Option<String> {
    let (Some(first), Some(second)) = (first, second) else {
        return Some("missing catalog for this instrument".to_string());
    };
    if common_quote(snapshot, first, second).is_some() {
        None
    } else {
        Some(format!(
            "not comparable: missing rate for {} vs {}",
            first.quote_asset, second.quote_asset
        ))
    }
}

fn draw_spread_summary_table(
    frame: &mut Frame<'_>,
    area: Rect,
    snapshot: &BboSnapshot,
    first: &crate::state::BboRow<'_>,
    second: &crate::state::BboRow<'_>,
) {
    let rows = spread_summary_rows(snapshot, first, second)
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
            .name("A bid - B ask")
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(Color::Green))
            .data(&a_to_b),
        Dataset::default()
            .name("B bid - A ask")
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

fn cross_spread(
    snapshot: &BboSnapshot,
    sell: &crate::state::BboRow<'_>,
    buy: &crate::state::BboRow<'_>,
) -> String {
    cross_spread_value(snapshot, sell, buy)
        .map(format_decimal)
        .unwrap_or_else(|| "-".to_string())
}

fn cross_spread_value(
    snapshot: &BboSnapshot,
    sell: &crate::state::BboRow<'_>,
    buy: &crate::state::BboRow<'_>,
) -> Option<f64> {
    let target_quote = common_quote(snapshot, sell.catalog?, buy.catalog?)?;
    let sell_bid = converted_price(
        snapshot,
        sell.tick.bid.as_ref()?.price,
        sell.catalog?,
        &target_quote,
    )?;
    let buy_ask = converted_price(
        snapshot,
        buy.tick.ask.as_ref()?.price,
        buy.catalog?,
        &target_quote,
    )?;
    Some(sell_bid - buy_ask)
}

fn spread_bp(
    snapshot: &BboSnapshot,
    sell: &crate::state::BboRow<'_>,
    buy: &crate::state::BboRow<'_>,
) -> String {
    let Some(spread) = cross_spread_value(snapshot, sell, buy) else {
        return "-".to_string();
    };
    let (Some(sell_catalog), Some(buy_catalog)) = (sell.catalog, buy.catalog) else {
        return "-".to_string();
    };
    let Some(target_quote) = common_quote(snapshot, sell_catalog, buy_catalog) else {
        return "-".to_string();
    };
    let Some(buy_ask) = buy
        .tick
        .ask
        .as_ref()
        .and_then(|ask| converted_price(snapshot, ask.price, buy_catalog, &target_quote))
    else {
        return "-".to_string();
    };

    format_bp(spread, buy_ask)
}

fn format_bp(numerator: f64, denominator: f64) -> String {
    if !numerator.is_finite() || !denominator.is_finite() || denominator.abs() < f64::EPSILON {
        return "-".to_string();
    }
    format!("{:.2}", numerator / denominator * 10_000.0)
}

fn common_quote(
    snapshot: &BboSnapshot,
    first: &InstrumentCatalog,
    second: &InstrumentCatalog,
) -> Option<String> {
    snapshot
        .rates
        .common_quote(&first.quote_asset, &second.quote_asset)
}

fn converted_price(
    snapshot: &BboSnapshot,
    price: Fixed,
    catalog: &InstrumentCatalog,
    target_quote: &str,
) -> Option<f64> {
    let rate = snapshot.rates.rate(&catalog.quote_asset, target_quote)?;
    Some(price.to_f64() * rate.to_f64())
}

fn format_decimal(value: f64) -> String {
    if !value.is_finite() {
        return "-".to_string();
    }
    if value.abs() >= 100.0 {
        format!("{value:.2}")
    } else if value.abs() >= 1.0 {
        format!("{value:.4}")
    } else {
        format!("{value:.6}")
    }
}

fn diff_mid(
    snapshot: &BboSnapshot,
    lhs: &crate::state::BboRow<'_>,
    rhs: &crate::state::BboRow<'_>,
) -> String {
    let (Some(lhs_catalog), Some(rhs_catalog)) = (lhs.catalog, rhs.catalog) else {
        return "-".to_string();
    };
    let Some(target_quote) = common_quote(snapshot, lhs_catalog, rhs_catalog) else {
        return "-".to_string();
    };
    let Some(lhs_mid) = lhs
        .tick
        .mid
        .and_then(|mid| converted_price(snapshot, mid, lhs_catalog, &target_quote))
    else {
        return "-".to_string();
    };
    let Some(rhs_mid) = rhs
        .tick
        .mid
        .and_then(|mid| converted_price(snapshot, mid, rhs_catalog, &target_quote))
    else {
        return "-".to_string();
    };
    format_decimal(lhs_mid - rhs_mid)
}

fn level_price(level: Option<&crate::domain::BestLevel>) -> String {
    level
        .map(|level| level.price.to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn instrument_venue(tick: &BboTick, catalog: Option<&InstrumentCatalog>) -> String {
    catalog
        .map(|catalog| catalog.venue_instance_id.clone())
        .unwrap_or_else(|| tick.instrument.venue_instance_id.clone())
}

fn instrument_symbol(tick: &BboTick, catalog: Option<&InstrumentCatalog>) -> String {
    catalog
        .map(|catalog| catalog.display_symbol().to_string())
        .unwrap_or_else(|| tick.instrument.instrument_id.clone())
}

fn instrument_quote(catalog: Option<&InstrumentCatalog>) -> String {
    catalog
        .map(|catalog| catalog.quote_asset.clone())
        .unwrap_or_else(|| "?".to_string())
}

fn instrument_short_label(tick: &BboTick, catalog: Option<&InstrumentCatalog>) -> String {
    let venue = instrument_venue(tick, catalog);
    let symbol = instrument_symbol(tick, catalog);
    let quote = instrument_quote(catalog);
    format!("{venue} {symbol}/{quote}")
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

fn selected_spread_key(snapshot: &BboSnapshot, selection: &TuiSelection) -> Option<SpreadKey> {
    let market = selected_market(snapshot, selection.spread_market)?;
    let first = snapshot.row_for_market(market, selection.spread_row_a)?;
    let second = snapshot.row_for_market(market, selection.spread_row_b)?;
    Some(SpreadKey {
        market: market.to_string(),
        instrument_a: first.tick.instrument.catalog_id.clone(),
        instrument_b: second.tick.instrument.catalog_id.clone(),
    })
}

fn row_index_for_catalog(snapshot: &BboSnapshot, market: &str, catalog_id: &str) -> Option<usize> {
    snapshot
        .rows_for_market(market)
        .into_iter()
        .position(|row| row.tick.instrument.catalog_id == catalog_id)
}

impl SpreadHistory {
    fn record_selected(&mut self, snapshot: &BboSnapshot, selection: &TuiSelection) {
        let Some(key) = selected_spread_key(snapshot, selection) else {
            return;
        };
        if key.instrument_a == key.instrument_b {
            return;
        }

        let Some(first_index) = row_index_for_catalog(snapshot, &key.market, &key.instrument_a)
        else {
            return;
        };
        let Some(second_index) = row_index_for_catalog(snapshot, &key.market, &key.instrument_b)
        else {
            return;
        };
        let Some(first) = snapshot.row_for_market(&key.market, first_index) else {
            return;
        };
        let Some(second) = snapshot.row_for_market(&key.market, second_index) else {
            return;
        };
        let Some(a_sell_b_buy) = cross_spread_value(snapshot, &first, &second) else {
            return;
        };
        let Some(b_sell_a_buy) = cross_spread_value(snapshot, &second, &first) else {
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
        self.bbo_row = clamp_index(
            self.bbo_row,
            selected_market(snapshot, self.bbo_market)
                .map(|market| snapshot.rows_for_market(market).len())
                .unwrap_or_default(),
        );
        let spread_rows = selected_market(snapshot, self.spread_market)
            .map(|market| snapshot.rows_for_market(market).len())
            .unwrap_or_default();
        self.spread_row_a = clamp_index(self.spread_row_a, spread_rows);
        self.spread_row_b = clamp_index(self.spread_row_b, spread_rows);
        if spread_rows > 1 && self.spread_row_a == self.spread_row_b {
            self.spread_row_b = (self.spread_row_a + 1) % spread_rows;
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
                    self.bbo_row = wrap_prev(self.bbo_row, len);
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
                    self.bbo_row = (self.bbo_row + 1) % len;
                }
            }
            FocusPanel::Spread => self.next_spread_venue(snapshot),
        }
    }

    fn prev_spread_venue(&mut self, snapshot: &BboSnapshot) {
        let len = selected_market(snapshot, self.spread_market)
            .map(|market| snapshot.rows_for_market(market).len())
            .unwrap_or_default();
        if len == 0 {
            return;
        }
        match self.spread_leg {
            SpreadLeg::First => self.spread_row_a = wrap_prev(self.spread_row_a, len),
            SpreadLeg::Second => self.spread_row_b = wrap_prev(self.spread_row_b, len),
        }
    }

    fn next_spread_venue(&mut self, snapshot: &BboSnapshot) {
        let len = selected_market(snapshot, self.spread_market)
            .map(|market| snapshot.rows_for_market(market).len())
            .unwrap_or_default();
        if len == 0 {
            return;
        }
        match self.spread_leg {
            SpreadLeg::First => self.spread_row_a = (self.spread_row_a + 1) % len,
            SpreadLeg::Second => self.spread_row_b = (self.spread_row_b + 1) % len,
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
        domain::{
            BboTick, BestLevel, Fixed, InstrumentCatalog, ProductType, QuoteRate, QuoteRateBook,
            SourceKind,
        },
        pipeline::normalizer,
        state::BboStore,
        tui::{SpreadHistory, TuiSelection, cross_spread, main_areas, spread_summary_rows},
    };

    fn catalog(venue: &str) -> InstrumentCatalog {
        catalog_with_quote(venue, "USDC")
    }

    fn catalog_with_quote(venue: &str, quote_asset: &str) -> InstrumentCatalog {
        InstrumentCatalog::new(
            venue,
            "ETH",
            "ETH",
            Some("ETH".to_string()),
            ProductType::Perp,
            "ETH",
            quote_asset,
            quote_asset,
            quote_asset,
            None,
            None,
            None,
            "active",
            None,
        )
    }

    fn rate(from: &str, to: &str, rate: &str) -> QuoteRate {
        QuoteRate {
            from: from.to_string(),
            to: to.to_string(),
            rate: rate.parse().unwrap(),
        }
    }

    fn tick(catalog: &InstrumentCatalog, bid: &str, ask: &str) -> BboTick {
        normalizer::normalize(
            BboTick::new(
                catalog.instrument_ref(),
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
            ),
            5_000,
        )
    }

    #[test]
    fn calculates_cross_spread() {
        let mut store = BboStore::default();
        let first_catalog = catalog("hyperliquid");
        let second_catalog = catalog("lighter");
        store.update_catalog(first_catalog.clone());
        store.update_catalog(second_catalog.clone());
        store.update_tick(tick(&first_catalog, "101", "102"));
        store.update_tick(tick(&second_catalog, "100", "100.5"));
        let snapshot = store.snapshot();
        let first = snapshot.row_for_market("ETH", 0).unwrap();
        let second = snapshot.row_for_market("ETH", 1).unwrap();

        assert_eq!(cross_spread(&snapshot, &first, &second), "0.500000");
        assert_eq!(cross_spread(&snapshot, &second, &first), "-2.0000");
    }

    #[test]
    fn calculates_spread_profit_bp_from_buy_ask() {
        let mut store = BboStore::default();
        let first_catalog = catalog("hyperliquid");
        let second_catalog = catalog("lighter");
        store.update_catalog(first_catalog.clone());
        store.update_catalog(second_catalog.clone());
        store.update_tick(tick(&first_catalog, "101", "102"));
        store.update_tick(tick(&second_catalog, "100", "100.5"));
        let snapshot = store.snapshot();
        let first = snapshot.row_for_market("ETH", 0).unwrap();
        let second = snapshot.row_for_market("ETH", 1).unwrap();

        assert_eq!(super::spread_bp(&snapshot, &first, &second), "49.75");
        assert_eq!(super::spread_bp(&snapshot, &second, &first), "-196.08");
    }

    #[test]
    fn calculates_cross_spread_through_configured_common_quote() {
        let rates = QuoteRateBook::new([rate("USDC", "USD", "1"), rate("USDT", "USD", "0.999")]);
        let mut store = BboStore::new(rates);
        let first_catalog = catalog_with_quote("hyperliquid", "USDC");
        let second_catalog = catalog_with_quote("binance", "USDT");
        store.update_catalog(first_catalog.clone());
        store.update_catalog(second_catalog.clone());
        store.update_tick(tick(&first_catalog, "101", "102"));
        store.update_tick(tick(&second_catalog, "100", "100.5"));
        let snapshot = store.snapshot();
        let rows = snapshot.rows_for_market("ETH");
        let first = rows
            .iter()
            .find(|row| row.tick.instrument.venue_instance_id == "hyperliquid")
            .unwrap();
        let second = rows
            .iter()
            .find(|row| row.tick.instrument.venue_instance_id == "binance")
            .unwrap();

        assert_eq!(cross_spread(&snapshot, &first, &second), "0.600500");
    }

    #[test]
    fn selection_clamps_to_available_snapshot() {
        let mut store = BboStore::default();
        let first_catalog = catalog("hyperliquid");
        store.update_catalog(first_catalog.clone());
        store.update_tick(tick(&first_catalog, "101", "102"));
        let snapshot = store.snapshot();
        let mut selection = TuiSelection {
            bbo_market: 99,
            bbo_row: 99,
            spread_market: 99,
            spread_row_a: 99,
            spread_row_b: 99,
            ..TuiSelection::default()
        };

        selection.clamp(&snapshot);
        assert_eq!(selection.bbo_market, 0);
        assert_eq!(selection.bbo_row, 0);
        assert_eq!(selection.spread_market, 0);
        assert_eq!(selection.spread_row_a, 0);
        assert_eq!(selection.spread_row_b, 0);
    }

    #[test]
    fn spread_history_keeps_recent_samples_for_selected_pair() {
        let mut store = BboStore::default();
        let first_catalog = catalog("hyperliquid");
        let second_catalog = catalog("lighter");
        store.update_catalog(first_catalog.clone());
        store.update_catalog(second_catalog.clone());
        store.update_tick(tick(&first_catalog, "101", "102"));
        store.update_tick(tick(&second_catalog, "100", "100.5"));
        let snapshot = store.snapshot();
        let mut selection = TuiSelection {
            spread_market: 0,
            spread_row_a: 0,
            spread_row_b: 1,
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
        let mut store = BboStore::default();
        let first_catalog = catalog("hyperliquid");
        let second_catalog = catalog("lighter");
        store.update_catalog(first_catalog.clone());
        store.update_catalog(second_catalog.clone());
        store.update_tick(tick(&first_catalog, "101", "102"));
        store.update_tick(tick(&second_catalog, "100", "100.5"));
        let snapshot = store.snapshot();
        let first = snapshot.row_for_market("ETH", 0).unwrap();
        let second = snapshot.row_for_market("ETH", 1).unwrap();

        let rows = spread_summary_rows(&snapshot, &first, &second);

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

        assert!(joined.contains("hyperliquid ETH/USDC bid - lighter ETH/USDC ask"));
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
