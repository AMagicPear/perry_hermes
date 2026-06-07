//! Smoke test for the TUI render module. Visual output is verified manually.

use hermes_cli::tui::app::App;
use hermes_cli::tui::event::{AppMode, RenderedLine};
use hermes_cli::tui::render::render;
use ratatui::backend::TestBackend;
use ratatui::layout::Position;
use ratatui::style::{Color, Modifier};
use ratatui::Terminal;
use std::time::{Duration, Instant};

fn row_at(buffer: &ratatui::buffer::Buffer, y: u16) -> String {
    (0..buffer.area.width)
        .map(|x| {
            buffer
                .cell((x, y))
                .map(|c| c.symbol().chars().next().unwrap_or(' '))
                .unwrap_or(' ')
        })
        .collect()
}

#[test]
fn empty_app_renders_input_box_with_arrow_prompt() {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let mut app = App::new_for_test();
    terminal.draw(|f| render(f, &mut app)).expect("draw");

    let buffer = terminal.backend().buffer().clone();

    // The input block is the last 3 rows; the middle row should contain the
    // placeholder "Send a message…" and the ❯ prompt.
    let input_mid_y = buffer.area.height.saturating_sub(2);
    let input_row = row_at(&buffer, input_mid_y);
    assert!(
        input_row.contains("Send a message"),
        "input middle row should contain placeholder; got: {input_row:?}"
    );
    assert!(
        input_row.contains('❯'),
        "input middle row should contain ❯ prompt; got: {input_row:?}"
    );
}

#[test]
fn chat_scrolls_to_show_most_recent_message() {
    let backend = TestBackend::new(80, 10);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut app = App::new_for_test();
    for i in 0..30 {
        app.push_line(RenderedLine::User(format!("message {i}")));
    }
    app.push_line(RenderedLine::Assistant("LATEST REPLY".to_string()));

    terminal.draw(|f| render(f, &mut app)).expect("draw");

    let buffer = terminal.backend().buffer().clone();
    let mut text = String::new();
    for y in 0..buffer.area.height {
        text.push_str(&row_at(&buffer, y));
    }
    assert!(
        text.contains("LATEST REPLY"),
        "expected most recent message to be visible after auto-scroll; full text:\n{text}"
    );
}

#[test]
fn cursor_uses_display_width_for_cjk_input() {
    let backend = TestBackend::new(20, 6);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let mut app = App::new_for_test();
    app.input = "你好".to_string();
    terminal.draw(|f| render(f, &mut app)).expect("draw");

    terminal
        .backend_mut()
        .assert_cursor_position(Position::new(7, 4));
}

#[test]
fn assistant_render_preserves_explicit_newlines() {
    let backend = TestBackend::new(18, 12);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let mut app = App::new_for_test();
    app.push_line(RenderedLine::Assistant(
        "first line wraps\n\nthird line wraps".to_string(),
    ));
    terminal.draw(|f| render(f, &mut app)).expect("draw");

    let buffer = terminal.backend().buffer().clone();
    let mut rows = Vec::new();
    for y in 0..buffer.area.height {
        rows.push(row_at(&buffer, y));
    }
    let first_idx = rows
        .iter()
        .position(|row| row.contains("first line"))
        .expect("expected first line row");
    let third_idx = rows
        .iter()
        .position(|row| row.contains("third line"))
        .expect("expected third line row");
    assert!(
        third_idx > first_idx + 1,
        "expected a preserved blank line between paragraphs; rows:\n{}",
        rows.join("\n")
    );
    assert!(
        rows.iter().any(|row| row.contains("╭─ ⚕ Hermes")),
        "expected framed assistant header; rows:\n{}",
        rows.join("\n")
    );
    assert!(
        rows.iter()
            .filter(|row| row.contains("first line") || row.contains("third line"))
            .all(|row| !row.contains('│')),
        "expected assistant body rows without vertical borders; rows:\n{}",
        rows.join("\n")
    );
}

#[test]
fn tool_result_render_preserves_multiline_preview() {
    let backend = TestBackend::new(50, 12);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let mut app = App::new_for_test();
    app.push_line(RenderedLine::ToolResult {
        name: "read_file".to_string(),
        output: "1|first\n2|second\n3|third".to_string(),
        ok: true,
    });
    terminal.draw(|f| render(f, &mut app)).expect("draw");

    let buffer = terminal.backend().buffer().clone();
    let mut text = String::new();
    for y in 0..buffer.area.height {
        text.push_str(&row_at(&buffer, y));
        text.push('\n');
    }
    assert!(
        text.contains("1|first"),
        "expected first preview line: {text}"
    );
    assert!(
        text.contains("2|second"),
        "expected second preview line: {text}"
    );
    assert!(
        text.contains("3|third"),
        "expected third preview line: {text}"
    );
}

#[test]
fn assistant_body_is_indented_without_vertical_borders() {
    let backend = TestBackend::new(60, 10);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let mut app = App::new_for_test();
    app.push_line(RenderedLine::Assistant(
        "Hey, 我在～ 🌊✨ 有什么需要帮忙的吗？".to_string(),
    ));
    terminal.draw(|f| render(f, &mut app)).expect("draw");

    let buffer = terminal.backend().buffer().clone();
    let mut rows = Vec::new();
    for y in 0..buffer.area.height {
        rows.push(row_at(&buffer, y));
    }

    let body_row = rows
        .iter()
        .find(|row| row.contains("Hey,") || row.contains("有什么需要"))
        .expect("expected assistant body row");
    assert!(
        body_row.starts_with("  "),
        "expected assistant body indentation: {body_row:?}"
    );
    assert!(
        !body_row.contains('│'),
        "expected no vertical borders in assistant body: {body_row:?}"
    );
}

#[test]
fn assistant_header_keeps_right_border_with_unicode_title() {
    let backend = TestBackend::new(40, 8);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let mut app = App::new_for_test();
    app.push_line(RenderedLine::Assistant("hello".to_string()));
    terminal.draw(|f| render(f, &mut app)).expect("draw");

    let buffer = terminal.backend().buffer().clone();
    let header_row = (0..buffer.area.height)
        .map(|y| row_at(&buffer, y))
        .find(|row| row.contains("⚕ Hermes ✦"))
        .expect("expected assistant header row");
    assert!(
        header_row.trim_end().ends_with('╮'),
        "expected assistant header to end with right border: {header_row:?}"
    );
}

#[test]
fn reasoning_rows_are_dimmed_and_prefixed() {
    let backend = TestBackend::new(40, 8);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let mut app = App::new_for_test();
    app.push_line(RenderedLine::Reasoning("thinking step".to_string()));
    terminal.draw(|f| render(f, &mut app)).expect("draw");

    let buffer = terminal.backend().buffer().clone();
    let row_y = (0..buffer.area.height)
        .find(|y| row_at(&buffer, *y).contains("thinking step"))
        .expect("expected reasoning row");
    let row = row_at(&buffer, row_y);
    assert!(
        row.contains("… thinking step"),
        "expected reasoning prefix: {row:?}"
    );

    let cell = buffer.cell((0, row_y)).expect("cell");
    assert_eq!(cell.fg, Color::Reset, "expected no explicit fg override");
    assert!(
        cell.modifier.contains(Modifier::DIM),
        "expected reasoning row to be dimmed; modifier={:?}",
        cell.modifier
    );
}

#[test]
fn awaiting_state_renders_interrupt_hint_without_needing_full_phrase() {
    let backend = TestBackend::new(40, 8);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let mut app = App::new_for_test();
    app.mode = AppMode::AwaitingModel;
    app.turn_started_at = Some(Instant::now() - Duration::from_secs(2));
    terminal.draw(|f| render(f, &mut app)).expect("draw");

    let buffer = terminal.backend().buffer().clone();
    let mut text = String::new();
    for y in 0..buffer.area.height {
        text.push_str(&row_at(&buffer, y));
        text.push('\n');
    }
    assert!(text.contains("Working"), "expected working row: {text}");
    assert!(
        text.contains("Esc to stop") || text.contains("stop"),
        "expected interrupt hint to remain visible in narrow width: {text}"
    );
}

#[test]
fn cancelling_state_renders_activity_line() {
    let backend = TestBackend::new(50, 8);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let mut app = App::new_for_test();
    app.mode = AppMode::Cancelling;
    app.turn_started_at = Some(Instant::now() - Duration::from_secs(1));
    terminal.draw(|f| render(f, &mut app)).expect("draw");

    let buffer = terminal.backend().buffer().clone();
    let activity_row_y = buffer.area.height.saturating_sub(5);
    let activity_row = row_at(&buffer, activity_row_y);
    assert!(
        activity_row.contains("Cancelling"),
        "expected explicit cancelling activity row: {activity_row:?}"
    );
}

#[test]
fn idle_state_has_no_activity_row() {
    let backend = TestBackend::new(50, 8);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let mut app = App::new_for_test();
    terminal.draw(|f| render(f, &mut app)).expect("draw");

    let buffer = terminal.backend().buffer().clone();
    let status_row_y = buffer.area.height.saturating_sub(4);
    let status_row = row_at(&buffer, status_row_y);
    assert!(
        !status_row.contains("ready") && !status_row.contains("working"),
        "expected idle metadata row to avoid mode labels: {status_row:?}"
    );
}

#[test]
fn awaiting_state_uses_activity_row_without_duplicate_status_label() {
    let backend = TestBackend::new(50, 8);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let mut app = App::new_for_test();
    app.mode = AppMode::AwaitingModel;
    app.turn_started_at = Some(Instant::now() - Duration::from_secs(2));
    terminal.draw(|f| render(f, &mut app)).expect("draw");

    let buffer = terminal.backend().buffer().clone();
    let activity_row_y = buffer.area.height.saturating_sub(5);
    let activity_row = row_at(&buffer, activity_row_y);
    let status_row_y = buffer.area.height.saturating_sub(4);
    let status_row = row_at(&buffer, status_row_y);
    assert!(
        activity_row.contains("Working"),
        "expected working activity row: {activity_row:?}"
    );
    assert!(
        !status_row.to_lowercase().contains("working"),
        "expected status metadata row to avoid duplicate working label: {status_row:?}"
    );
}
