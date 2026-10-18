//! fork: groups agents by what they need from the user.
//!
//! Detection alone cannot tell a finished agent from one that asked a question
//! in plain text or one waiting on CI. Agents report that through the `phase`
//! metadata token (see `herdr mcp`), and the agent panel's status sort shows
//! the groups as sections.

/// Metadata token an agent sets when it ends a turn.
pub(crate) const PHASE_TOKEN: &str = "phase";
/// Unix milliseconds of the agent's last report, so a stop hook can tell
/// whether the current turn reported without racing the agent's transcript.
pub(crate) const PHASE_AT_TOKEN: &str = "phase_at";
pub(crate) const PHASE_QUESTION: &str = "question";
pub(crate) const PHASE_WAITING: &str = "waiting";
pub(crate) const PHASE_DONE: &str = "done";

pub(crate) fn is_phase(value: &str) -> bool {
    matches!(value, PHASE_QUESTION | PHASE_WAITING | PHASE_DONE)
}

/// Sections in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum AgentGroup {
    /// Blocked on a prompt, or asked a question in its reply.
    NeedsInput,
    Working,
    /// Idle, but waiting on something such as CI before it can continue.
    Waiting,
    /// Idle with nothing pending; safe to close.
    Finished,
}

impl AgentGroup {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::NeedsInput => "確認待ち",
            Self::Working => "実行中",
            Self::Waiting => "監視中",
            Self::Finished => "完了",
        }
    }
}

/// A live blocker or live work wins over the last reported phase, which only
/// describes how the previous turn ended.
pub(crate) fn agent_group(blocked: bool, working: bool, phase: Option<&str>) -> AgentGroup {
    if blocked {
        return AgentGroup::NeedsInput;
    }
    if working {
        return AgentGroup::Working;
    }
    match phase {
        Some(PHASE_QUESTION) => AgentGroup::NeedsInput,
        Some(PHASE_WAITING) => AgentGroup::Waiting,
        _ => AgentGroup::Finished,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_state_wins_over_reported_phase() {
        assert_eq!(
            agent_group(true, false, Some(PHASE_DONE)),
            AgentGroup::NeedsInput
        );
        assert_eq!(
            agent_group(false, true, Some(PHASE_QUESTION)),
            AgentGroup::Working
        );
    }

    #[test]
    fn idle_agents_follow_their_reported_phase() {
        assert_eq!(
            agent_group(false, false, Some(PHASE_QUESTION)),
            AgentGroup::NeedsInput
        );
        assert_eq!(
            agent_group(false, false, Some(PHASE_WAITING)),
            AgentGroup::Waiting
        );
        assert_eq!(
            agent_group(false, false, Some(PHASE_DONE)),
            AgentGroup::Finished
        );
        assert_eq!(agent_group(false, false, None), AgentGroup::Finished);
        assert_eq!(
            agent_group(false, false, Some("other")),
            AgentGroup::Finished
        );
    }
}
