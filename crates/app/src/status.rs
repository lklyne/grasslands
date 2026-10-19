//! herdr's agent-status vocabulary, the one place every view takes it from:
//! color, glyph, words and priority. It follows herdr 0.9.5's `shell.rs`
//! (`status_color`, `status_icon`, `status_priority`): blocked red, working
//! yellow, done (finished, not yet seen) teal, idle green, unknown muted,
//! mapped onto the theme's semantic colors. herdr's `ui.status_indicators`
//! picks the glyphs: "dots" (`● ● ● ○ ·` for blocked, working, done, idle,
//! unknown) or "symbols" (`× ◐ ✓ ○ ·`).

use std::collections::BTreeMap;

use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::*;

use crate::herdr::AgentStatus;
use crate::herdr::config::StatusIndicators;

/// How status glyphs draw: herdr's `ui.status_indicators`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Indicators {
    #[default]
    Dots,
    Symbols,
}

/// The indicators herdr's config asks for.
pub fn indicators(cx: &App) -> Indicators {
    match crate::herdr_prefs::get(cx).status_indicators {
        StatusIndicators::Dots => Indicators::Dots,
        StatusIndicators::Symbols => Indicators::Symbols,
    }
}

/// herdr's `status_priority`: what needs you most ranks highest.
pub fn priority(status: AgentStatus) -> u8 {
    match status {
        AgentStatus::Blocked => 4,
        AgentStatus::Done => 3,
        AgentStatus::Working => 2,
        AgentStatus::Idle => 1,
        AgentStatus::Unknown => 0,
    }
}

/// The status that needs you most among `statuses`.
pub fn rolled_up(statuses: impl IntoIterator<Item = AgentStatus>) -> AgentStatus {
    statuses
        .into_iter()
        .max_by_key(|&s| priority(s))
        .unwrap_or(AgentStatus::Unknown)
}

/// Blocked, or finished and not yet seen: the statuses that toast, play a
/// sound and count on the dock badge.
pub fn wants_user(status: AgentStatus) -> bool {
    matches!(status, AgentStatus::Blocked | AgentStatus::Done)
}

/// An agent that would be stopped by closing its pane.
pub fn is_busy(status: AgentStatus) -> bool {
    matches!(status, AgentStatus::Working | AgentStatus::Blocked)
}

/// herdr's word for `status`, as the CLI and API spell it.
pub fn word(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Working => "working",
        AgentStatus::Blocked => "blocked",
        AgentStatus::Done => "done",
        AgentStatus::Idle => "idle",
        AgentStatus::Unknown => "unknown",
    }
}

/// What to show for `status`: the agent's own label for it (`pane
/// report-metadata --state-label`), else herdr's word.
pub fn text(status: AgentStatus, state_labels: &BTreeMap<String, String>) -> &str {
    state_labels
        .get(word(status))
        .map(String::as_str)
        .filter(|label| !label.trim().is_empty())
        .unwrap_or(word(status))
}

/// herdr's `status_color` on the theme.
pub fn color(status: AgentStatus, cx: &App) -> Hsla {
    let theme = cx.theme();
    match status {
        AgentStatus::Blocked => theme.danger,
        AgentStatus::Working => theme.warning,
        AgentStatus::Done => theme.info,
        AgentStatus::Idle => theme.success,
        AgentStatus::Unknown => theme.muted_foreground,
    }
}

/// The glyph for `status` as a 12×12 SVG.
pub fn glyph(status: AgentStatus, indicators: Indicators) -> &'static [u8] {
    match (indicators, status) {
        (Indicators::Dots, AgentStatus::Blocked | AgentStatus::Working | AgentStatus::Done) => DOT,
        (Indicators::Symbols, AgentStatus::Blocked) => CROSS,
        (Indicators::Symbols, AgentStatus::Working) => HALF,
        (Indicators::Symbols, AgentStatus::Done) => CHECK,
        (_, AgentStatus::Idle) => RING,
        (_, AgentStatus::Unknown) => SPECK,
    }
}

/// `status`'s glyph in its color, in a fixed slot so labels beside it line
/// up whatever the status.
pub fn indicator(status: AgentStatus, cx: &App) -> Div {
    div().flex_shrink_0().size(SLOT).child(icon(status, cx))
}

/// `status`'s glyph as an icon (a toast's, say), in its color.
pub fn icon(status: AgentStatus, cx: &App) -> Icon {
    Icon::default()
        .data(glyph(status, indicators(cx)))
        .with_size(SLOT)
        .text_color(color(status, cx))
}

const SLOT: Pixels = px(12.);

const DOT: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 12 12"><circle cx="6" cy="6" r="3.5" fill="currentColor"/></svg>"#;
const RING: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 12 12"><circle cx="6" cy="6" r="3" fill="none" stroke="currentColor" stroke-width="1.2"/></svg>"#;
const SPECK: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 12 12"><circle cx="6" cy="6" r="1.5" fill="currentColor"/></svg>"#;
const CROSS: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M3 3l6 6M9 3l-6 6"/></svg>"#;
const HALF: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 12 12"><circle cx="6" cy="6" r="3.9" fill="none" stroke="currentColor" stroke-width="1.2"/><path d="M6 2.1a3.9 3.9 0 0 0 0 7.8z" fill="currentColor"/></svg>"#;
const CHECK: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M2.5 6.5 5 9l4.5-6"/></svg>"#;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{Indicators, glyph, priority, rolled_up, text, wants_user};
    use crate::herdr::AgentStatus::{self, *};

    #[test]
    fn rolled_up_prefers_blocked_over_done() {
        assert_eq!(rolled_up([Idle, Done, Blocked, Working]), Blocked);
    }

    #[test]
    fn rolled_up_of_nothing_is_unknown() {
        assert_eq!(rolled_up([]), Unknown);
    }

    #[test]
    fn priority_follows_herdr() {
        let mut statuses = [Unknown, Working, Idle, Blocked, Done];
        statuses.sort_by_key(|&s| std::cmp::Reverse(priority(s)));
        assert_eq!(statuses, [Blocked, Done, Working, Idle, Unknown]);
    }

    #[test]
    fn only_blocked_and_done_want_the_user() {
        let wanted: Vec<AgentStatus> = [Idle, Working, Blocked, Done, Unknown]
            .into_iter()
            .filter(|&s| wants_user(s))
            .collect();
        assert_eq!(wanted, [Blocked, Done]);
    }

    #[test]
    fn state_label_replaces_the_word() {
        let labels = BTreeMap::from([("blocked".to_owned(), "needs approval".to_owned())]);
        assert_eq!(text(Blocked, &labels), "needs approval");
    }

    #[test]
    fn missing_state_label_falls_back_to_the_word() {
        let labels = BTreeMap::from([("blocked".to_owned(), "needs approval".to_owned())]);
        assert_eq!(text(Working, &labels), "working");
    }

    #[test]
    fn dots_share_one_glyph_for_active_states() {
        let dots = Indicators::Dots;
        assert!(
            glyph(Blocked, dots) == glyph(Working, dots)
                && glyph(Done, dots) == glyph(Working, dots)
        );
    }

    #[test]
    fn symbols_tell_every_state_apart() {
        let symbols: std::collections::HashSet<&[u8]> = [Idle, Working, Blocked, Done, Unknown]
            .into_iter()
            .map(|s| glyph(s, Indicators::Symbols))
            .collect();
        assert_eq!(symbols.len(), 5);
    }
}
