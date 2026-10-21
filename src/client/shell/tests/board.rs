// fork: the agents' asks in the sidebar and the focused board in its column.
use super::super::board_panel::BoardTarget;
use super::*;
use crate::board::{AskKind, BoardAsk, BoardLink, BoardTodo, PaneBoard, TodoState};

fn agent(pane_id: &str, focused: bool, board: PaneBoard) -> ClientShellAgent {
    ClientShellAgent {
        pane_id: pane_id.into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_1".into(),
        name: Some(pane_id.into()),
        display_agent: None,
        agent: None,
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: AgentStatus::Idle,
        state_change_seq: 1,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused,
        board,
    }
}

fn board_state(agents: Vec<ClientShellAgent>) -> ClientShellState {
    let mut snapshot = snapshot();
    snapshot.agents = agents;
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    state.compose(120, 50).expect("board");
    state
}

fn click(state: &mut ClientShellState, rect: Rect, modifiers: KeyModifiers) -> ClientShellInput {
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: rect.x + 4,
        row: rect.y,
        modifiers,
    })])
}

fn screen(state: &mut ClientShellState, cols: u16) -> String {
    let frame = state.compose(cols, 50).expect("board");
    frame
        .cells
        .chunks(frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn sidebar_lists_every_agents_asks_and_the_column_shows_the_focused_board() {
    let focused = agent(
        "pane_1",
        true,
        PaneBoard {
            todos: vec![BoardTodo {
                text: "add tests".into(),
                state: TodoState::InProgress,
            }],
            links: vec![BoardLink {
                label: "dev server".into(),
                url: "http://localhost:3000".into(),
            }],
            ..PaneBoard::default()
        },
    );
    let other = agent(
        "pane_2",
        false,
        PaneBoard {
            asks: vec![BoardAsk {
                kind: AskKind::Decide,
                text: "Q1 削除してよいか".into(),
            }],
            todos: vec![BoardTodo {
                text: "hidden todo".into(),
                state: TodoState::Pending,
            }],
            links: vec![BoardLink {
                label: "other link".into(),
                url: "http://localhost:4000".into(),
            }],
        },
    );
    let mut state = board_state(vec![focused, other]);

    let layout = state.layout(120, 50);
    assert_eq!(layout.board.width, 34);
    assert_eq!(layout.board.right(), 120);
    assert_eq!(layout.pane_surface.right(), layout.board.x);
    assert_eq!(
        state.surface_size(120, 50).cols,
        layout.pane_surface.width.max(1)
    );

    let screen = screen(&mut state, 120);
    for shown in [
        "Q1 削除してよいか",
        "add tests",
        "dev server",
        "localhost:3000",
    ] {
        // Wide characters leave a blank second cell, so compare without spaces.
        let compact = screen.replace(' ', "");
        assert!(
            compact.contains(&shown.replace(' ', "")),
            "{shown}\n{screen}"
        );
    }
    for hidden in ["hidden todo", "other link"] {
        assert!(!screen.contains(hidden), "{hidden}\n{screen}");
    }

    let hit = |target: &BoardTarget| {
        state
            .hits
            .board
            .iter()
            .find(|(_, candidate)| candidate == target)
            .map(|(rect, _)| *rect)
            .expect("board line")
    };
    let link = hit(&BoardTarget::OpenUrl("http://localhost:3000".into()));
    let ask = hit(&BoardTarget::FocusPane("pane_2".into()));
    assert!(link.x >= layout.board.x, "links sit in the column");
    assert!(
        ask.right() <= layout.sidebar.right(),
        "asks sit in the sidebar"
    );
    for modifiers in [KeyModifiers::empty(), KeyModifiers::CONTROL] {
        let outcome = click(&mut state, link, modifiers);
        assert!(
            matches!(&outcome.actions[..], [ClientShellAction::OpenSafeWebUrl(url)] if url == "http://localhost:3000"),
            "{:?}",
            outcome.actions
        );
    }
    let outcome = click(&mut state, ask, KeyModifiers::empty());
    let [ClientShellAction::Endpoint { request, .. }] = &outcome.actions[..] else {
        panic!("clicking an ask should focus its pane");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_2"
    ));
}

#[test]
fn an_empty_board_takes_no_room() {
    let state = board_state(vec![agent("pane_1", true, PaneBoard::default())]);
    assert!(state.hits.board.is_empty());
    assert_eq!(state.layout(120, 50).board.width, 0);
}

#[test]
fn a_narrow_window_keeps_its_panes_instead_of_the_column() {
    let board = PaneBoard {
        todos: vec![BoardTodo {
            text: "add tests".into(),
            state: TodoState::Pending,
        }],
        ..PaneBoard::default()
    };
    let state = board_state(vec![agent("pane_1", true, board)]);
    assert_eq!(state.layout(90, 50).board.width, 0);
}
