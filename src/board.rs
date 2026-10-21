//! fork: a pane's board, what its agent keeps in front of the user.
//!
//! An agent replaces whole lists through `pane.board.set` (see `herdr mcp`):
//! what it needs from the user (`asks`), its own steps (`todos`), and URLs
//! worth keeping open (`links`). Replacing whole lists keeps the board in step
//! with the agent even when it forgets an earlier entry. The client sidebar
//! shows every pane's asks and the focused pane's todos and links.

use serde::{Deserialize, Serialize};

pub(crate) const MAX_ASKS: usize = 10;
pub(crate) const MAX_TODOS: usize = 20;
pub(crate) const MAX_LINKS: usize = 10;
const MAX_TEXT_CHARS: usize = 160;
const MAX_LABEL_CHARS: usize = 60;
const MAX_URL_CHARS: usize = 2048;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneBoard {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub asks: Vec<BoardAsk>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub todos: Vec<BoardTodo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<BoardLink>,
}

impl PaneBoard {
    pub fn is_empty(&self) -> bool {
        self.asks.is_empty() && self.todos.is_empty() && self.links.is_empty()
    }
}

/// Something the agent needs from the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BoardAsk {
    #[serde(default)]
    pub kind: AskKind,
    pub text: String,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum AskKind {
    /// A decision or answer only the user can give.
    #[default]
    Decide,
    /// Work only the user can do, such as signing in or reviewing.
    Do,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BoardTodo {
    pub text: String,
    #[serde(default)]
    pub state: TodoState,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TodoState {
    #[default]
    Pending,
    InProgress,
    Done,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BoardLink {
    pub label: String,
    pub url: String,
}

pub(crate) fn normalize_asks(asks: Vec<BoardAsk>) -> Result<Vec<BoardAsk>, String> {
    let asks = asks
        .into_iter()
        .filter_map(|ask| {
            Some(BoardAsk {
                kind: ask.kind,
                text: clean_text(&ask.text, MAX_TEXT_CHARS)?,
            })
        })
        .collect::<Vec<_>>();
    within_limit(asks, MAX_ASKS, "asks")
}

pub(crate) fn normalize_todos(todos: Vec<BoardTodo>) -> Result<Vec<BoardTodo>, String> {
    let todos = todos
        .into_iter()
        .filter_map(|todo| {
            Some(BoardTodo {
                state: todo.state,
                text: clean_text(&todo.text, MAX_TEXT_CHARS)?,
            })
        })
        .collect::<Vec<_>>();
    within_limit(todos, MAX_TODOS, "todos")
}

pub(crate) fn normalize_links(links: Vec<BoardLink>) -> Result<Vec<BoardLink>, String> {
    let mut normalized = Vec::with_capacity(links.len());
    for link in links {
        let url = link.url.trim();
        if url.chars().count() > MAX_URL_CHARS
            || url.chars().any(char::is_whitespace)
            || crate::app::actions::safe_web_url(url).is_none()
        {
            return Err(format!(
                "link url must be a scheme:// URL without spaces, at most {MAX_URL_CHARS} characters: {url}"
            ));
        }
        let label = clean_text(&link.label, MAX_LABEL_CHARS).unwrap_or_else(|| url.to_string());
        normalized.push(BoardLink {
            label,
            url: url.to_string(),
        });
    }
    within_limit(normalized, MAX_LINKS, "links")
}

fn within_limit<T>(items: Vec<T>, max: usize, name: &str) -> Result<Vec<T>, String> {
    if items.len() > max {
        return Err(format!(
            "{name} may contain at most {max} entries; drop finished or minor ones"
        ));
    }
    Ok(items)
}

/// One line of plain text: control characters become spaces, runs of
/// whitespace collapse, and long text is cut with an ellipsis.
fn clean_text(text: &str, max_chars: usize) -> Option<String> {
    let text = text
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if text.is_empty() {
        return None;
    }
    if text.chars().count() <= max_chars {
        return Some(text);
    }
    let mut cut = text.chars().take(max_chars - 1).collect::<String>();
    cut.push('…');
    Some(cut)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_becomes_one_trimmed_line_and_blank_entries_drop() {
        let todos = normalize_todos(vec![
            BoardTodo {
                text: "  run\n tests\t ".into(),
                state: TodoState::InProgress,
            },
            BoardTodo {
                text: " \n ".into(),
                state: TodoState::Pending,
            },
        ])
        .unwrap();
        assert_eq!(
            todos,
            vec![BoardTodo {
                text: "run tests".into(),
                state: TodoState::InProgress,
            }]
        );
    }

    #[test]
    fn long_text_is_cut_with_an_ellipsis() {
        let asks = normalize_asks(vec![BoardAsk {
            kind: AskKind::Do,
            text: "あ".repeat(MAX_TEXT_CHARS + 5),
        }])
        .unwrap();
        assert_eq!(asks[0].text.chars().count(), MAX_TEXT_CHARS);
        assert!(asks[0].text.ends_with('…'));
    }

    #[test]
    fn too_many_entries_are_rejected() {
        let asks = (0..=MAX_ASKS)
            .map(|index| BoardAsk {
                kind: AskKind::Decide,
                text: format!("q{index}"),
            })
            .collect();
        assert!(normalize_asks(asks).is_err());
    }

    #[test]
    fn links_need_a_scheme_and_fall_back_to_the_url_as_label() {
        let links = normalize_links(vec![BoardLink {
            label: " ".into(),
            url: " http://localhost:3000/a ".into(),
        }])
        .unwrap();
        assert_eq!(links[0].label, "http://localhost:3000/a");
        assert_eq!(links[0].url, "http://localhost:3000/a");
        for url in ["localhost:3000", "http://a b", "C://x"] {
            assert!(
                normalize_links(vec![BoardLink {
                    label: "x".into(),
                    url: url.into(),
                }])
                .is_err(),
                "{url}"
            );
        }
    }

    #[test]
    fn unknown_kinds_and_states_from_newer_servers_still_decode() {
        let board: PaneBoard = serde_json::from_str(
            r#"{"asks":[{"kind":"approve","text":"x"}],"todos":[{"text":"y","state":"blocked"}]}"#,
        )
        .unwrap();
        assert_eq!(board.asks[0].kind, AskKind::Unknown);
        assert_eq!(board.todos[0].state, TodoState::Unknown);
    }
}
