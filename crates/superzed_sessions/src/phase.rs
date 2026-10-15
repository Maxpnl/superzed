//! Fase del flusso superpowers di una sessione, ricavata dagli hook di claude.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    // prima di New: un evento superpowers le porta sempre avanti (`advance`)
    Others,
    Analysis,
    #[default]
    New,
    Brainstorming,
    Spec,
    Plan,
    Implementing,
    Done,
}

impl Phase {
    pub const ALL: [Phase; 8] = [
        Phase::Others,
        Phase::Analysis,
        Phase::New,
        Phase::Brainstorming,
        Phase::Spec,
        Phase::Plan,
        Phase::Implementing,
        Phase::Done,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Phase::Others => "Others",
            Phase::Analysis => "Analysis",
            Phase::New => "New",
            Phase::Brainstorming => "Brainstorm",
            Phase::Spec => "Spec",
            Phase::Plan => "Plan",
            Phase::Implementing => "Implement",
            Phase::Done => "Done",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Others => "others",
            Phase::Analysis => "analysis",
            Phase::New => "new",
            Phase::Brainstorming => "brainstorming",
            Phase::Spec => "spec",
            Phase::Plan => "plan",
            Phase::Implementing => "implementing",
            Phase::Done => "done",
        }
    }

    fn index(self) -> usize {
        Phase::ALL.iter().position(|p| *p == self).unwrap_or(0)
    }

    pub fn next(self) -> Option<Phase> {
        Phase::ALL.get(self.index() + 1).copied()
    }

    pub fn prev(self) -> Option<Phase> {
        self.index().checked_sub(1).map(|i| Phase::ALL[i])
    }
}

/// Skill (con o senza prefisso di plugin) → fase.
fn skill_phase(name: &str) -> Option<Phase> {
    match name.rsplit(':').next().unwrap_or(name) {
        "brainstorming" => Some(Phase::Brainstorming),
        "writing-plans" => Some(Phase::Plan),
        "executing-plans" | "subagent-driven-development" | "test-driven-development" => Some(Phase::Implementing),
        "finishing-a-development-branch" => Some(Phase::Done),
        _ => None,
    }
}

const SPEC_DIR: &str = "/docs/superpowers/specs/";

pub fn phase_for_event(payload: &serde_json::Value) -> Option<Phase> {
    match payload.get("hook_event_name")?.as_str()? {
        "PreToolUse" => {
            let input = payload.get("tool_input")?;
            match payload.get("tool_name")?.as_str()? {
                "Skill" => skill_phase(input.get("skill")?.as_str()?),
                "Write" | "Edit" | "MultiEdit" => {
                    input.get("file_path")?.as_str()?.contains(SPEC_DIR).then_some(Phase::Spec)
                }
                _ => None,
            }
        }
        // slash command digitato dall'utente: niente PreToolUse Skill
        "UserPromptSubmit" => {
            let prompt = payload.get("prompt")?.as_str()?.trim_start();
            skill_phase(prompt.strip_prefix('/')?.split_whitespace().next()?)
        }
        _ => None,
    }
}

/// Un evento automatico sposta solo in avanti.
pub fn advance(current: Phase, event: Phase) -> Option<Phase> {
    (event > current).then_some(event)
}

/// Sessione salvata nel `data` di un nodo claude.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SavedSession {
    pub session_id: Option<String>,
    pub phase: Phase,
    pub account: Option<String>,
    pub times: crate::model::SessionTimes,
}

impl SavedSession {
    pub fn from_data(data: &serde_json::Value) -> Self {
        Self {
            session_id: data.get("session_id").and_then(|v| v.as_str()).map(str::to_string),
            phase: data
                .get("phase")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default(),
            account: data.get("account").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_string),
            times: crate::model::SessionTimes::from_data(data),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn skill(name: &str) -> serde_json::Value {
        json!({"hook_event_name": "PreToolUse", "tool_name": "Skill", "tool_input": {"skill": name}})
    }

    #[test]
    fn skills_map_to_phases() {
        assert_eq!(phase_for_event(&skill("superpowers:brainstorming")), Some(Phase::Brainstorming));
        assert_eq!(phase_for_event(&skill("brainstorming")), Some(Phase::Brainstorming));
        assert_eq!(phase_for_event(&skill("superpowers:writing-plans")), Some(Phase::Plan));
        for s in ["executing-plans", "superpowers:subagent-driven-development", "test-driven-development"] {
            assert_eq!(phase_for_event(&skill(s)), Some(Phase::Implementing), "{s}");
        }
        assert_eq!(phase_for_event(&skill("superpowers:finishing-a-development-branch")), Some(Phase::Done));
        assert_eq!(phase_for_event(&skill("superpowers:systematic-debugging")), None);
    }

    #[test]
    fn writing_a_spec_is_the_spec_phase() {
        for tool in ["Write", "Edit", "MultiEdit"] {
            let p = json!({"hook_event_name": "PreToolUse", "tool_name": tool,
                "tool_input": {"file_path": "/repo/docs/superpowers/specs/2026-01-01-x-design.md"}});
            assert_eq!(phase_for_event(&p), Some(Phase::Spec), "{tool}");
        }
        let elsewhere = json!({"hook_event_name": "PreToolUse", "tool_name": "Write",
            "tool_input": {"file_path": "/repo/src/main.rs"}});
        assert_eq!(phase_for_event(&elsewhere), None);
        let read = json!({"hook_event_name": "PreToolUse", "tool_name": "Read",
            "tool_input": {"file_path": "/repo/docs/superpowers/specs/x.md"}});
        assert_eq!(phase_for_event(&read), None);
    }

    #[test]
    fn typed_slash_commands_count() {
        let p = |prompt: &str| json!({"hook_event_name": "UserPromptSubmit", "prompt": prompt});
        assert_eq!(phase_for_event(&p("/superpowers:brainstorming idea nuova")), Some(Phase::Brainstorming));
        assert_eq!(phase_for_event(&p("  /writing-plans")), Some(Phase::Plan));
        assert_eq!(phase_for_event(&p("usa brainstorming per favore")), None);
        assert_eq!(phase_for_event(&p("/")), None);
        assert_eq!(phase_for_event(&p("")), None);
    }

    #[test]
    fn malformed_payloads_have_no_phase() {
        for p in [
            json!({}),
            json!([1]),
            json!({"hook_event_name": "PreToolUse", "tool_name": "Skill"}),
            json!({"hook_event_name": "PreToolUse", "tool_name": "Skill", "tool_input": {"skill": 7}}),
            json!({"hook_event_name": "PreToolUse", "tool_name": "Write", "tool_input": {}}),
            json!({"hook_event_name": "UserPromptSubmit", "prompt": 3}),
            json!({"hook_event_name": "Stop"}),
        ] {
            assert_eq!(phase_for_event(&p), None, "{p}");
        }
    }

    #[test]
    fn manual_move_back_then_events() {
        // l'utente riporta a mano la card da Implementing a Brainstorming
        let current = Phase::Brainstorming;
        assert_eq!(advance(current, Phase::Brainstorming), None, "stessa fase: fermo");
        assert_eq!(advance(current, Phase::New), None, "indietro: fermo");
        assert_eq!(advance(current, Phase::Plan), Some(Phase::Plan), "avanti: si muove");
    }

    #[test]
    fn phase_order_labels_and_serde() {
        assert_eq!(Phase::ALL.len(), 8);
        assert!(Phase::ALL.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(Phase::New.next(), Some(Phase::Brainstorming));
        assert_eq!(Phase::Done.next(), None);
        assert_eq!(Phase::New.prev(), Some(Phase::Analysis));
        assert_eq!(Phase::Analysis.prev(), Some(Phase::Others));
        assert_eq!(Phase::Others.prev(), None);
        assert_eq!(Phase::Analysis.next(), Some(Phase::New));
        assert_eq!(Phase::default(), Phase::New);
        let labels: Vec<_> = Phase::ALL.iter().map(|p| p.label()).collect();
        assert_eq!(labels, ["Others", "Analysis", "New", "Brainstorm", "Spec", "Plan", "Implement", "Done"]);
        assert_eq!(serde_json::to_value(Phase::Plan).unwrap(), json!("plan"));
        assert_eq!(serde_json::to_value(Phase::Analysis).unwrap(), json!("analysis"));
        assert_eq!(Phase::Implementing.as_str(), "implementing");
        assert_eq!(Phase::Others.as_str(), "others");
        assert_eq!(SavedSession::from_data(&json!({"phase": "analysis"})).phase, Phase::Analysis);
        assert_eq!(SavedSession::from_data(&json!({"phase": "others"})).phase, Phase::Others);
        assert_eq!(SavedSession::from_data(&json!({"session_id": "a", "phase": "plan"})),
            SavedSession { session_id: Some("a".into()), phase: Phase::Plan, account: None, times: Default::default() });
        assert_eq!(SavedSession::from_data(&json!({"session_id": "a", "phase": "boh"})).phase, Phase::New);
        let saved = SavedSession::from_data(&json!({"session_id": "a", "phase": "new", "opened_at": 10, "replied_at": 20}));
        assert_eq!((saved.times.opened_at, saved.times.replied_at), (Some(10), Some(20)));
        assert_eq!(SavedSession::from_data(&json!({"session_id": "a"})).phase, Phase::New);
        assert_eq!(SavedSession::from_data(&serde_json::Value::Null), SavedSession::default());
    }

    #[test]
    fn analysis_and_others_move_forward_on_superpowers_events() {
        for from in [Phase::Others, Phase::Analysis] {
            for to in [Phase::Brainstorming, Phase::Spec, Phase::Plan, Phase::Implementing, Phase::Done] {
                assert_eq!(advance(from, to), Some(to), "{from:?} -> {to:?}");
            }
        }
        for phase in Phase::ALL {
            assert_ne!(skill_phase(phase.as_str()), Some(Phase::Analysis));
            assert_ne!(skill_phase(phase.as_str()), Some(Phase::Others));
        }
        let prompt = json!({"hook_event_name": "UserPromptSubmit", "prompt": "/analysis"});
        assert_eq!(phase_for_event(&prompt), None, "solo a mano");
    }

    #[test]
    fn saved_session_reads_the_account() {
        assert_eq!(SavedSession::from_data(&json!({"session_id": "a", "account": "a1"})).account.as_deref(), Some("a1"));
        assert_eq!(SavedSession::from_data(&json!({"account": ""})).account, None, "vuoto = sistema");
        assert_eq!(SavedSession::from_data(&json!({"account": 3})).account, None);
    }
}
