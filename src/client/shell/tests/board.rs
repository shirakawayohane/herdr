// fork: the agents' board under the agents panel.
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

#[test]
fn board_lists_every_agents_asks_and_links_and_only_the_focused_todos() {
    let focused = agent(
        "pane_1",
        true,
        PaneBoard {
            todos: vec![BoardTodo {
                text: "add tests".into(),
                state: TodoState::InProgress,
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
                label: "dev server".into(),
                url: "http://localhost:3000".into(),
            }],
        },
    );
    let mut state = board_state(vec![focused, other]);

    let targets = state
        .hits
        .board
        .iter()
        .map(|(_, target)| target.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        targets,
        [
            BoardTarget::FocusPane("pane_2".into()),
            BoardTarget::FocusPane("pane_2".into()),
            BoardTarget::OpenUrl("http://localhost:3000".into()),
        ]
    );
    let frame = state.compose(120, 50).expect("board");
    let screen = frame
        .cells
        .chunks(frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(screen.contains("add tests"), "{screen}");
    assert!(!screen.contains("hidden todo"), "{screen}");

    let link = state.hits.board[2].0;
    for modifiers in [KeyModifiers::empty(), KeyModifiers::CONTROL] {
        let outcome = click(&mut state, link, modifiers);
        assert!(
            matches!(&outcome.actions[..], [ClientShellAction::OpenSafeWebUrl(url)] if url == "http://localhost:3000"),
            "{:?}",
            outcome.actions
        );
    }
    let ask = state.hits.board[1].0;
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
fn an_empty_board_takes_no_rows() {
    let state = board_state(vec![agent("pane_1", true, PaneBoard::default())]);
    assert!(state.hits.board.is_empty());
}
