//! Sessioni claude dei nodi del canvas: stato da hook e status line, file degli
//! hook, comando di lancio, store globale.
pub mod hook_files;
pub mod launch;
pub mod model;
pub mod phase;
pub mod store;
pub mod ui;

pub use model::{
    AccountLoad, Attention, EDIT_TOOLS, HookOutcome, InputKind, RateLimits, RateWindow, SYSTEM_ACCOUNT, ScheduledTask, Session, SessionState, SessionTimes,
    WAKEUP_ID, apply_hook, apply_status, attention, now_unix, now_unix_ms, priority_rank,
};
pub use phase::{Phase, SavedSession, advance, phase_for_event};
pub use store::{SessionEvent, SessionStore, WaitRouter, init, init_test};
