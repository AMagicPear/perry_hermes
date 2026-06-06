//! Smoke test for the TUI render module. Visual output is verified manually.

use hermes_cli::tui::app::App;
use hermes_cli::tui::event::RenderedLine;
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

#[test]
fn empty_app_renders_input_box_with_arrow_prompt() {
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
fn chat_scrolls_to_show_most_recent_message() {
    // Build a long scrollback (more lines than the chat area can hold) and
    // assert the bottom of the scrollback is visible after rendering.
    let backend = TestBackend::new(80, 10);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut app = App::new_for_test();
    for i in 0..30 {
        app.push_line(RenderedLine::User(format!("message {i}")));
    }
    app.push_line(RenderedLine::Assistant("LATEST REPLY".to_string()));

    terminal.draw(|f| render(f, &app)).expect("draw");

    // Concatenate every row and check the latest reply is rendered.
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
