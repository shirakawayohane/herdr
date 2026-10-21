//! fork: `pane.board.set`, the agent's board (`crate::board`).

use super::responses::{encode_error, encode_success};
use crate::api::schema::{PaneBoardSetParams, ResponseResult};
use crate::app::App;

impl App {
    pub(super) fn handle_pane_board_set(
        &mut self,
        id: String,
        params: PaneBoardSetParams,
    ) -> String {
        if params.asks.is_none() && params.todos.is_none() && params.links.is_none() {
            return encode_error(
                id,
                "invalid_board_request",
                "pass at least one of asks, todos, links",
            );
        }
        let normalized = (|| {
            Ok::<_, String>((
                params.asks.map(crate::board::normalize_asks).transpose()?,
                params
                    .todos
                    .map(crate::board::normalize_todos)
                    .transpose()?,
                params
                    .links
                    .map(crate::board::normalize_links)
                    .transpose()?,
            ))
        })();
        let (asks, todos, links) = match normalized {
            Ok(lists) => lists,
            Err(message) => return encode_error(id, "invalid_board", message),
        };
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(terminal) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.pane_state(pane_id))
            .map(|pane| pane.attached_terminal_id.clone())
            .and_then(|terminal_id| self.state.terminals.get_mut(&terminal_id))
        else {
            return pane_not_found(id, &params.pane_id);
        };
        let mut board = terminal.board.clone();
        if let Some(asks) = asks {
            board.asks = asks;
        }
        if let Some(todos) = todos {
            board.todos = todos;
        }
        if let Some(links) = links {
            board.links = links;
        }
        if board != terminal.board {
            terminal.board = board;
            terminal.revision = terminal.revision.saturating_add(1);
            self.emit_pane_updated(ws_idx, pane_id);
        }
        encode_success(id, ResponseResult::Ok {})
    }
}

fn pane_not_found(id: String, pane_id: &str) -> String {
    encode_error(id, "pane_not_found", format!("pane {pane_id} not found"))
}
