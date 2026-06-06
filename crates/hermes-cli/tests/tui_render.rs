//! Smoke test for the TUI render module. Drives an empty `App` through a
//! `TestBackend` and asserts the rendered buffer contains the expected
//! status bar, welcome banner, assistant block, and input-box placeholders.

use hermes_cli::tui::app::App;
use hermes_cli::tui::event::{AppMode, RenderedLine};
use hermes_cli::tui::render::render;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

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

fn full_text(buffer: &ratatui::buffer::Buffer) -> String {
    let mut text = String::new();
    for y in 0..buffer.area.height {
        text.push_str(&row_at(buffer, y));
        text.push('\n');
    }
    text
}

#[test]
fn empty_app_renders_input_box_and_status_bar() {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let app = App::new_for_test();
    terminal.draw(|f| render(f, &app)).expect("draw");

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
fn populated_app_renders_status_bar_with_provider_model_iter_mode() {
    let backend = TestBackend::new(120, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let mut app = App::new_for_test();
    app.welcome_shown = true; // skip the banner so layout is predictable
    app.provider_name = Some("openai".to_string());
    app.model_name = Some("gpt-4.1-mini".to_string());
    app.iteration = 2;
    app.max_iterations = 10;
    app.last_input_tokens = Some(12_345);
    app.last_output_tokens = Some(4_549);
    app.mode = AppMode::AwaitingModel;
    // context_window_size is None — context segment must be hidden.

    terminal.draw(|f| render(f, &app)).expect("draw");

    let buffer = terminal.backend().buffer().clone();
    // Status row sits 1 above the 3-row input block → height-4.
    let status_y = buffer.area.height.saturating_sub(4);
    let status_row = row_at(&buffer, status_y);
    assert!(
        status_row.contains("openai"),
        "status row missing provider: {status_row:?}"
    );
    assert!(
        status_row.contains("gpt-4.1-mini"),
        "status row missing model: {status_row:?}"
    );
    assert!(
        status_row.contains("iter 2/10"),
        "status row missing iter: {status_row:?}"
    );
    assert!(
        status_row.contains("awaiting"),
        "status row missing mode: {status_row:?}"
    );
    // No context segment when context_window_size is None.
    assert!(
        !status_row.contains('%'),
        "status row should not show percent when context_window_size is None: {status_row:?}"
    );
}

#[test]
fn welcome_banner_visible_on_first_render() {
    let backend = TestBackend::new(120, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let app = App::new_for_test();
    terminal.draw(|f| render(f, &app)).expect("draw");
    let buffer = terminal.backend().buffer().clone();
    let text = full_text(&buffer);
    // The banner spans rows 0..5 (5 art rows + 1 tip line). Use row_at
    // for each so we don't depend on line splitting.
    let mut found_banner = false;
    let mut found_tip = false;
    for y in 0..6.min(buffer.area.height) {
        let row = row_at(&buffer, y);
        if row.contains("| |") || row.contains("_____") {
            found_banner = true;
        }
        if row.contains("Tip:") {
            found_tip = true;
        }
    }
    assert!(
        found_banner,
        "expected HERMES banner art in first 6 rows; text:\n{text}"
    );
    assert!(
        found_tip,
        "expected tip line in first 6 rows; text:\n{text}"
    );
}

#[test]
fn welcome_banner_hidden_after_first_render() {
    let backend = TestBackend::new(120, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut app = App::new_for_test();
    app.welcome_shown = true;
    terminal.draw(|f| render(f, &app)).expect("draw");
    let buffer = terminal.backend().buffer().clone();
    let text = full_text(&buffer);
    for y in 0..6.min(buffer.area.height) {
        let row = row_at(&buffer, y);
        assert!(
            !row.contains("| |") && !row.contains("_____"),
            "banner must be hidden after welcome_shown is true; row y={y}: {row:?}"
        );
        assert!(
            !row.contains("Tip:"),
            "tip must be hidden after welcome_shown is true; row y={y}: {row:?}"
        );
    }
    let _ = text; // suppress unused
}

#[test]
fn assistant_message_in_rounded_box() {
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut app = App::new_for_test();
    app.welcome_shown = true;
    app.push_line(RenderedLine::Assistant("hello there".to_string()));
    terminal.draw(|f| render(f, &app)).expect("draw");
    let buffer = terminal.backend().buffer().clone();
    let text = full_text(&buffer);
    assert!(
        text.contains('╭'),
        "expected top-left ╭ border; got:\n{text}"
    );
    assert!(
        text.contains('╰'),
        "expected bottom-left ╰ border; got:\n{text}"
    );
    assert!(
        text.contains("Hermes"),
        "expected Hermes title; got:\n{text}"
    );
    assert!(
        text.contains("hello there"),
        "expected assistant text; got:\n{text}"
    );
}

#[test]
fn status_row_shows_context_percent_when_configured() {
    let backend = TestBackend::new(120, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut app = App::new_for_test();
    app.welcome_shown = true;
    app.context_window_size = Some(1_000_000);
    app.last_input_tokens = Some(200_000);
    app.provider_name = Some("openai".into());
    app.model_name = Some("mimo-v2.5".into());
    terminal.draw(|f| render(f, &app)).expect("draw");
    let buffer = terminal.backend().buffer().clone();
    // Status row sits 1 above the 3-row input block → height-4.
    let status_y = buffer.area.height.saturating_sub(4);
    let row = row_at(&buffer, status_y);
    assert!(row.contains("20%"), "expected 20% in status; got {row:?}");
    assert!(
        row.contains("200.0K"),
        "expected 200.0K tokens; got {row:?}"
    );
    assert!(row.contains("1.0M"), "expected 1.0M total; got {row:?}");
}

#[test]
fn working_indicator_only_when_awaiting() {
    use std::time::{Duration, Instant};

    let backend = TestBackend::new(120, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut app = App::new_for_test();
    app.welcome_shown = true;
    app.mode = AppMode::AwaitingModel;
    app.turn_started_at = Some(Instant::now() - Duration::from_secs(5));
    app.iteration = 3;
    terminal.draw(|f| render(f, &app)).expect("draw");
    let buffer = terminal.backend().buffer().clone();
    let text = full_text(&buffer);
    assert!(
        text.contains("Working"),
        "expected 'Working' indicator in AwaitingModel mode; full text:\n{text}"
    );

    // Now render with Idle mode and assert no Working text.
    app.mode = AppMode::Idle;
    app.turn_started_at = None;
    terminal.draw(|f| render(f, &app)).expect("draw");
    let buffer = terminal.backend().buffer().clone();
    let text = full_text(&buffer);
    assert!(
        !text.contains("Working"),
        "Working must not appear in Idle mode; full text:\n{text}"
    );
}
