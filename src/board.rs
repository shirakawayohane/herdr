//! fork: a pane's board, what its agent keeps in front of the user.
//!
//! An agent replaces whole lists through `pane.board.set` (see `herdr mcp`):
//! what it needs from the user (`asks`), its own steps (`todos`), and URLs
//! worth keeping open (`links`). Replacing whole lists keeps the board in step
//! with the agent even when it forgets an earlier entry. The client sidebar
//! shows every pane's asks and the focused pane's todos and links.
//!
//! The board itself only has room for one line per ask, so each ask also
//! carries what the user needs to answer it (`detail`, `options`). Those are
//! shown on the pane's question sheet (`crate::sheet`), a web page the board
//! links to (`sheet_url`), and never sent to the terminal client.

use serde::{Deserialize, Serialize};

pub(crate) const MAX_ASKS: usize = 10;
pub(crate) const MAX_TODOS: usize = 20;
pub(crate) const MAX_LINKS: usize = 10;
pub(crate) const MAX_OPTIONS: usize = 6;
const MAX_TEXT_CHARS: usize = 160;
const MAX_DETAIL_CHARS: usize = 6000;
const MAX_OPTION_CHARS: usize = 120;
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
    /// Where the user reads the asks in full and answers them. Filled in by
    /// the server when it reports a board with asks; never stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sheet_url: Option<String>,
}

impl PaneBoard {
    pub fn is_empty(&self) -> bool {
        self.asks.is_empty() && self.todos.is_empty() && self.links.is_empty()
    }

    /// The board as the terminal client draws it: one line per ask. The
    /// question sheet's text stays out of client snapshots.
    pub(crate) fn for_client(mut self) -> Self {
        for ask in &mut self.asks {
            ask.detail = String::new();
            ask.options = Vec::new();
        }
        self
    }
}

/// Something the agent needs from the user.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BoardAsk {
    #[serde(default)]
    pub kind: AskKind,
    pub text: String,
    /// Markdown for the question sheet: everything the user needs to answer
    /// without going back to the conversation.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    /// Answers the user can pick on the question sheet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
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
                detail: clean_block(&ask.detail, MAX_DETAIL_CHARS),
                options: ask
                    .options
                    .iter()
                    .filter_map(|option| clean_text(option, MAX_OPTION_CHARS))
                    .collect(),
            })
        })
        .collect::<Vec<_>>();
    if let Some(ask) = asks.iter().find(|ask| ask.options.len() > MAX_OPTIONS) {
        return Err(format!(
            "an ask may offer at most {MAX_OPTIONS} options; \"{}\" has {}",
            ask.text,
            ask.options.len()
        ));
    }
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

/// Image types a question sheet shows. SVG is left out: opened directly it
/// could run script on the sheet's origin.
pub(crate) fn image_content_type(path: &str) -> Option<&'static str> {
    let extension = path.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => return None,
    })
}

/// The targets of the Markdown images (`![caption](target)`) in a detail.
pub(crate) fn image_targets(detail: &str) -> Vec<&str> {
    let mut targets = Vec::new();
    let mut rest = detail;
    while let Some(start) = rest.find("![") {
        rest = &rest[start + 2..];
        let Some(close) = rest.find("](") else { break };
        if rest[..close].contains('\n') {
            continue;
        }
        let after = &rest[close + 2..];
        if let Some(end) = after.find(')') {
            let target = &after[..end];
            if !target.is_empty() && !target.contains(char::is_whitespace) {
                targets.push(target);
            }
            rest = &after[end..];
        }
    }
    targets
}

/// Markdown kept as written: line breaks and indentation stay, other control
/// characters go, and long text is cut with an ellipsis.
fn clean_block(text: &str, max_chars: usize) -> String {
    let text = text
        .replace("\r\n", "\n")
        .chars()
        .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'))
        .collect::<String>();
    let text = text.trim_matches('\n').trim_end();
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut cut = text.chars().take(max_chars - 1).collect::<String>();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ask_detail_keeps_its_lines_and_options_become_single_lines() {
        let asks = normalize_asks(vec![BoardAsk {
            kind: AskKind::Decide,
            text: "Q1 削除してよいか".into(),
            detail: "\n## 背景\r\n- a\n  - b\u{7}\n\n".into(),
            options: vec![" はい\n(推奨) ".into(), " ".into(), "いいえ".into()],
        }])
        .unwrap();
        assert_eq!(asks[0].detail, "## 背景\n- a\n  - b");
        assert_eq!(asks[0].options, ["はい (推奨)", "いいえ"]);

        let long = normalize_asks(vec![BoardAsk {
            text: "q".into(),
            detail: "あ".repeat(MAX_DETAIL_CHARS + 1),
            ..BoardAsk::default()
        }])
        .unwrap();
        assert_eq!(long[0].detail.chars().count(), MAX_DETAIL_CHARS);

        let too_many = BoardAsk {
            text: "q".into(),
            options: (0..=MAX_OPTIONS).map(|n| n.to_string()).collect(),
            ..BoardAsk::default()
        };
        assert!(normalize_asks(vec![too_many]).is_err());
    }

    #[test]
    fn image_targets_are_found_and_only_raster_images_are_served() {
        let detail = "案A:\n![現在の画面](/tmp/a b.png)\n![案A](/tmp/a.png) と ![案B](https://x.test/b.jpg)\n[link](/tmp/c.png) ![broken](";
        assert_eq!(
            image_targets(detail),
            ["/tmp/a.png", "https://x.test/b.jpg"]
        );
        assert_eq!(image_content_type("/tmp/A.PNG"), Some("image/png"));
        assert_eq!(image_content_type("/tmp/a.jpeg"), Some("image/jpeg"));
        for path in ["/tmp/a.svg", "/tmp/a", "/etc/passwd", "/tmp/a.png.txt"] {
            assert_eq!(image_content_type(path), None, "{path}");
        }
    }

    #[test]
    fn the_client_board_drops_what_only_the_sheet_shows() {
        let board = PaneBoard {
            asks: vec![BoardAsk {
                kind: AskKind::Do,
                text: "stg に再ログイン".into(),
                detail: "手順".into(),
                options: vec!["済".into()],
            }],
            sheet_url: Some("http://127.0.0.1:1/s/w1:p1".into()),
            ..PaneBoard::default()
        }
        .for_client();
        assert_eq!(board.asks[0].text, "stg に再ログイン");
        assert!(board.asks[0].detail.is_empty() && board.asks[0].options.is_empty());
        assert!(board.sheet_url.is_some());
    }

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
            ..BoardAsk::default()
        }])
        .unwrap();
        assert_eq!(asks[0].text.chars().count(), MAX_TEXT_CHARS);
        assert!(asks[0].text.ends_with('…'));
    }

    #[test]
    fn too_many_entries_are_rejected() {
        let asks = (0..=MAX_ASKS)
            .map(|index| BoardAsk {
                text: format!("q{index}"),
                ..BoardAsk::default()
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
