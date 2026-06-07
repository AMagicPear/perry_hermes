//! Smoke test for the TUI render module. Visual output is verified manually.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hermes_cli::tui::app::App;
use hermes_cli::tui::event::{AppMode, RenderedLine};
use hermes_cli::tui::input::handle_key;
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

    // The input block is taller now (5 rows). With height 24, input starts at
    // row 18 (24 - 0 - 1 - 5 = 18). The middle usable row is 18 + 1 = 19.
    let _input_inner_y = buffer.area.height.saturating_sub(4); // 24 - 5 + 1 = 20 → hmm
                                                               // Actually: status at 24-5-1=18, input at 19, inner at 20.
                                                               // Let's just check the last few rows.
    let rows: Vec<String> = (0..buffer.area.height)
        .map(|y| row_at(&buffer, y))
        .collect();
    let has_prompt = rows
        .iter()
        .any(|row| row.contains("❯") && row.contains("Send a message"));
    assert!(
        has_prompt,
        "input should contain ❯ prompt and placeholder; rows:\n{}",
        rows.join("\n")
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
    // Default input height is 1 content row (3 rows total with borders).
    //   chat = 24 - 1 - 3 = 20, status at y=20, input at y=21, inner at y=22
    let backend = TestBackend::new(20, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let mut app = App::new_for_test();
    app.input = "你好".to_string();
    app.cursor = app.input.len(); // cursor at end
    terminal.draw(|f| render(f, &mut app)).expect("draw");

    // Prompt "❯ " = 2 columns, "你好" = 4 columns → total = 6.
    // Inner width = 20 - 2 = 18, no wrapping needed. Inner x = 1.
    // Cursor x = 1 + 6 = 7. Cursor y = input_inner_y + 0 = 22.
    terminal
        .backend_mut()
        .assert_cursor_position(Position::new(7, 22));
}

#[test]
fn cursor_position_accounts_for_cjk_with_half_width_mix() {
    let backend = TestBackend::new(20, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let mut app = App::new_for_test();
    app.input = "ab你好".to_string(); // 2 + 4 = 6 visible cols
    app.cursor = 2; // between "ab" and "你好"
    terminal.draw(|f| render(f, &mut app)).expect("draw");

    // Prompt "❯ " (2) + "ab" (2) = 4. Inner x = 1.
    // Cursor x = 1 + 4 = 5. Cursor y = 22.
    terminal
        .backend_mut()
        .assert_cursor_position(Position::new(5, 22));
}

#[test]
fn input_cursor_when_empty_is_at_prompt_end() {
    let backend = TestBackend::new(20, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let mut app = App::new_for_test();
    // cursor defaults to 0, input is empty
    terminal.draw(|f| render(f, &mut app)).expect("draw");

    // Prompt "❯ " (2) + empty text (0) = 2. Inner x = 1.
    // Cursor x = 1 + 2 = 3. Cursor y = 22.
    terminal
        .backend_mut()
        .assert_cursor_position(Position::new(3, 22));
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
    let backend = TestBackend::new(40, 12);
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
    let backend = TestBackend::new(40, 12);
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
    let backend = TestBackend::new(40, 12);
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
    let backend = TestBackend::new(50, 14);
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
    let backend = TestBackend::new(50, 14);
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
    let backend = TestBackend::new(50, 14);
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

fn count_char_in_buffer(buffer: &ratatui::buffer::Buffer, target: &str) -> usize {
    let mut count = 0;
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            if let Some(cell) = buffer.cell((x, y)) {
                if cell.symbol() == target {
                    count += 1;
                }
            }
        }
    }
    count
}

#[test]
fn input_box_grows_when_text_wraps_to_multiple_lines() {
    // 20-wide terminal → inner_w = 18. With a 60-char ASCII input the
    // text wraps to 4 lines (2 + 60 = 62 visible cols → ceil(62/18) = 4).
    // The input block should grow to fit all 4 lines (4 + 2 borders = 6 rows)
    // so every character is visible and the cursor is not clamped.
    let backend = TestBackend::new(20, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut app = App::new_for_test();
    app.input = "a".repeat(60);
    app.cursor = app.input.len();
    terminal.draw(|f| render(f, &mut app)).expect("draw");

    let buffer = terminal.backend().buffer().clone();
    let a_count = count_char_in_buffer(&buffer, "a");
    assert_eq!(
        a_count, 60,
        "expected all 60 'a' chars visible when input box grows; got {a_count}"
    );
}

#[test]
fn cursor_in_middle_of_wrapped_input_is_not_clamped() {
    // With the input box at a fixed 5 rows the cursor on the 2nd wrapped
    // line gets clamped to the bottom row. Once the box grows to fit the
    // full text, the cursor should land on the correct row.
    //
    // total_width = 2 (prompt) + 30 (chars before cursor) = 32
    // cursor_row = 32 / 18 = 1
    // cursor_col = 32 % 18 = 14
    // With input_h = 6, inner_y = 19, cursor_y = 19 + 1 = 20.
    let backend = TestBackend::new(20, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut app = App::new_for_test();
    app.input = "a".repeat(60);
    app.cursor = 30;
    terminal.draw(|f| render(f, &mut app)).expect("draw");

    terminal
        .backend_mut()
        .assert_cursor_position(Position::new(15, 20));
}

#[test]
fn left_arrow_visibly_moves_cursor_in_wrapped_input() {
    // Typing 60 chars and pressing Left should produce a cursor on a
    // different (x, y) from the end-of-input position. With the old
    // fixed-height input box the cursor is clamped to the bottom row,
    // so pressing Left would only shift x by 1 — but the row never
    // changes. Once the box grows, the cursor should be on its true row.
    //
    // After one Left press: cursor = 59, total = 61, row = 3, col = 7.
    // inner_y = 19, cursor_y = 19 + 3 = 22. cursor_x = 1 + 7 = 8.
    let backend = TestBackend::new(20, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut app = App::new_for_test();
    app.input = "a".repeat(60);
    app.cursor = app.input.len();
    handle_key(&mut app, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    terminal.draw(|f| render(f, &mut app)).expect("draw");

    terminal
        .backend_mut()
        .assert_cursor_position(Position::new(8, 22));
}

#[test]
fn input_box_caps_height_when_text_exceeds_max_lines() {
    // 200 chars wraps to 12 lines, but the input block is capped at
    // 8 visible content lines (10 rows total). The chat area must
    // reclaim the rows the input doesn't need.
    //
    // With cursor at 0, no scroll is applied, so the first 8 wrapped
    // lines should be visible: "❯ " + 16 a's, then 7 more lines of
    // 18 a's = 16 + 7*18 = 142 a's.
    let backend = TestBackend::new(20, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut app = App::new_for_test();
    app.input = "a".repeat(200);
    app.cursor = 0;
    terminal.draw(|f| render(f, &mut app)).expect("draw");

    let buffer = terminal.backend().buffer().clone();
    let a_count = count_char_in_buffer(&buffer, "a");
    assert_eq!(
        a_count, 142,
        "expected 142 'a' chars visible (8 content rows); got {a_count}"
    );
}

#[test]
fn input_scrolls_to_keep_cursor_visible_in_long_text() {
    // 200 chars wraps to 12 lines, capped at 8 visible. Cursor in
    // the middle (byte 100) is on the 6th visual row — inside the
    // visible window, no scroll needed.
    let backend = TestBackend::new(20, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut app = App::new_for_test();
    app.input = "a".repeat(200);
    app.cursor = 100;
    terminal.draw(|f| render(f, &mut app)).expect("draw");

    // text_before_width = 100, first_w = 16, after_first = 84.
    // cursor_row = 1 + 84/18 = 5, cursor_col = 84%18 = 12.
    // inner_h = 8, no scroll. inner_y = 15.
    // cursor_y = 15 + 5 = 20, cursor_x = 1 + 12 = 13.
    terminal
        .backend_mut()
        .assert_cursor_position(Position::new(13, 20));
}

#[test]
fn input_scrolls_when_cursor_past_visible_window() {
    // 200 chars wraps to 12 lines, capped at 8 visible. Cursor at
    // the end (byte 200) is on visual row 11 — past the visible
    // window — so the input should scroll so the cursor lands on
    // the last visible row.
    let backend = TestBackend::new(20, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut app = App::new_for_test();
    app.input = "a".repeat(200);
    app.cursor = app.input.len();
    terminal.draw(|f| render(f, &mut app)).expect("draw");

    // text_before_width = 200, first_w = 16, after_first = 184.
    // cursor_row = 1 + 184/18 = 11, cursor_col = 184%18 = 4.
    // inner_h = 8, scroll_y = 11 - 8 + 1 = 4.
    // visible_cursor_row = 11 - 4 = 7 (last visible row).
    // inner_y = 15. cursor_y = 15 + 7 = 22, cursor_x = 1 + 4 = 5.
    terminal
        .backend_mut()
        .assert_cursor_position(Position::new(5, 22));
}
