use chrono::Local;
use gpui::{
    Action, App, Context, Div, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, Global,
    IntoElement, Pixels, Render, SharedString, Stateful, Subscription, SystemNotification, SystemNotificationAction,
    WeakEntity, Window, WindowHandle, div, px,
};
use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
};
use superzed_actions::SidebarView;
use superzed_canvas::{CanvasView, canvas_for, severity_style::SeverityStyle};
use superzed_notifications::{
    Notification, NotificationFilter, NotificationState, NotificationStore, Severity, project_label, relative_time,
};
use ui::{Button, ButtonStyle, IconName, Label, LabelSize, prelude::*};
use workspace::{
    MultiWorkspace, OpenOptions, OpenVisible, Toast, Workspace,
    dock::{DockPosition, Panel, PanelEvent},
    notifications::NotificationId,
    superzed_tabs::{activate_project_tab, project_tabs},
};

/// Id dell'azione "Risolvi" sulle notifiche di sistema.
pub const RESOLVE_ACTION: &str = "resolve";

/// Stile della card per gravita': lo stesso delle righe delle fonti, sullo sfondo delle
/// superfici in rilievo.
pub fn card_style(severity: Severity, cx: &App) -> SeverityStyle {
    SeverityStyle::new(Some(severity), cx.theme().colors().elevated_surface_background, cx)
}

/// Vista Notifiche: le notifiche delle automazioni come card colorate per gravita',
/// del solo progetto corrente o di tutti, con Risolvi, Dettagli e Archivia.
pub struct NotificationsPanel {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    all_projects: bool,
    show_archived: bool,
    store_sub: Option<Subscription>,
}

impl NotificationsPanel {
    pub fn new(workspace: WeakEntity<Workspace>, cx: &mut Context<Self>) -> Self {
        // subito, non al primo render: il badge delle nuove si aggiorna anche a pannello chiuso
        let store_sub = NotificationStore::global(cx).map(|store| cx.observe(&store, |_, _, cx| cx.notify()));
        Self {
            workspace,
            focus: cx.focus_handle(),
            all_projects: false,
            show_archived: false,
            store_sub,
        }
    }

    /// Cartella del progetto di questo pannello (quella del suo canvas).
    fn current_project(&self, cx: &App) -> Option<PathBuf> {
        let ws = self.workspace.upgrade()?;
        canvas_for(ws.read(cx))?.read(cx).project_root_path(cx)
    }

    /// Card da mostrare, nell'ordine di visualizzazione (piu' gravi, poi piu' recenti).
    pub fn cards(&self, cx: &App) -> Vec<Notification> {
        let Some(store) = NotificationStore::global(cx) else {
            return Vec::new();
        };
        let project = if self.all_projects {
            None
        } else {
            // un workspace senza cartella non ha notifiche "sue": con `project: None`
            // il filtro mostrerebbe invece quelle di tutti i progetti
            match self.current_project(cx) {
                Some(p) => Some(p),
                None => return Vec::new(),
            }
        };
        store.read(cx).list(&NotificationFilter { project, include_archived: self.show_archived })
    }

    /// Testo dello stato vuoto (`None` se ci sono card): col filtro sul progetto dice
    /// quante notifiche ci sono negli altri.
    pub fn empty_message(&self, cx: &App) -> Option<String> {
        self.empty_message_for(&self.cards(cx), cx)
    }

    /// `empty_message` con le card gia' calcolate (il render non le rilegge).
    fn empty_message_for(&self, cards: &[Notification], cx: &App) -> Option<String> {
        if !cards.is_empty() {
            return None;
        }
        let elsewhere = if self.all_projects {
            0
        } else {
            // qui non ce n'e' nessuna: tutte quelle visibili sono di altri progetti
            NotificationStore::global(cx).map_or(0, |store| {
                store.read(cx).list(&NotificationFilter { project: None, include_archived: self.show_archived }).len()
            })
        };
        Some(empty_text(self.all_projects, elsewhere))
    }

    pub fn set_all_projects(&mut self, all: bool, cx: &mut Context<Self>) {
        self.all_projects = all;
        cx.notify();
    }

    pub fn set_show_archived(&mut self, show: bool, cx: &mut Context<Self>) {
        self.show_archived = show;
        cx.notify();
    }

    fn render_card(&self, n: Notification, cx: &App) -> Stateful<Div> {
        let style = card_style(n.severity, cx);
        let id = n.id.clone();
        let dimmed = matches!(n.state, NotificationState::Taken | NotificationState::Archived);
        let meta = format!(
            "{} · {} · {}",
            project_label(n.project_root.as_deref()),
            n.source,
            relative_time(n.created_at, Local::now())
        );
        let mw = self.workspace.upgrade().and_then(|ws| ws.read(cx).multi_workspace()?.upgrade());
        // un'azione propria (es. «Rivedi» della pipeline Fatto → GitHub) prende il posto di «Risolvi»
        let action = n.action.clone().map(|a| {
            let (id, mw) = (id.clone(), mw.clone());
            card_button(format!("superzed-notification-action-{id}"), a.display_label(), move |window, cx| {
                if let Some(mw) = &mw {
                    run_notification_action(mw, &id, window, cx);
                }
            })
        });
        let resolve = n.action.is_none().then(|| {
            let id = id.clone();
            card_button(format!("superzed-notification-resolve-{id}"), "Resolve", move |window, cx| {
                if let Some(mw) = &mw {
                    resolve_notification(mw, &id, window, cx);
                }
            })
        });
        let details = n.details_path.clone().map(|path| {
            let ws = self.workspace.clone();
            card_button(format!("superzed-notification-details-{id}"), "Details", move |window, cx| {
                if let Some(ws) = ws.upgrade() {
                    open_details(&ws, path.clone(), window, cx);
                }
            })
        });
        let archive = (n.state != NotificationState::Archived).then(|| {
            let id = id.clone();
            card_button(format!("superzed-notification-archive-{id}"), "Archive", move |_, cx| {
                if let Some(store) = NotificationStore::global(cx) {
                    store.update(cx, |s, cx| {
                        s.set_state(&id, NotificationState::Archived, cx);
                    });
                }
            })
        });
        let selector = format!("superzed-notification-{id}");
        let severity = h_flex()
            .gap_1()
            .children(style.icon_element())
            .child(Label::new(n.severity.label().to_uppercase()).size(LabelSize::Small).weight(FontWeight::BOLD).color(Color::Muted));
        // prese o archiviate: attenuato solo lo sfondo, il testo resta pieno e leggibile
        let style = SeverityStyle { background: if dimmed { style.background.opacity(0.6) } else { style.background }, ..style };
        style
            .frame(cx)
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector)
            // altezza piena: nella colonna le card si stringerebbero tagliando i pulsanti invece di scorrere
            .flex_none()
            .m(px(8.))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .p_3()
                    .gap_1()
                    .child(severity)
                    .child(Label::new(n.title).size(LabelSize::Large).weight(FontWeight::BOLD))
                    .child(Label::new(meta).size(LabelSize::Small).color(Color::Muted))
                    .when(!n.summary.is_empty(), |d| d.child(div().line_clamp(4).child(n.summary)))
                    .when(n.state == NotificationState::Taken, |d| {
                        d.child(Label::new("In progress").size(LabelSize::Small).italic().color(Color::Muted))
                    })
                    .child(h_flex().pt_1().gap_2().flex_wrap().children(action).children(resolve).children(details).children(archive)),
            )
    }
}

/// Stato vuoto: «No notifications», o col filtro sul progetto quante ce ne sono altrove.
pub fn empty_text(all_projects: bool, elsewhere: usize) -> String {
    if all_projects || elsewhere == 0 {
        "No notifications".into()
    } else {
        format!("No notifications in this project ({elsewhere} in others)")
    }
}

/// Pulsante di una card: `Button` del tema (hover, active, focus), con il selettore
/// `id` sul contenitore per i test.
fn card_button(id: String, label: impl Into<SharedString>, on_click: impl Fn(&mut Window, &mut App) + 'static) -> impl IntoElement {
    let selector = id.clone();
    div().debug_selector(move || selector).child(
        Button::new(SharedString::from(id), label)
            .style(ButtonStyle::Subtle)
            .label_size(LabelSize::Small)
            .on_click(move |_, window, cx| on_click(window, cx)),
    )
}

/// Risolvi: apre (attivandone finestra e tab) il progetto della notifica, crea una chat
/// claude nel suo kanban in Nuove con il prompt della notifica e, quando la chat c'e'
/// davvero, la segna "in lavorazione"; se la chat di un Risolvi precedente c'e' ancora
/// porta li'. Progetto non aperto in nessuna finestra → toast nella finestra di `mw`, la
/// notifica resta nuova.
pub fn resolve_notification(mw: &Entity<MultiWorkspace>, id: &str, _window: &mut Window, cx: &mut App) {
    // rinviato: chi chiama e' dentro l'update della propria finestra, che cosi' non si
    // potrebbe ne' leggere ne' aggiornare cercando il progetto fra tutte le finestre
    let (id, fallback) = (id.to_string(), mw.downgrade());
    cx.defer(move |cx| resolve_in_any_window(&id, Some(fallback), cx));
}

/// Progetto aperto in una tab di una finestra qualunque.
struct ResolveTarget {
    window: WindowHandle<MultiWorkspace>,
    tab_ix: usize,
    workspace: Entity<Workspace>,
    canvas: Entity<CanvasView>,
}

/// Cerca la tab del progetto `root` in tutte le finestre, prima in quella di `preferred`.
fn find_project(root: &Path, preferred: Option<&Entity<MultiWorkspace>>, cx: &App) -> Option<ResolveTarget> {
    let mut windows: Vec<(WindowHandle<MultiWorkspace>, Entity<MultiWorkspace>)> = cx
        .windows()
        .into_iter()
        .filter_map(|w| w.downcast::<MultiWorkspace>())
        .filter_map(|w| Some((w, w.entity(cx).ok()?)))
        .collect();
    windows.sort_by_key(|(_, mw)| Some(mw) != preferred); // stabile
    windows.into_iter().find_map(|(window, mw)| {
        project_tabs(mw.read(cx), cx).into_iter().enumerate().find_map(|(tab_ix, tab)| {
            let workspace = tab.workspace?;
            let canvas = canvas_for(workspace.read(cx))?;
            (canvas.read(cx).project_root_path(cx).as_deref() == Some(root))
                .then(|| ResolveTarget { window, tab_ix, workspace: workspace.clone(), canvas })
        })
    })
}

/// Toast "progetto non aperto" nella finestra di `fallback` (o nella prima).
fn show_not_open_toast(fallback: Option<Entity<MultiWorkspace>>, cx: &mut App) {
    let Some(mw) = fallback.or_else(|| multi_workspace_window(cx)) else {
        return;
    };
    let active = mw.read(cx).workspace().clone();
    active.update(cx, |w, cx| {
        w.show_toast(Toast::new(NotificationId::named("superzed-notification-not-open".into()), "Project not open"), cx)
    });
}

/// Attiva finestra e tab di `target`, poi (dopo il ridisegno: il workspace appena
/// attivato e' nell'albero) chiama `then` sul suo canvas.
fn activate_target(target: ResolveTarget, cx: &mut App, then: impl FnOnce(Entity<CanvasView>, &mut Window, &mut App) + 'static) {
    let ResolveTarget { window, tab_ix, workspace, canvas } = target;
    window
        .update(cx, |mw, window, cx| {
            window.activate_window();
            if mw.workspace() != &workspace {
                activate_project_tab(mw, tab_ix, window, cx);
            }
            window.defer(cx, move |window, cx| then(canvas, window, cx));
        })
        .ok();
}

/// Il Risolvi vero e proprio, fuori dall'update di qualunque finestra (anche dalla
/// risposta a una notifica di sistema). Una notifica con un'azione propria fa quella.
/// `fallback`: dove mostrare il toast "progetto non aperto" (altrimenti la prima finestra).
pub fn resolve_in_any_window(id: &str, fallback: Option<WeakEntity<MultiWorkspace>>, cx: &mut App) {
    let Some(store) = NotificationStore::global(cx) else {
        return;
    };
    let Some(n) = store.read(cx).get(id).cloned() else {
        return;
    };
    if n.action.is_some() {
        action_in_any_window(id, fallback, cx);
        return;
    }
    let fallback = fallback.and_then(|w| w.upgrade());
    let Some(target) = n.project_root.as_deref().and_then(|root| find_project(root, fallback.as_ref(), cx)) else {
        show_not_open_toast(fallback, cx);
        return;
    };
    let id = n.id.clone();
    // gia' presa in carico e la sua chat c'e' ancora (in questo progetto): si va li'
    let existing = cx
        .try_global::<ResolvedChats>()
        .and_then(|chats| chats.0.get(&id))
        .filter(|(c, _)| c == &target.canvas.downgrade())
        .and_then(|(_, key)| target.canvas.read(cx).node_for_session_key(key));
    let prompt = superzed_notifications::resolve_prompt(&n);
    activate_target(target, cx, move |canvas, window, cx| {
        if let Some(node) = existing {
            canvas.update(cx, |c, cx| {
                c.focus_node(node, window, cx);
                c.reveal(node, cx);
            });
            return;
        }
        let key = canvas.update(cx, |c, cx| c.new_claude_in_kanban_with_prompt(Some(prompt), window, cx));
        mark_taken_when_created(canvas, key, id, cx);
    });
}

/// Pulsante dell'azione propria di una notifica (es. «Rivedi»).
pub fn run_notification_action(mw: &Entity<MultiWorkspace>, id: &str, _window: &mut Window, cx: &mut App) {
    // rinviato come Risolvi: chi chiama e' dentro l'update della propria finestra
    let (id, fallback) = (id.to_string(), mw.downgrade());
    cx.defer(move |cx| action_in_any_window(&id, Some(fallback), cx));
}

/// L'azione nel canvas del progetto della notifica; la notifica non cambia stato.
pub fn action_in_any_window(id: &str, fallback: Option<WeakEntity<MultiWorkspace>>, cx: &mut App) {
    let Some(n) = NotificationStore::global(cx).and_then(|s| s.read(cx).get(id).cloned()) else {
        return;
    };
    let Some(action) = n.action.clone() else {
        return;
    };
    let fallback = fallback.and_then(|w| w.upgrade());
    let Some(target) = n.project_root.as_deref().and_then(|root| find_project(root, fallback.as_ref(), cx)) else {
        show_not_open_toast(fallback, cx);
        return;
    };
    activate_target(target, cx, move |canvas, window, cx| {
        canvas.update(cx, |c, cx| c.open_notification_action(&action, window, cx));
    });
}

/// Chat aperta dal Risolvi di ogni notifica (id → canvas e chiave di sessione), per
/// tornarci con un secondo Risolvi. Solo in memoria: dopo un riavvio se ne apre una nuova.
#[derive(Default)]
struct ResolvedChats(HashMap<String, (WeakEntity<CanvasView>, String)>);

impl Global for ResolvedChats {}

/// "In lavorazione" solo quando la chat di questa notifica (chiave di sessione `key`)
/// compare davvero sul canvas: il PTY parte in modo asincrono e puo' fallire (allora la
/// notifica resta nuova); altri nodi aperti nel frattempo non contano.
pub(crate) fn mark_taken_when_created(canvas: Entity<CanvasView>, key: String, id: String, cx: &mut App) {
    let slot: Rc<RefCell<Option<Subscription>>> = Rc::default();
    let sub = cx.observe(&canvas, {
        let slot = slot.clone();
        move |canvas, cx| {
            if canvas.read(cx).node_for_session_key(&key).is_none() {
                return;
            }
            if let Some(store) = NotificationStore::global(cx) {
                store.update(cx, |s, cx| {
                    s.set_state(&id, NotificationState::Taken, cx);
                });
            }
            cx.default_global::<ResolvedChats>().0.insert(id.clone(), (canvas.downgrade(), key.clone()));
            // l'iscrizione si toglie dopo, non dentro la sua stessa callback
            let done = slot.borrow_mut().take();
            cx.defer(move |_| drop(done));
        }
    });
    *slot.borrow_mut() = Some(sub);
}

/// Dettagli: apre il `.md` della notifica nell'editor del workspace.
pub fn open_details(workspace: &Entity<Workspace>, path: PathBuf, window: &mut Window, cx: &mut App) {
    workspace.update(cx, |ws, cx| {
        ws.open_abs_path(path, OpenOptions { visible: Some(OpenVisible::None), ..Default::default() }, window, cx)
            .detach_and_log_err(cx)
    });
}

/// Eseguibile dentro un bundle `.app` (solo li' macOS consegna le notifiche di sistema).
pub fn is_app_bundle(exe: &Path) -> bool {
    exe.to_string_lossy().contains(".app/Contents/MacOS/")
}

/// Errore e critico: notifica di sistema (solo da bundle: altrimenti macOS la ignora)
/// o toast nella finestra attiva, entrambi con "Risolvi" (o l'etichetta dell'azione propria).
pub fn alert(n: &Notification, bundled: bool, cx: &mut App) {
    let project = project_label(n.project_root.as_deref());
    // un'azione propria (es. «Rivedi») al posto di «Risolvi»: il clic passa da Risolvi, che la esegue
    let label = n.action.as_ref().map(|a| a.display_label()).unwrap_or_else(|| "Resolve".to_string());
    if bundled {
        cx.show_system_notification(SystemNotification {
            tag: n.id.clone().into(),
            title: format!("superzed · {project}").into(),
            body: n.title.clone().into(),
            actions: vec![SystemNotificationAction { id: RESOLVE_ACTION.into(), label: label.clone().into() }],
        });
        return;
    }
    let id = n.id.clone();
    let message = format!("{} · {}: {}", project, n.severity.label(), n.title);
    // rinviato: chi aggiunge la notifica puo' essere dentro l'update della finestra,
    // che allora non si potrebbe leggere per trovarne il MultiWorkspace
    cx.defer(move |cx| {
        let Some(mw) = multi_workspace_window(cx) else {
            return;
        };
        let workspace = mw.read(cx).workspace().clone();
        let weak_mw = mw.downgrade();
        workspace.update(cx, |ws, cx| {
            ws.show_toast(
                Toast::new(NotificationId::named(format!("superzed-notification-{id}").into()), message).on_click(
                    label,
                    move |window, cx| {
                        if let Some(mw) = weak_mw.upgrade() {
                            resolve_notification(&mw, &id, window, cx);
                        }
                    },
                ),
                cx,
            )
        });
    });
}

/// Il `MultiWorkspace` della finestra attiva, o della prima finestra se nessuna e'
/// attiva (nei test).
fn multi_workspace_window(cx: &mut App) -> Option<Entity<MultiWorkspace>> {
    let handles = cx.active_window().into_iter().chain(cx.windows());
    for handle in handles {
        if let Some(handle) = handle.downcast::<MultiWorkspace>()
            && let Ok(mw) = handle.entity(cx)
        {
            return Some(mw);
        }
    }
    None
}

impl EventEmitter<PanelEvent> for NotificationsPanel {}

impl Focusable for NotificationsPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for NotificationsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // archivio creato dopo il pannello (non succede nell'app, ma costa poco)
        if self.store_sub.is_none()
            && let Some(store) = NotificationStore::global(cx)
        {
            self.store_sub = Some(cx.observe(&store, |_, _, cx| cx.notify()));
        }
        let cards = self.cards(cx);
        let empty = self.empty_message_for(&cards, cx);
        let (all, show) = (self.all_projects, self.show_archived);
        let toolbar = h_flex()
            .px_2()
            .pt_2()
            .gap_2()
            .flex_wrap()
            .child(
                // entrambi descrivono lo stato attuale; il clic lo cambia
                Button::new("superzed-notifications-scope", if all { "All projects" } else { "This project only" })
                    .on_click(cx.listener(move |p, _, _, cx| p.set_all_projects(!all, cx))),
            )
            .child(
                Button::new("superzed-notifications-archived", if show { "Archived shown" } else { "Archived hidden" })
                    .on_click(cx.listener(move |p, _, _, cx| p.set_show_archived(!show, cx))),
            );
        let cards: Vec<_> = cards.into_iter().map(|n| self.render_card(n, cx)).collect();
        v_flex()
            .key_context("SuperzedNotifications")
            .track_focus(&self.focus)
            .size_full()
            .child(toolbar)
            .child(
                v_flex()
                    .id("superzed-notifications-list")
                    .flex_1()
                    .overflow_y_scroll()
                    .when_some(empty, |d, text| {
                        d.child(div().debug_selector(|| "superzed-notifications-empty".into()).p_3().child(Label::new(text).color(Color::Muted)))
                    })
                    .children(cards),
            )
    }
}

impl Panel for NotificationsPanel {
    fn persistent_name() -> &'static str {
        "SuperzedNotificationsPanel"
    }
    fn panel_key() -> &'static str {
        "SuperzedNotificationsPanel"
    }
    // nel dock destro, come il pannello Git: il pulsante sta in basso a destra accanto a Git
    fn position(&self, _: &Window, _: &App) -> DockPosition {
        DockPosition::Right
    }
    fn position_is_valid(&self, position: DockPosition) -> bool {
        position == DockPosition::Right
    }
    fn set_position(&mut self, _: DockPosition, _: &mut Window, _: &mut Context<Self>) {}
    fn default_size(&self, _: &Window, _: &App) -> Pixels {
        px(280.)
    }
    fn icon(&self, _: &Window, _: &App) -> Option<IconName> {
        Some(IconName::Bell)
    }
    fn icon_tooltip(&self, _: &Window, _: &App) -> Option<&'static str> {
        Some("Notifications")
    }
    /// Notifiche nuove del progetto corrente (niente se zero o senza cartella).
    fn icon_label(&self, _: &Window, cx: &App) -> Option<String> {
        let project = self.current_project(cx)?;
        let count = NotificationStore::global(cx)?.read(cx).count_new(Some(&project));
        (count > 0).then(|| count.to_string())
    }
    fn toggle_action(&self) -> Box<dyn Action> {
        Box::new(SidebarView(5))
    }
    // subito dopo Git (3) nel dock destro
    fn activation_priority(&self) -> u32 {
        4
    }
}
