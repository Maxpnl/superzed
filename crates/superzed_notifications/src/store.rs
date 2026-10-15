//! Archivio globale delle notifiche: `notifications.json` (scrittura atomica) piu' un
//! `.md` di dettaglio per notifica, con un tetto di 500 attive + 500 archiviate.
use crate::model::{NewNotification, Notification, NotificationAction, NotificationState, sort_for_display};
use chrono::{DateTime, Local};
use gpui::{App, AppContext as _, Context, Entity, EventEmitter, Global};
use serde_json::json;
use std::path::{Path, PathBuf};

/// Notifiche attive (o archiviate) tenute al massimo prima di eliminare le piu' vecchie.
pub const MAX_KEPT: usize = 500;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotificationEvent {
    Added(String),
    Changed(String),
}

#[derive(Clone, Debug, Default)]
pub struct NotificationFilter {
    pub project: Option<PathBuf>,
    pub include_archived: bool,
}

pub struct NotificationStore {
    dir: PathBuf,
    items: Vec<Notification>,
}

struct GlobalNotificationStore(Entity<NotificationStore>);
impl Global for GlobalNotificationStore {}

impl EventEmitter<NotificationEvent> for NotificationStore {}

impl NotificationStore {
    pub fn global(cx: &App) -> Option<Entity<NotificationStore>> {
        cx.try_global::<GlobalNotificationStore>().map(|g| g.0.clone())
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn get(&self, id: &str) -> Option<&Notification> {
        self.items.iter().find(|n| n.id == id)
    }

    pub fn add(&mut self, n: NewNotification, cx: &mut Context<Self>) -> String {
        self.add_at(n, Local::now(), cx)
    }

    pub fn add_at(&mut self, n: NewNotification, now: DateTime<Local>, cx: &mut Context<Self>) -> String {
        self.insert(n, None, now, cx)
    }

    /// Notifica interna con un'azione (es. «Rivedi»).
    pub fn add_with_action(&mut self, n: NewNotification, action: NotificationAction, cx: &mut Context<Self>) -> String {
        self.insert(n, Some(action), Local::now(), cx)
    }

    fn insert(&mut self, n: NewNotification, action: Option<NotificationAction>, now: DateTime<Local>, cx: &mut Context<Self>) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        let path = self.dir.join(format!("{id}.md"));
        let markdown = render_markdown(&n, now);
        let details_path = match std::fs::write(&path, markdown) {
            Ok(()) => Some(path),
            Err(e) => {
                log::error!("superzed notifications: writing detail {id} failed: {e:#}");
                None
            }
        };
        self.items.push(Notification {
            id: id.clone(),
            created_at: now,
            severity: n.severity,
            title: n.title,
            summary: n.summary,
            details_path,
            suggested_prompt: n.suggested_prompt,
            project_root: n.project_root,
            source: n.source,
            state: NotificationState::New,
            action,
        });
        for removed in prune(&mut self.items, MAX_KEPT) {
            if let Some(p) = removed.details_path
                && let Err(e) = std::fs::remove_file(&p)
            {
                log::error!("superzed notifications: deleting {} failed: {e:#}", p.display());
            }
        }
        self.save();
        cx.emit(NotificationEvent::Added(id.clone()));
        cx.notify();
        id
    }

    pub fn set_state(&mut self, id: &str, state: NotificationState, cx: &mut Context<Self>) -> bool {
        let Some(n) = self.items.iter_mut().find(|n| n.id == id) else {
            return false;
        };
        if n.state == state {
            return true;
        }
        n.state = state;
        self.save();
        cx.emit(NotificationEvent::Changed(id.to_string()));
        cx.notify();
        true
    }

    pub fn list(&self, filter: &NotificationFilter) -> Vec<Notification> {
        let mut list: Vec<Notification> = self
            .items
            .iter()
            .filter(|n| filter.project.as_deref().is_none_or(|p| n.project_root.as_deref() == Some(p)))
            .filter(|n| filter.include_archived || n.state != NotificationState::Archived)
            .cloned()
            .collect();
        sort_for_display(&mut list);
        list
    }

    pub fn count_new(&self, project: Option<&Path>) -> usize {
        self.items
            .iter()
            .filter(|n| n.state == NotificationState::New)
            .filter(|n| project.is_none_or(|p| n.project_root.as_deref() == Some(p)))
            .count()
    }

    /// Scrittura sincrona e atomica (la lista e' tenuta corta da `prune`, quindi e'
    /// economica): fatta cosi' un `add`/`set_state` non puo' perdere il salvataggio se
    /// arriva un secondo cambiamento nello stesso tick o l'app chiude subito dopo,
    /// come poteva succedere con un `Task` di sfondo scartato prima di girare.
    fn save(&self) {
        let path = self.dir.join("notifications.json");
        match serde_json::to_string_pretty(&json!({"version": 1, "notifications": &self.items})) {
            Ok(text) => write_atomic(&path, &text),
            Err(e) => log::error!("superzed notifications: serialization failed: {e:#}"),
        }
    }

    /// Legge l'archivio da disco. File mancante → vuoto; contenuto illeggibile →
    /// spostato in `notifications.json.bak` (sovrascritto se gia' esiste) e vuoto.
    pub fn load(dir: &Path) -> Vec<Notification> {
        let path = dir.join("notifications.json");
        if !path.is_file() {
            return Vec::new();
        }
        if let Some(items) = try_load(&path) {
            return items;
        }
        let bak = dir.join("notifications.json.bak");
        if let Err(e) = std::fs::rename(&path, &bak) {
            log::error!("superzed notifications: moving notifications.json to .bak failed: {e:#}");
        }
        Vec::new()
    }
}

fn try_load(path: &Path) -> Option<Vec<Notification>> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    serde_json::from_value(value.get("notifications")?.clone()).ok()
}

fn write_atomic(path: &Path, text: &str) {
    let tmp = path.with_extension("json.tmp");
    if let Err(e) = std::fs::write(&tmp, text) {
        log::error!("superzed notifications: writing {} failed: {e:#}", tmp.display());
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        log::error!("superzed notifications: renaming {} to {} failed: {e:#}", tmp.display(), path.display());
    }
}

fn render_markdown(n: &NewNotification, now: DateTime<Local>) -> String {
    let root = n.project_root.as_deref().map(|p| p.display().to_string()).unwrap_or_else(|| "—".to_string());
    let summary = if n.summary.is_empty() { "(nessun riassunto)" } else { &n.summary };
    let details = n.details.as_deref().unwrap_or("(nessun dettaglio)");
    let mut md = format!(
        "# {title}\n\n- gravita': {label}\n- sorgente: {source}\n- progetto: {root}\n- data: {date}\n\n## Riassunto\n\n{summary}\n\n## Dettagli\n\n{details}\n",
        title = n.title,
        label = n.severity.prompt_label(),
        source = n.source,
        date = now.to_rfc3339(),
    );
    if let Some(p) = &n.suggested_prompt {
        md.push_str(&format!("\n## Prompt suggerito\n\n{p}\n"));
    }
    md
}

/// Se le notifiche non archiviate superano `max`, le piu' vecchie diventano archiviate;
/// se poi le archiviate superano `max`, le piu' vecchie sono tolte dalla lista e restituite
/// (chi chiama elimina il loro `.md`).
pub fn prune(list: &mut Vec<Notification>, max: usize) -> Vec<Notification> {
    let mut active: Vec<usize> = (0..list.len()).filter(|&i| list[i].state != NotificationState::Archived).collect();
    if active.len() > max {
        active.sort_by_key(|&i| list[i].created_at);
        for &i in &active[..active.len() - max] {
            list[i].state = NotificationState::Archived;
        }
    }
    let mut archived: Vec<usize> = (0..list.len()).filter(|&i| list[i].state == NotificationState::Archived).collect();
    let mut removed = Vec::new();
    if archived.len() > max {
        archived.sort_by_key(|&i| list[i].created_at);
        let mut to_remove: Vec<usize> = archived[..archived.len() - max].to_vec();
        to_remove.sort_unstable_by(|a, b| b.cmp(a));
        for i in to_remove {
            removed.push(list.remove(i));
        }
    }
    removed
}

fn new_store(cx: &mut App, dir: PathBuf) -> Entity<NotificationStore> {
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::error!("superzed notifications: creating {} failed: {e:#}", dir.display());
    }
    let items = NotificationStore::load(&dir);
    let store = cx.new(|_| NotificationStore { dir, items });
    cx.set_global(GlobalNotificationStore(store.clone()));
    store
}

pub fn init(cx: &mut App) {
    new_store(cx, paths::data_dir().join("notifications"));
}

pub fn init_test(cx: &mut App, dir: PathBuf) -> Entity<NotificationStore> {
    new_store(cx, dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NewNotification, Severity};
    use chrono::TimeZone as _;
    use gpui::TestAppContext;

    fn new(title: &str, severity: Severity, project: Option<&str>) -> NewNotification {
        NewNotification {
            severity,
            title: title.into(),
            summary: "riassunto".into(),
            details: Some(format!("## stack\n{title}")),
            suggested_prompt: None,
            project_root: project.map(PathBuf::from),
            source: "chat".into(),
        }
    }

    #[gpui::test]
    fn an_action_is_saved_with_the_notification(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let store = cx.update(|cx| init_test(cx, dir.path().to_path_buf()));
        let action = crate::model::NotificationAction { kind: "done_review".into(), label: "Rivedi".into(), arg: "s1".into() };
        let id = store.update(cx, |s, cx| s.add_with_action(new("Report pronto: x", Severity::Info, Some("/p")), action.clone(), cx));
        let plain = store.update(cx, |s, cx| s.add(new("altra", Severity::Info, None), cx));
        store.read_with(cx, |s, _| {
            assert_eq!(s.get(&id).unwrap().action.as_ref(), Some(&action));
            assert_eq!(s.get(&plain).unwrap().action, None);
        });
        let loaded = NotificationStore::load(dir.path());
        assert_eq!(loaded.iter().find(|n| n.id == id).unwrap().action.as_ref(), Some(&action));
        let text = std::fs::read_to_string(dir.path().join("notifications.json")).unwrap();
        assert_eq!(text.matches("\"action\"").count(), 1, "senza azione il campo non si scrive");
    }

    #[test]
    fn files_without_actions_still_load() {
        let dir = tempfile::tempdir().unwrap();
        let old = r#"{"version": 1, "notifications": [{"id": "a", "created_at": "2026-09-27T10:00:00+02:00", "severity": "info",
            "title": "t", "summary": "", "details_path": null, "suggested_prompt": null, "project_root": null, "source": "chat", "state": "new"}]}"#;
        std::fs::write(dir.path().join("notifications.json"), old).unwrap();
        let loaded = NotificationStore::load(dir.path());
        assert_eq!((loaded.len(), loaded[0].action.clone()), (1, None));
    }

    #[gpui::test]
    fn add_writes_the_markdown_and_the_list_and_reloads(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let store = cx.update(|cx| init_test(cx, dir.path().to_path_buf()));
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = events.clone();
        let _sub = cx.update(|cx| cx.subscribe(&store, move |_, e: &NotificationEvent, _| seen.borrow_mut().push(e.clone())));
        let id = store.update(cx, |s, cx| s.add(new("boom", Severity::Error, Some("/a")), cx));
        cx.run_until_parked();
        let n = store.read_with(cx, |s, _| s.get(&id).cloned().unwrap());
        let md = std::fs::read_to_string(n.details_path.as_ref().unwrap()).unwrap();
        assert!(md.starts_with("# boom\n"));
        assert!(md.contains("- gravita': errore") && md.contains("## stack"));
        assert_eq!(n.details_path.as_deref(), Some(dir.path().join(format!("{id}.md")).as_path()));
        assert_eq!(NotificationStore::load(dir.path()), vec![n], "notifications.json riletto uguale");
        assert!(store.update(cx, |s, cx| s.set_state(&id, NotificationState::Taken, cx)));
        assert!(!store.update(cx, |s, cx| s.set_state("nope", NotificationState::Taken, cx)));
        cx.run_until_parked();
        assert_eq!(*events.borrow(), vec![NotificationEvent::Added(id.clone()), NotificationEvent::Changed(id.clone())]);
        assert_eq!(NotificationStore::load(dir.path())[0].state, NotificationState::Taken);
    }

    #[gpui::test]
    fn add_saves_synchronously_even_without_running_the_executor(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let store = cx.update(|cx| init_test(cx, dir.path().to_path_buf()));
        // Due `add` nello stesso update, senza mai far girare l'executor (niente
        // `run_until_parked`): se il salvataggio fosse un task di sfondo scartato dal
        // secondo `add` prima di essere eseguito, il primo andrebbe perso.
        store.update(cx, |s, cx| {
            s.add(new("first", Severity::Info, None), cx);
            s.add(new("second", Severity::Warning, None), cx);
        });
        let titles: Vec<String> = NotificationStore::load(dir.path()).into_iter().map(|n| n.title).collect();
        assert_eq!(titles, ["first", "second"]);
    }

    #[gpui::test]
    fn list_filters_by_project_and_archive_and_sorts(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let store = cx.update(|cx| init_test(cx, dir.path().to_path_buf()));
        let t = |h| Local.with_ymd_and_hms(2026, 9, 27, h, 0, 0).unwrap();
        let (a1, a2, b1) = store.update(cx, |s, cx| {
            (
                s.add_at(new("a-info", Severity::Info, Some("/a")), t(9), cx),
                s.add_at(new("a-crit", Severity::Critical, Some("/a")), t(8), cx),
                s.add_at(new("b-err", Severity::Error, Some("/b")), t(10), cx),
            )
        });
        store.update(cx, |s, cx| s.set_state(&a1, NotificationState::Archived, cx));
        let titles = |f: NotificationFilter, cx: &mut TestAppContext| {
            store.read_with(cx, |s, _| s.list(&f).into_iter().map(|n| n.title).collect::<Vec<_>>())
        };
        assert_eq!(titles(NotificationFilter { project: Some("/a".into()), include_archived: false }, cx), ["a-crit"]);
        assert_eq!(titles(NotificationFilter { project: Some("/a".into()), include_archived: true }, cx), ["a-crit", "a-info"]);
        assert_eq!(titles(NotificationFilter { project: None, include_archived: false }, cx), ["a-crit", "b-err"]);
        assert_eq!(store.read_with(cx, |s, _| s.count_new(Some(Path::new("/a")))), 1);
        assert_eq!(store.read_with(cx, |s, _| s.count_new(None)), 2);
        let _ = (a2, b1);
    }

    #[test]
    fn prune_archives_oldest_active_and_drops_oldest_archived() {
        let t = |m| Local.with_ymd_and_hms(2026, 9, 27, 10, m, 0).unwrap();
        let mut list: Vec<Notification> = (0..5)
            .map(|i| crate::model::tests::sample(&format!("n{i}"), Severity::Info, t(i)))
            .collect();
        list[0].state = NotificationState::Archived;
        list[1].state = NotificationState::Archived;
        // max 2: 3 attive → la piu' vecchia (n2) si archivia; 3 archiviate → la piu' vecchia (n0) si elimina
        let removed = prune(&mut list, 2);
        assert_eq!(removed.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), ["n0"]);
        let state = |id: &str| list.iter().find(|n| n.id == id).map(|n| n.state);
        assert_eq!(state("n2"), Some(NotificationState::Archived));
        assert_eq!(state("n0"), None);
        assert_eq!(list.len(), 4);
    }

    #[test]
    fn broken_list_file_is_set_aside() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notifications.json"), "{ rotto").unwrap();
        assert!(NotificationStore::load(dir.path()).is_empty());
        assert!(dir.path().join("notifications.json.bak").is_file());
    }
}
