//! Notifiche di superzed: gravita', archivio globale e parsing del canale `notify`.
pub mod model;
pub mod payload;
pub mod store;

pub use model::{NewNotification, Notification, NotificationAction, NotificationState, Severity, project_label, relative_time, resolve_prompt, sort_for_display};
pub use payload::{MAX_DETAILS, NotifyInput, parse_notify_payload};
pub use store::{MAX_KEPT, NotificationEvent, NotificationFilter, NotificationStore, init, init_test, prune};
