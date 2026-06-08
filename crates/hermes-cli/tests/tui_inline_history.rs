use perry_hermes_agent::LoopEvent;
use perry_hermes_cli::tui::event::RenderedLine;
use perry_hermes_cli::tui::history::{line_text, HistoryWrite};
use perry_hermes_cli::tui::run::run_with_backend;
use perry_hermes_cli::tui::App;
use perry_hermes_cli::tui::AppEvent;
use perry_hermes_core::message::{Message, ToolCall};
use perry_hermes_core::tool::ToolOutput;
use ratatui::backend::TestBackend;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use unicode_width::UnicodeWidthChar;

fn terminal_text(backend: &Arc<Mutex<TestBackend>>) -> String {
    let guard = backend.lock().unwrap();
    let mut text = buffer_text_from_buffer(guard.scrollback());
    text.push_str(&buffer_text_from_buffer(guard.buffer()));
    text
}

fn buffer_text_from_buffer(buffer: &ratatui::buffer::Buffer) -> String {
    let width = buffer.area.width as usize;
    let height = buffer.area.height as usize;
    let mut text = String::new();
    for y in 0..height {
        let mut skip = 0usize;
        for x in 0..width {
            if let Some(cell) = buffer.cell((x as u16, y as u16)) {
                if skip == 0 {
                    text.push(cell.symbol().chars().next().unwrap_or(' '));
                }
                skip = cell
                    .symbol()
                    .chars()
                    .next()
                    .and_then(UnicodeWidthChar::width)
                    .unwrap_or(0)
                    .saturating_sub(1);
            }
        }
        text.push('\n');
    }
    text
}

#[test]
fn history_writer_records_lines_without_keeping_them_in_app_scrollback() {
    let mut app = App::new_for_test();
    let mut history = HistoryWrite::default();

    history.push(&mut app, RenderedLine::User("hi".to_string()), 80);

    assert!(app.scrollback.is_empty());
    assert_eq!(
        history.lines.iter().map(line_text).collect::<Vec<_>>(),
        ["> hi"]
    );
}

#[test]
fn streaming_assistant_chunks_flush_by_visual_line() {
    let mut history = HistoryWrite::default();

    history.push_assistant_delta("你好", 24);
    let first = history.drain();
    let first_rendered = first.iter().map(line_text).collect::<Vec<_>>().join("\n");
    assert!(first_rendered.contains("Perry Hermes"));
    assert!(
        !first_rendered.contains("你好"),
        "short partial line should wait until newline, wrap, or completion:\n{first_rendered}"
    );

    history.push_assistant_delta("世界\n下一行", 24);
    let second = history.drain();
    let second_rendered = second.iter().map(line_text).collect::<Vec<_>>().join("\n");
    assert_eq!(second_rendered, "  你好世界");

    history.finish_stream(24);
    let footer = history.drain();
    let footer_rendered = footer.iter().map(line_text).collect::<Vec<_>>().join("\n");
    assert!(footer_rendered.contains("  下一行"));
    assert!(
        footer_rendered
            .lines()
            .last()
            .is_some_and(|line| line.starts_with('╰')),
        "assistant footer should be written only at completion:\n{footer_rendered}"
    );
}

#[tokio::test]
async fn content_delta_is_written_to_terminal_history_before_completion() {
    let backend = Arc::new(Mutex::new(TestBackend::new(80, 24)));
    let cancel = CancellationToken::new();
    let (input_tx, input_rx) = mpsc::unbounded_channel::<AppEvent>();

    input_tx
        .send(AppEvent::Submit("start".to_string()))
        .expect("send submit");
    input_tx
        .send(AppEvent::Loop(LoopEvent::ContentDelta(
            "streaming now".to_string(),
        )))
        .expect("send content delta");
    input_tx.send(AppEvent::Quit).expect("send quit");
    drop(input_tx);

    run_with_backend(
        backend.clone(),
        input_rx,
        cancel,
        "echo".to_string(),
        "test-model".to_string(),
        10,
        None,
    )
    .await
    .expect("tui run returned error");

    let text = terminal_text(&backend);
    assert!(
        text.contains("Perry Hermes"),
        "assistant header should be in terminal history before completion:\n{text}"
    );
}

#[tokio::test]
async fn assistant_message_flushes_stream_to_terminal_history() {
    let backend = Arc::new(Mutex::new(TestBackend::new(80, 24)));
    let cancel = CancellationToken::new();
    let (input_tx, input_rx) = mpsc::unbounded_channel::<AppEvent>();

    input_tx
        .send(AppEvent::Loop(LoopEvent::ContentDelta("你".to_string())))
        .expect("send content delta");
    input_tx
        .send(AppEvent::Loop(LoopEvent::ContentDelta("好".to_string())))
        .expect("send content delta");
    input_tx
        .send(AppEvent::Loop(LoopEvent::AssistantMessage(
            Message::assistant("你好"),
        )))
        .expect("send assistant message");
    input_tx.send(AppEvent::Quit).expect("send quit");
    drop(input_tx);

    run_with_backend(
        backend.clone(),
        input_rx,
        cancel,
        "echo".to_string(),
        "test-model".to_string(),
        10,
        None,
    )
    .await
    .expect("tui run returned error");

    let text = terminal_text(&backend);
    assert!(
        text.contains("  你好"),
        "missing coalesced final assistant content:\n{text}"
    );
}

#[tokio::test]
async fn cjk_history_text_reads_without_inserted_spaces() {
    let backend = Arc::new(Mutex::new(TestBackend::new(80, 24)));
    let cancel = CancellationToken::new();
    let (input_tx, input_rx) = mpsc::unbounded_channel::<AppEvent>();

    input_tx
        .send(AppEvent::Submit("你好".to_string()))
        .expect("send submit");
    input_tx
        .send(AppEvent::Loop(LoopEvent::ContentDelta("世界".to_string())))
        .expect("send content delta");
    input_tx
        .send(AppEvent::Loop(LoopEvent::AssistantMessage(
            Message::assistant("世界"),
        )))
        .expect("send assistant message");
    input_tx.send(AppEvent::Quit).expect("send quit");
    drop(input_tx);

    run_with_backend(
        backend.clone(),
        input_rx,
        cancel,
        "echo".to_string(),
        "test-model".to_string(),
        10,
        None,
    )
    .await
    .expect("tui run returned error");

    let text = terminal_text(&backend);
    assert!(
        text.contains("> 你好") && text.contains("  世界"),
        "CJK history text should not contain inserted spaces:\n{text}"
    );
}

#[tokio::test]
async fn tool_call_events_are_written_to_terminal_history() {
    let backend = Arc::new(Mutex::new(TestBackend::new(80, 24)));
    let cancel = CancellationToken::new();
    let (input_tx, input_rx) = mpsc::unbounded_channel::<AppEvent>();
    let call = ToolCall {
        id: "call_1".to_string(),
        name: "read_file".to_string(),
        arguments: serde_json::json!({"path":"src/main.rs"}),
    };

    input_tx
        .send(AppEvent::Loop(LoopEvent::ToolCallStarted {
            call: call.clone(),
            iteration: 1,
        }))
        .expect("send tool call started");
    input_tx
        .send(AppEvent::Loop(LoopEvent::ToolCallFinished {
            call,
            result: Ok(ToolOutput {
                content: "1|fn main() {}".to_string(),
            }),
        }))
        .expect("send tool call finished");
    input_tx.send(AppEvent::Quit).expect("send quit");
    drop(input_tx);

    run_with_backend(
        backend.clone(),
        input_rx,
        cancel,
        "echo".to_string(),
        "test-model".to_string(),
        10,
        None,
    )
    .await
    .expect("tui run returned error");

    let text = terminal_text(&backend);
    assert!(
        text.contains("read_file"),
        "missing tool call history:\n{text}"
    );
    assert!(
        text.contains("fn main"),
        "missing tool result history:\n{text}"
    );
}
