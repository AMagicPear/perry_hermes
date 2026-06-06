//! Smoke test for the TUI render module. Drives an empty `App` through a
//! `TestBackend` and asserts the rendered buffer contains the expected
//! status bar and input-box placeholders.

use hermes_cli::tui::app::App;
use hermes_cli::tui::event::AppMode;
use hermes_cli::tui::render::render;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

#[test]
fn empty_app_renders_input_box_and_status_bar() {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let app = App::new_for_test();
    terminal
        .draw(|f| render(f, &app))
        .expect("draw");

    let buffer = terminal.backend().buffer().clone();

    // The input box is the 3-row block at the bottom, just above the status bar.
    // Its middle row is `status_y - 2`.
    let status_y = buffer.area.height.saturating_sub(1);
    let input_y = status_y.saturating_sub(2);
    let input_row: String = (0..buffer.area.width)
        .map(|x| {
            buffer
                .cell((x, input_y))
                .map(|c| c.symbol().chars().next().unwrap_or(' '))
                .unwrap_or(' ')
        })
        .collect();
    assert!(
        input_row.contains("Type a message"),
        "input box row should contain the placeholder; got: {input_row:?}"
    );
}

#[test]
fn populated_app_renders_status_bar_with_provider_and_tokens() {
    use hermes_cli::tui::event::AppMode;

    let backend = TestBackend::new(120, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let mut app = App::new_for_test();
    app.provider_name = Some("openai".to_string());
    app.model_name = Some("gpt-4.1-mini".to_string());
    app.iteration = 2;
    app.max_iterations = 10;
    app.last_input_tokens = Some(12_345);
    app.last_output_tokens = Some(4_549);
    app.mode = AppMode::AwaitingModel;

    terminal
        .draw(|f| render(f, &app))
        .expect("draw");

    let buffer = terminal.backend().buffer().clone();
    let status_y = buffer.area.height.saturating_sub(1);
    let status_row: String = (0..buffer.area.width)
        .map(|x| {
            buffer
                .cell((x, status_y))
                .map(|c| c.symbol().chars().next().unwrap_or(' '))
                .unwrap_or(' ')
        })
        .collect();
    assert!(status_row.contains("openai"), "status row missing provider: {status_row:?}");
    assert!(status_row.contains("gpt-4.1-mini"), "status row missing model: {status_row:?}");
    assert!(status_row.contains("12.3K"), "status row missing input tokens: {status_row:?}");
    assert!(status_row.contains("4.5K"), "status row missing output tokens: {status_row:?}");
    assert!(status_row.contains("iter 2/10"), "status row missing iteration: {status_row:?}");
    assert!(status_row.contains("awaiting"), "status row missing mode: {status_row:?}");
}