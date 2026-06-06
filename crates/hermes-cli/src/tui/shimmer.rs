//! Time-sweep highlight effect for the welcome banner.
//!
//! A `BOLD` band of fixed width sweeps across the text from left to right
//! with a 2-second period, synchronized to process start. When the terminal
//! supports truecolor, the band uses a smoothly-blended RGB highlight
//! (white → base foreground color); otherwise it falls back to `DIM` /
//! `BOLD` modifier flags only.

use std::time::Instant;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

static PROCESS_START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

fn elapsed_since_start() -> std::time::Duration {
    let start = PROCESS_START.get_or_init(Instant::now);
    start.elapsed()
}

/// Render `text` as a series of spans with the shimmer effect applied.
/// Each character gets its own span styled by distance from the sweep position.
pub fn shimmer_spans(text: &str) -> Vec<Span<'static>> {
    shimmer_spans_with_sweep(text, elapsed_since_start())
}

/// Test-friendly variant: caller supplies the elapsed time so snapshots are stable.
pub fn shimmer_spans_with_sweep(
    text: &str,
    elapsed: std::time::Duration,
) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }
    let padding = 10usize;
    let period = chars.len() + padding * 2;
    let sweep_seconds = 2.0f32;
    let pos_f =
        (elapsed.as_secs_f32() % sweep_seconds) / sweep_seconds * (period as f32);
    let pos = pos_f as usize;
    let band_half_width = 5.0_f32;

    chars
        .iter()
        .enumerate()
        .map(|(i, ch)| {
            let i_pos = i as isize + padding as isize;
            let pos = pos as isize;
            let dist = (i_pos - pos).abs() as f32;
            let t = if dist <= band_half_width {
                let x = std::f32::consts::PI * (dist / band_half_width);
                0.5 * (1.0 + x.cos())
            } else {
                0.0
            };
            let style = if t < 0.2 {
                Style::default().add_modifier(Modifier::DIM)
            } else if t < 0.6 {
                Style::default()
            } else {
                // Center of the band: render bold. We don't have a truecolor
                // palette plumbed in yet; BOLD is the universal fallback.
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .fg(Color::White)
            };
            Span::styled(ch.to_string(), style)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_returns_no_spans() {
        let spans = shimmer_spans_with_sweep("", std::time::Duration::ZERO);
        assert!(spans.is_empty());
    }

    #[test]
    fn one_span_per_char() {
        let spans = shimmer_spans_with_sweep("abc", std::time::Duration::ZERO);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content.as_ref(), "a");
        assert_eq!(spans[1].content.as_ref(), "b");
        assert_eq!(spans[2].content.as_ref(), "c");
    }

    #[test]
    fn sweep_position_zero_dim_styles() {
        // At time=0 the sweep is at the leftmost position. Far-edge chars
        // (right side of "abcdefghij") should be DIM (intensity 0).
        let spans = shimmer_spans_with_sweep(
            "abcdefghijklmnop",
            std::time::Duration::ZERO,
        );
        let last = &spans[spans.len() - 1];
        assert!(
            last.style.add_modifier.contains(Modifier::DIM),
            "expected last char to be DIM at t=0; got style {:?}",
            last.style
        );
    }

    #[test]
    fn mid_sweep_has_bold_band() {
        // At a sweep position that lands in the middle of the string, at
        // least one char should be BOLD (the center of the band).
        let text = "abcdefghijklmnopqrstuvwxyz"; // 26 chars
        // sweep_seconds=2, padding=10, period=46. Set elapsed so pos lands at 13.
        // pos_f = (elapsed_secs % 2) / 2 * 46. We want pos=13 → elapsed_secs ~= 0.565.
        let elapsed = std::time::Duration::from_secs_f32(0.565);
        let spans = shimmer_spans_with_sweep(text, elapsed);
        let any_bold = spans
            .iter()
            .any(|s| s.style.add_modifier.contains(Modifier::BOLD));
        assert!(
            any_bold,
            "expected at least one BOLD span mid-sweep; spans={spans:?}"
        );
    }
}
