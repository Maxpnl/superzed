//! Nodo Posta: le email del progetto (query Gmail salvata in project.json) lette via
//! `gog`, il thread selezionato e, da un thread, una chat claude nel kanban. Un nodo per
//! progetto, che il canvas non lascia chiudere.
use crate::node::NodeContent;
use chrono::{DateTime, Local};
use editor::Editor;
use gpui::{
    AnyElement, AnyView, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    IntoElement, Render, SharedString, Subscription, Task, Window,
};
use std::{sync::Arc, time::Duration};
use superzed_sources::{
    Draft, GmailClient, MailThread, Runner, ThreadSummary, chat_prompt,
    gmail::{SEARCH_MAX, project_query},
};
use ui::{IconButton, IconName, IconSize, Label, LabelSize, Tooltip, prelude::*, utils::WithRemSize};

pub const MAIL_SIZE: (f32, f32) = (900., 560.);
pub const MAIL_TITLE: &str = "mail";
pub const REFRESH_EVERY: Duration = Duration::from_secs(5 * 60);
const NOT_CONFIGURED: &str = "Set the account and the project query, then press Enter";

#[derive(Clone, Debug, PartialEq)]
pub enum MailPanelEvent {
    /// Account o query cambiati: il canvas salva.
    SettingsChanged,
    OpenChat { prompt: String },
    PrepareDraft { prompt: String },
}

#[derive(Clone, Debug, PartialEq)]
enum MailStatus {
    NotConfigured,
    Idle,
    Loading,
    Error(String),
}

pub fn mail_data(data: &serde_json::Value) -> (String, String) {
    let field = |key: &str| data.get(key).and_then(|v| v.as_str()).unwrap_or_default().to_string();
    (field("account"), field("query"))
}

/// Testo del thread per una chat: oggetto, poi ogni messaggio con mittente e data.
pub fn thread_context(thread: &MailThread) -> String {
    let mut out = format!("Oggetto: {}\nThread Gmail: {}\n", thread.subject, thread.id);
    for message in &thread.messages {
        out.push_str(&format!("\n--- Da: {} · {} · id {}\n", message.from, message.date, message.id));
        if !message.attachments.is_empty() {
            out.push_str(&format!("Allegati: {}\n", message.attachments.join(", ")));
        }
        out.push_str(message.body.trim_end());
        out.push('\n');
    }
    out
}

/// Claude scrive solo la bozza (le chat hanno `GOG_GMAIL_NO_SEND`): l'invio resta al nodo.
pub fn draft_prompt(account: &str, thread: &MailThread) -> String {
    let last_message = thread.messages.last().map(|m| m.id.as_str()).unwrap_or_default();
    let context = chat_prompt("Gmail", &thread_context(thread));
    format!(
        "{context}\n\nPrepara la risposta a questo thread. Prima guarda il repo e la board del progetto per capire lo stato reale di quello che chiedono; scrivi nella lingua del thread, breve e concreta. Poi crea la bozza su Gmail passando il testo su stdin:\n\n`gog --account {account} gmail drafts reply {last_message} --no-quote --body-file -`\n\nNon inviare: l'invio lo faccio io dal nodo Posta dopo averla riletta. Se mancano informazioni per rispondere, non creare la bozza e dimmi cosa manca."
    )
}

pub struct MailPanelView {
    account: Entity<Editor>,
    query: Entity<Editor>,
    search: Entity<Editor>,
    saved_account: String,
    saved_query: String,
    runner: Arc<dyn Runner>,
    version_checked: bool,
    threads: Vec<ThreadSummary>,
    selected: Option<String>,
    thread: Option<MailThread>,
    status: MailStatus,
    refreshed_at: Option<DateTime<Local>>,
    focus: FocusHandle,
    pub(crate) zoom: f32,
    loading: Option<Task<()>>,
    /// Account e query della ricerca in corso: solo una richiesta identica viene saltata.
    loading_key: Option<(String, String)>,
    thread_loading: Option<Task<()>>,
    reply: Entity<Editor>,
    reply_all: bool,
    /// Primo clic su «Send…»: si mostrano i destinatari e si invia solo con «Send now».
    confirm_send: bool,
    sending: Option<Task<()>>,
    drafts: Vec<Draft>,
    /// Bozza caricata nell'editor e il suo testo originale: se non cambia si invia com'e'.
    loaded_draft: Option<(Draft, String)>,
    _ticker: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<MailPanelEvent> for MailPanelView {}

fn single_line(text: &str, placeholder: &str, window: &mut Window, cx: &mut App) -> Entity<Editor> {
    cx.new(|cx| {
        let mut editor = Editor::single_line(window, cx);
        editor.set_placeholder_text(placeholder, window, cx);
        editor.set_text(text, window, cx);
        editor
    })
}

impl MailPanelView {
    pub fn new(account: String, query: String, runner: Arc<dyn Runner>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let account_editor = single_line(&account, "account (j.doe@example.com)", window, cx);
        let query_editor = single_line(&query, "project query (from:@acme.example OR label:atlas)", window, cx);
        let search = single_line("", "search in this project's mail", window, cx);
        let reply = cx.new(|cx| {
            let mut editor = Editor::auto_height(3, 12, window, cx);
            editor.set_placeholder_text("Reply…", window, cx);
            editor
        });
        let ticker = cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(REFRESH_EVERY).await;
            if this.update(cx, |v, cx| v.refresh(cx)).is_err() {
                break;
            }
        });
        let mut view = Self {
            account: account_editor,
            query: query_editor,
            search,
            saved_account: account,
            saved_query: query,
            runner,
            version_checked: false,
            threads: Vec::new(),
            selected: None,
            thread: None,
            status: MailStatus::NotConfigured,
            refreshed_at: None,
            focus: cx.focus_handle(),
            zoom: 1.,
            loading: None,
            loading_key: None,
            thread_loading: None,
            reply,
            reply_all: false,
            confirm_send: false,
            sending: None,
            drafts: Vec::new(),
            loaded_draft: None,
            _ticker: ticker,
            _subscriptions: Vec::new(),
        };
        view.refresh(cx);
        view
    }

    pub fn account_editor(&self) -> &Entity<Editor> {
        &self.account
    }

    pub fn query_editor(&self) -> &Entity<Editor> {
        &self.query
    }

    pub fn search_editor(&self) -> &Entity<Editor> {
        &self.search
    }

    pub fn threads(&self) -> &[ThreadSummary] {
        &self.threads
    }

    pub fn shown_thread(&self) -> Option<&MailThread> {
        self.thread.as_ref()
    }

    pub fn settings(&self) -> (String, String) {
        (self.saved_account.clone(), self.saved_query.clone())
    }

    pub fn save_data(&self) -> serde_json::Value {
        serde_json::json!({"account": self.saved_account, "query": self.saved_query})
    }

    pub fn status_text(&self) -> String {
        match &self.status {
            MailStatus::NotConfigured => NOT_CONFIGURED.to_string(),
            MailStatus::Idle => self.refreshed_at.map(|t| format!("updated at {}", t.format("%H:%M"))).unwrap_or_default(),
            MailStatus::Loading => "updating…".to_string(),
            MailStatus::Error(message) => message.clone(),
        }
    }

    pub(crate) fn client(&self) -> Option<GmailClient> {
        let account = self.saved_account.trim();
        (!account.is_empty()).then(|| GmailClient::new(self.runner.clone(), account))
    }

    /// Account e query del progetto si applicano con Invio (non a ogni tasto: ogni
    /// modifica sarebbe una ricerca su Gmail).
    pub(crate) fn apply_settings(&mut self, cx: &mut Context<Self>) {
        let account = self.account.read(cx).text(cx).trim().to_string();
        let query = self.query.read(cx).text(cx).trim().to_string();
        if account != self.saved_account || query != self.saved_query {
            // una conferma in sospeso non deve rispondere a un id dell'account di prima
            if account != self.saved_account {
                self.selected = None;
                self.thread = None;
                self.thread_loading = None;
                self.confirm_send = false;
                self.loaded_draft = None;
                self.drafts.clear();
                self.clear_reply(cx);
            }
            self.saved_account = account;
            self.saved_query = query;
            self.version_checked = false;
            cx.emit(MailPanelEvent::SettingsChanged);
        }
        self.refresh(cx);
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let search = self.search.read(cx).text(cx);
        let (Some(client), Some(query)) = (self.client(), project_query(&self.saved_query, &search)) else {
            self.loading = None;
            self.loading_key = None;
            self.threads.clear();
            self.selected = None;
            self.thread = None;
            self.thread_loading = None;
            self.drafts.clear();
            self.loaded_draft = None;
            self.status = MailStatus::NotConfigured;
            cx.notify();
            return;
        };
        // una ricerca in corso non ne fa partire un'altra uguale; se account o query sono
        // cambiati, sostituirla annulla quella vecchia, che non sovrascrive piu' le righe
        let key = (client.account().to_string(), query.clone());
        if self.loading.is_some() && self.loading_key.as_ref() == Some(&key) {
            return;
        }
        self.loading_key = Some(key);
        let check_version = !self.version_checked;
        self.status = MailStatus::Loading;
        cx.notify();
        self.loading = Some(cx.spawn(async move |this, cx| {
            let result = async {
                if check_version {
                    client.check_version().await?;
                }
                let threads = client.search(&query, SEARCH_MAX).await?;
                // senza le bozze la posta resta leggibile: l'errore va solo nel log
                let drafts = client.drafts().await.unwrap_or_else(|error| {
                    log::info!("gmail: bozze non lette: {error}");
                    Vec::new()
                });
                Ok((threads, drafts))
            }
            .await;
            this.update(cx, |v, cx| {
                match result {
                    Ok((threads, drafts)) => {
                        v.version_checked = true;
                        v.threads = threads;
                        v.drafts = drafts;
                        v.status = MailStatus::Idle;
                        v.refreshed_at = Some(Local::now());
                    }
                    Err(message) => v.status = MailStatus::Error(message),
                }
                v.loading = None;
                v.loading_key = None;
                cx.notify();
            })
            .ok();
        }));
    }

    pub fn select(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(client) = self.client() else {
            return;
        };
        // il thread mostrato segue la selezione: «apri chat» non manda il thread di prima
        // mentre il nuovo si carica o se non si legge
        if self.selected.as_deref() != Some(id.as_str()) {
            self.thread = None;
            self.confirm_send = false;
            self.loaded_draft = None;
            self.clear_reply(cx);
        }
        self.selected = Some(id.clone());
        let draft = self.drafts.iter().find(|draft| draft.thread_id == id).cloned();
        // riaprire lo stesso thread non sostituisce le modifiche gia' fatte alla bozza
        let keep_loaded = draft
            .as_ref()
            .is_some_and(|draft| self.loaded_draft.as_ref().is_some_and(|(loaded, _)| loaded.id == draft.id));
        let draft = draft.filter(|_| !keep_loaded);
        cx.notify();
        // il task precedente si scarta: una risposta vecchia non sovrascrive la selezione nuova
        self.thread_loading = Some(cx.spawn(async move |this, cx| {
            let result = client.readable_thread(&id).await;
            let draft = match draft {
                Some(draft) if result.is_ok() => match client.draft_body(&draft).await {
                    Ok(body) => Some((draft, body)),
                    Err(error) => {
                        log::info!("gmail: bozza {} non letta: {error}", draft.id);
                        None
                    }
                },
                _ => None,
            };
            this.update(cx, |v, cx| {
                if v.selected.as_deref() != Some(id.as_str()) {
                    return;
                }
                match result {
                    Ok(thread) => {
                        if thread.messages.iter().any(|message| message.unread) {
                            v.mark_read(client, &id, cx);
                        }
                        v.thread = Some(thread);
                        if !keep_loaded {
                            v.show_draft(draft, cx);
                        }
                    }
                    Err(message) => v.status = MailStatus::Error(message),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Segnare letto non deve bloccare la lettura: se gog fallisce resta solo nel log.
    fn mark_read(&mut self, client: GmailClient, id: &str, cx: &mut Context<Self>) {
        if let Some(row) = self.threads.iter_mut().find(|row| row.id == id) {
            row.unread = false;
        }
        let id = id.to_string();
        cx.spawn(async move |_, _| {
            if let Err(error) = client.mark_thread_read(&id).await {
                log::info!("gmail: thread {id} non segnato come letto: {error}");
            }
        })
        .detach();
    }

    fn show_draft(&mut self, draft: Option<(Draft, String)>, cx: &mut Context<Self>) {
        let Some((draft, body)) = draft else {
            if self.loaded_draft.take().is_some() {
                self.confirm_send = false;
                self.clear_reply(cx);
            }
            return;
        };
        self.confirm_send = false;
        self.set_reply_text(&body, cx);
        self.loaded_draft = Some((draft, body));
    }

    fn clear_reply(&mut self, cx: &mut Context<Self>) {
        self.set_reply_text("", cx);
    }

    fn set_reply_text(&mut self, text: &str, cx: &mut Context<Self>) {
        // senza `Window` (selezione e fine invio arrivano da task): si scrive nel buffer
        if let Some(buffer) = self.reply.read(cx).buffer().read(cx).as_singleton() {
            buffer.update(cx, |buffer, cx| buffer.set_text(text, cx));
        }
    }

    pub fn loaded_draft(&self) -> Option<&Draft> {
        self.loaded_draft.as_ref().map(|(draft, _)| draft)
    }

    pub fn reply_editor(&self) -> &Entity<Editor> {
        &self.reply
    }

    pub fn set_reply_all(&mut self, all: bool, cx: &mut Context<Self>) {
        self.reply_all = all;
        self.confirm_send = false;
        cx.notify();
    }

    /// Destinatari mostrati prima dell'invio: gog li ricava allo stesso modo (Reply-To se
    /// c'e', altrimenti From; a tutti aggiunge To e Cc).
    pub fn send_summary(&self) -> Option<String> {
        if !self.confirm_send {
            return None;
        }
        if self.loaded_draft.is_some() {
            return Some("Send Claude's draft (to the thread's recipients)".to_string());
        }
        let last = self.thread.as_ref()?.messages.last()?;
        let to = if last.reply_to.is_empty() { last.from.clone() } else { last.reply_to.clone() };
        Some(if self.reply_all {
            let recipients: Vec<&str> = [to.as_str(), last.to.as_str(), last.cc.as_str()]
                .into_iter()
                .filter(|recipients| !recipients.is_empty())
                .collect();
            format!("Reply all to {}", recipients.join(", "))
        } else {
            format!("Reply to {to}")
        })
    }

    pub fn request_send(&mut self, cx: &mut Context<Self>) {
        if self.reply.read(cx).text(cx).trim().is_empty() || self.thread.is_none() {
            return;
        }
        self.confirm_send = true;
        cx.notify();
    }

    pub fn cancel_send(&mut self, cx: &mut Context<Self>) {
        self.confirm_send = false;
        cx.notify();
    }

    pub fn confirm_send(&mut self, cx: &mut Context<Self>) {
        if !self.confirm_send || self.sending.is_some() {
            return;
        }
        let (Some(client), Some(thread)) = (self.client(), self.thread.clone()) else {
            return;
        };
        let Some(last) = thread.messages.last().cloned() else {
            return;
        };
        let body = self.reply.read(cx).text(cx);
        let reply_all = self.reply_all;
        let draft = self.loaded_draft.clone().filter(|(draft, _)| draft.thread_id == thread.id);
        self.confirm_send = false;
        self.sending = Some(cx.spawn(async move |this, cx| {
            let result = match &draft {
                Some((draft, original)) => {
                    async {
                        if body != *original {
                            client.update_draft(draft, &body).await?;
                        }
                        client.send_draft(&draft.id).await
                    }
                    .await
                }
                None => client.reply(&last.id, &body, reply_all).await,
            };
            this.update(cx, |v, cx| {
                v.sending = None;
                match result {
                    // se nel frattempo e' cambiata la selezione l'editor e' gia' stato svuotato
                    Ok(()) => {
                        // l'errore di un invio precedente farebbe credere che questo non sia partito
                        if matches!(v.status, MailStatus::Error(_)) {
                            v.status = MailStatus::Idle;
                        }
                        if let Some((sent, _)) = &draft {
                            // la bozza inviata non va ricaricata finche' la lista non si aggiorna
                            v.drafts.retain(|draft| draft.id != sent.id);
                            if v.loaded_draft.as_ref().is_some_and(|(loaded, _)| loaded.id == sent.id) {
                                v.loaded_draft = None;
                            }
                            v.refresh(cx);
                        }
                        if v.selected.as_deref() == Some(thread.id.as_str()) {
                            v.clear_reply(cx);
                            v.select(thread.id.clone(), cx);
                        }
                    }
                    Err(message) => v.status = MailStatus::Error(message),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn open_chat(&self, cx: &mut Context<Self>) {
        if let Some(thread) = &self.thread {
            cx.emit(MailPanelEvent::OpenChat { prompt: chat_prompt("Gmail", &thread_context(thread)) });
        }
    }

    pub fn prepare_draft(&self, cx: &mut Context<Self>) {
        if let Some(thread) = &self.thread {
            cx.emit(MailPanelEvent::PrepareDraft { prompt: draft_prompt(self.saved_account.trim(), thread) });
        }
    }

    fn render_row(&self, thread: &ThreadSummary, cx: &mut Context<Self>) -> AnyElement {
        let id = thread.id.clone();
        let selected = self.selected.as_deref() == Some(thread.id.as_str());
        let date = thread.date.map(|d| d.with_timezone(&Local).format("%d/%m %H:%M").to_string()).unwrap_or_default();
        let weight = if thread.unread { FontWeight::BOLD } else { FontWeight::NORMAL };
        let has_draft = self.drafts.iter().any(|draft| draft.thread_id == thread.id);
        let colors = cx.theme().colors();
        let (selected_background, hover_background) = (colors.element_selected, colors.element_hover);
        v_flex()
            .id(SharedString::from(format!("superzed-mail-row-{}", thread.id)))
            .px_2()
            .py_1()
            .rounded_sm()
            .when(selected, |d| d.bg(selected_background))
            .hover(|d| d.bg(hover_background))
            .on_click(cx.listener(move |v, _, _, cx| v.select(id.clone(), cx)))
            .child(
                h_flex()
                    .gap_2()
                    .child(Label::new(thread.from.clone()).weight(weight).truncate())
                    .child(Label::new(date).size(LabelSize::Small).color(Color::Muted))
                    .when(has_draft, |d| d.child(Label::new("draft").size(LabelSize::XSmall).color(Color::Accent))),
            )
            .child(Label::new(thread.subject.clone()).size(LabelSize::Small).weight(weight).truncate())
            .into_any_element()
    }

    fn render_thread(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(thread) = &self.thread else {
            return Label::new("Select a thread").color(Color::Muted).into_any_element();
        };
        let border = cx.theme().colors().border_variant;
        let messages: Vec<AnyElement> = thread
            .messages
            .iter()
            .map(|message| {
                v_flex()
                    .gap_1()
                    .pb_2()
                    .border_b_1()
                    .border_color(border)
                    .child(Label::new(format!("{} · {}", message.from, message.date)).size(LabelSize::Small).color(Color::Muted))
                    .when(!message.attachments.is_empty(), |d| {
                        d.child(
                            Label::new(format!("Attachments: {}", message.attachments.join(", ")))
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        )
                    })
                    .child(div().whitespace_normal().child(message.body.clone()))
                    .into_any_element()
            })
            .collect();
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(Label::new(thread.subject.clone()).weight(FontWeight::BOLD).truncate())
                    .child(div().flex_1())
                    .child(
                        IconButton::new("superzed-mail-prepare-draft", IconName::Pencil)
                            .icon_size(IconSize::Medium)
                            .tooltip(Tooltip::text("Prepare a draft with Claude"))
                            .on_click(cx.listener(|v, _, _, cx| v.prepare_draft(cx))),
                    )
                    .child(
                        div().debug_selector(|| "superzed-mail-open-chat".into()).child(
                            IconButton::new("superzed-mail-open-chat", IconName::Sparkle)
                                .icon_size(IconSize::Medium)
                                .tooltip(Tooltip::text("Open a chat in the kanban"))
                                .on_click(cx.listener(|v, _, _, cx| v.open_chat(cx))),
                        ),
                    ),
            )
            .children(messages)
            .child(
                v_flex()
                    .gap_1()
                    .child(self.reply.clone())
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("superzed-mail-reply-all", "Reply all")
                                    .toggle_state(self.reply_all)
                                    .on_click(cx.listener(|v, _, _, cx| v.set_reply_all(!v.reply_all, cx))),
                            )
                            .child(div().flex_1())
                            .when_some(self.send_summary(), |d, summary| {
                                d.child(Label::new(summary).size(LabelSize::Small))
                                    .child(
                                        Button::new("superzed-mail-cancel", "Cancel")
                                            .on_click(cx.listener(|v, _, _, cx| v.cancel_send(cx))),
                                    )
                                    .child(
                                        Button::new("superzed-mail-confirm", "Send now")
                                            .on_click(cx.listener(|v, _, _, cx| v.confirm_send(cx))),
                                    )
                            })
                            .when(!self.confirm_send, |d| {
                                d.child(
                                    Button::new("superzed-mail-send", "Send…")
                                        .disabled(self.sending.is_some())
                                        .on_click(cx.listener(|v, _, _, cx| v.request_send(cx))),
                                )
                            }),
                    ),
            )
            .into_any_element()
    }
}

impl Focusable for MailPanelView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for MailPanelView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let status_color = match self.status {
            MailStatus::Error(_) => Color::Error,
            MailStatus::NotConfigured => Color::Warning,
            _ => Color::Muted,
        };
        let header = v_flex()
            .px_2()
            .py_1()
            .gap_1()
            .child(
                h_flex()
                    .gap_2()
                    .child(div().w(px(220.)).child(self.account.clone()))
                    .child(div().flex_1().child(self.query.clone()))
                    .child(
                        IconButton::new("superzed-mail-refresh", IconName::RotateCw)
                            .icon_size(IconSize::Medium)
                            .tooltip(Tooltip::text("Refresh"))
                            .on_click(cx.listener(|v, _, _, cx| v.refresh(cx))),
                    ),
            )
            .child(
                Label::new("The project query is saved in .superzed/project.json, which may be committed")
                    .size(LabelSize::XSmall)
                    .color(Color::Muted),
            )
            .child(
                h_flex().gap_2().child(div().flex_1().child(self.search.clone())).child(
                    div()
                        .debug_selector(|| "superzed-mail-status".into())
                        .child(Label::new(self.status_text()).size(LabelSize::Small).color(status_color).truncate()),
                ),
            );
        let rows: Vec<AnyElement> = self.threads.iter().map(|thread| self.render_row(thread, cx)).collect();
        let thread = self.render_thread(cx);
        WithRemSize::new(window.rem_size() * self.zoom).size_full().child(
            v_flex()
                .key_context("SuperzedMailPanel")
                .track_focus(&self.focus)
                .on_action(cx.listener(|v, _: &menu::Confirm, _, cx| v.apply_settings(cx)))
                .size_full()
                .bg(cx.theme().colors().panel_background)
                .child(header)
                .child(
                    h_flex()
                        .flex_1()
                        .min_h_0()
                        .items_start()
                        .child(
                            v_flex()
                                .id("superzed-mail-rows")
                                .w(px(300.))
                                .h_full()
                                .overflow_y_scroll()
                                .px_1()
                                .gap_1()
                                .children(rows),
                        )
                        .child(v_flex().id("superzed-mail-thread").flex_1().h_full().overflow_y_scroll().px_2().child(thread)),
                ),
        )
    }
}

/// Contenuto del nodo `NodeKind::Mail`.
pub struct MailNode {
    view: Entity<MailPanelView>,
}

impl MailNode {
    pub fn new(view: Entity<MailPanelView>) -> Self {
        Self { view }
    }

    pub fn view_entity(&self) -> &Entity<MailPanelView> {
        &self.view
    }
}

impl NodeContent for MailNode {
    fn view(&self) -> AnyView {
        self.view.clone().into()
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.view.focus_handle(cx)
    }

    fn set_font_zoom(&self, zoom: f32, cx: &mut App) {
        self.view.update(cx, |v, cx| {
            v.zoom = zoom;
            cx.notify();
        });
    }

    fn save_data(&self, cx: &App) -> serde_json::Value {
        self.view.read(cx).save_data()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    use std::{cell::RefCell, rc::Rc};
    use superzed_sources::test_support::{FakeRunner, gmail_runner};

    fn panel<'a>(
        cx: &'a mut TestAppContext,
        account: &str,
        query: &str,
        runner: Arc<FakeRunner>,
    ) -> (Entity<MailPanelView>, &'a mut VisualTestContext) {
        crate::test_support::init_test(cx);
        let (account, query) = (account.to_string(), query.to_string());
        let (view, cx) = cx.add_window_view(move |window, cx| MailPanelView::new(account, query, runner, window, cx));
        cx.run_until_parked();
        (view, cx)
    }

    fn events(view: &Entity<MailPanelView>, cx: &mut VisualTestContext) -> Rc<RefCell<Vec<MailPanelEvent>>> {
        let got = Rc::new(RefCell::new(Vec::new()));
        cx.update(|_, cx| {
            let got = got.clone();
            cx.subscribe(view, move |_, event: &MailPanelEvent, _| got.borrow_mut().push(event.clone())).detach();
        });
        got
    }

    #[gpui::test]
    async fn without_a_query_gog_is_never_called(cx: &mut TestAppContext) {
        let runner = gmail_runner();
        let (view, cx) = panel(cx, "j.doe@example.com", "  ", runner.clone());
        cx.executor().advance_clock(REFRESH_EVERY);
        cx.run_until_parked();
        assert_eq!(runner.calls().len(), 0);
        assert!(view.read_with(cx, |v, _| v.status_text()).contains("Set the account and the project query"));
    }

    #[gpui::test]
    async fn the_node_search_narrows_the_project_query(cx: &mut TestAppContext) {
        let runner = gmail_runner();
        let (view, cx) = panel(cx, "j.doe@example.com", "from:@acme.example OR label:atlas", runner.clone());
        assert_eq!(view.read_with(cx, |v, _| v.threads().len()), 2);
        view.update_in(cx, |v, window, cx| {
            v.search_editor().update(cx, |e, cx| e.set_text("policy", window, cx));
            v.refresh(cx);
        });
        cx.run_until_parked();
        let last = runner.calls().into_iter().rev().find(|c| c.line().contains("gmail search")).unwrap();
        assert_eq!(last.args.last().map(String::as_str), Some("(from:@acme.example OR label:atlas) policy"));
    }

    #[gpui::test]
    async fn refreshes_every_five_minutes(cx: &mut TestAppContext) {
        let runner = gmail_runner();
        let (_view, cx) = panel(cx, "j.doe@example.com", "from:@acme.example", runner.clone());
        assert_eq!(runner.count("gmail search"), 1);
        cx.executor().advance_clock(REFRESH_EVERY);
        cx.run_until_parked();
        assert_eq!(runner.count("gmail search"), 2);
    }

    #[gpui::test]
    async fn missing_gog_shows_the_install_command(cx: &mut TestAppContext) {
        let runner = FakeRunner::new();
        runner.on("gog --version", Err(superzed_sources::RunError::NotFound("gog".into())));
        let (view, cx) = panel(cx, "j.doe@example.com", "from:@acme.example", runner);
        assert_eq!(view.read_with(cx, |v, _| v.status_text()), superzed_sources::gmail::GOG_MISSING);
    }

    #[gpui::test]
    async fn a_late_answer_for_an_old_selection_is_dropped(cx: &mut TestAppContext) {
        let runner = gmail_runner();
        runner.on(
            "gmail thread get t-100",
            superzed_sources::test_support::ok(
                r#"{"thread":{"id":"t-100","messages":[{"id":"m-101","headers":{"subject":"Atlas: listini"},"body":"x"}]}}"#,
            ),
        );
        runner.on("gmail get m-101", superzed_sources::test_support::ok(r#"{"body":"x"}"#));
        let (view, cx) = panel(cx, "j.doe@example.com", "from:@acme.example", runner);
        // due selezioni senza parcheggiare: vince l'ultima
        view.update(cx, |v, cx| {
            v.select("t-200".into(), cx);
            v.select("t-100".into(), cx);
        });
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, _| v.shown_thread().map(|t| t.id.clone())), Some("t-100".into()));
    }

    #[gpui::test]
    async fn settings_are_saved_and_open_chat_carries_the_thread(cx: &mut TestAppContext) {
        let runner = gmail_runner();
        let (view, cx) = panel(cx, "j.doe@example.com", "from:@acme.example", runner);
        assert_eq!(
            view.read_with(cx, |v, _| v.save_data()),
            serde_json::json!({"account": "j.doe@example.com", "query": "from:@acme.example"})
        );
        let got = events(&view, cx);
        view.update(cx, |v, cx| v.select("t-200".into(), cx));
        cx.run_until_parked();
        view.update(cx, |v, cx| v.open_chat(cx));
        let MailPanelEvent::OpenChat { prompt } = got.borrow().last().cloned().unwrap() else {
            panic!("atteso OpenChat")
        };
        assert!(prompt.contains("Policy di annullamento approvata"));
        assert!(prompt.contains("vale da lunedì"));
        assert!(prompt.contains(superzed_sources::DATA_START));
    }

    #[gpui::test]
    async fn new_settings_replace_a_search_in_flight(cx: &mut TestAppContext) {
        let runner = gmail_runner();
        let (view, cx) = panel(cx, "j.doe@example.com", "from:@acme.example", runner.clone());
        let got = events(&view, cx);
        // la prima ricerca resta in sospeso: nessun parcheggio prima del cambio di query
        view.update_in(cx, |v, window, cx| {
            v.search_editor().update(cx, |e, cx| e.set_text("policy", window, cx));
            v.refresh(cx);
            v.query.update(cx, |e, cx| e.set_text("label:atlas", window, cx));
            v.apply_settings(cx);
        });
        cx.run_until_parked();
        let searches: Vec<_> = runner.calls().into_iter().filter(|c| c.line().contains("gmail search")).collect();
        assert_eq!(searches.len(), 2, "la ricerca vecchia e' annullata");
        assert_eq!(searches.last().and_then(|c| c.args.last()).map(String::as_str), Some("(label:atlas) policy"));
        assert_eq!(got.borrow().as_slice(), &[MailPanelEvent::SettingsChanged]);
        assert_eq!(view.read_with(cx, |v, _| v.settings()), ("j.doe@example.com".into(), "label:atlas".into()));
    }

    #[gpui::test]
    async fn open_chat_waits_for_the_selected_thread(cx: &mut TestAppContext) {
        let runner = gmail_runner();
        let (view, cx) = panel(cx, "j.doe@example.com", "from:@acme.example", runner);
        let got = events(&view, cx);
        view.update(cx, |v, cx| v.select("t-200".into(), cx));
        cx.run_until_parked();
        // t-404 non ha risposta nel finto: la lettura fallisce
        view.update(cx, |v, cx| {
            v.select("t-404".into(), cx);
            v.open_chat(cx);
        });
        assert!(got.borrow().is_empty(), "nessuna chat mentre il nuovo thread si carica");
        cx.run_until_parked();
        view.update(cx, |v, cx| v.open_chat(cx));
        assert!(got.borrow().is_empty(), "nessuna chat se il nuovo thread non si legge");
        assert!(view.read_with(cx, |v, _| v.shown_thread().is_none()));
    }

    #[gpui::test]
    async fn send_asks_first_then_replies_to_the_last_message(cx: &mut TestAppContext) {
        let runner = gmail_runner();
        runner.on("gmail drafts list", superzed_sources::test_support::ok(r#"{"drafts":[]}"#));
        runner.on("gmail thread modify t-200", superzed_sources::test_support::ok("{}"));
        runner.on("gmail reply m-202", superzed_sources::test_support::ok("{}"));
        let (view, cx) = panel(cx, "j.doe@example.com", "from:@acme.example", runner.clone());
        view.update(cx, |v, cx| v.select("t-200".into(), cx));
        cx.run_until_parked();
        assert_eq!(runner.count("gmail thread modify t-200 --remove UNREAD"), 1, "aprire un thread non letto lo segna letto");
        view.update_in(cx, |v, window, cx| {
            v.reply_editor().update(cx, |e, cx| e.set_text("Grazie, procediamo.", window, cx));
            v.request_send(cx);
        });
        assert_eq!(runner.count("gmail reply"), 0, "il primo clic chiede conferma");
        assert_eq!(view.read_with(cx, |v, _| v.send_summary()), Some("Reply to team@example.com".into()));
        view.update(cx, |v, cx| v.confirm_send(cx));
        cx.run_until_parked();
        assert_eq!(runner.count("gmail reply m-202 --body-file -"), 1);
        assert_eq!(view.read_with(cx, |v, cx| v.reply_editor().read(cx).text(cx)), "");
    }

    #[gpui::test]
    async fn an_empty_reply_is_not_sent_and_a_failure_keeps_the_text(cx: &mut TestAppContext) {
        let runner = gmail_runner();
        runner.on("gmail drafts list", superzed_sources::test_support::ok(r#"{"drafts":[]}"#));
        runner.on("gmail thread modify t-200", superzed_sources::test_support::ok("{}"));
        runner.on("gmail reply m-202", superzed_sources::test_support::fail(6, "forbidden"));
        let (view, cx) = panel(cx, "j.doe@example.com", "from:@acme.example", runner.clone());
        view.update(cx, |v, cx| v.select("t-200".into(), cx));
        cx.run_until_parked();
        view.update(cx, |v, cx| v.request_send(cx));
        assert_eq!(view.read_with(cx, |v, _| v.send_summary()), None, "testo vuoto: nessuna conferma");
        view.update_in(cx, |v, window, cx| {
            v.reply_editor().update(cx, |e, cx| e.set_text("Testo", window, cx));
            v.request_send(cx);
            v.confirm_send(cx);
        });
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, cx| v.reply_editor().read(cx).text(cx)), "Testo");
        assert!(view.read_with(cx, |v, _| v.status_text()).contains("forbidden"));
        runner.on("gmail reply m-202", superzed_sources::test_support::ok("{}"));
        view.update(cx, |v, cx| {
            v.request_send(cx);
            v.confirm_send(cx);
        });
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, cx| v.reply_editor().read(cx).text(cx)), "");
        assert!(!view.read_with(cx, |v, _| v.status_text()).contains("forbidden"), "un nuovo invio riuscito toglie l'errore");
    }

    #[gpui::test]
    async fn changing_account_drops_a_pending_confirmation(cx: &mut TestAppContext) {
        let runner = gmail_runner();
        runner.on("gmail thread modify t-200", superzed_sources::test_support::ok("{}"));
        runner.on("gmail reply m-202", superzed_sources::test_support::ok("{}"));
        let (view, cx) = panel(cx, "j.doe@example.com", "from:@acme.example", runner.clone());
        view.update(cx, |v, cx| v.select("t-200".into(), cx));
        cx.run_until_parked();
        view.update_in(cx, |v, window, cx| {
            v.reply_editor().update(cx, |e, cx| e.set_text("Testo", window, cx));
            v.request_send(cx);
            v.account_editor().update(cx, |e, cx| e.set_text("other@example.com", window, cx));
            v.apply_settings(cx);
            v.confirm_send(cx);
        });
        cx.run_until_parked();
        assert_eq!(runner.count("gmail reply"), 0);
        assert_eq!(view.read_with(cx, |v, _| v.send_summary()), None);
        assert!(view.read_with(cx, |v, _| v.shown_thread().is_none()));
        assert_eq!(view.read_with(cx, |v, cx| v.reply_editor().read(cx).text(cx)), "");
    }

    #[gpui::test]
    async fn reply_all_summary_lists_only_the_known_recipients(cx: &mut TestAppContext) {
        let runner = gmail_runner();
        runner.on("gmail thread modify t-200", superzed_sources::test_support::ok("{}"));
        let (view, cx) = panel(cx, "j.doe@example.com", "from:@acme.example", runner);
        view.update(cx, |v, cx| v.select("t-200".into(), cx));
        cx.run_until_parked();
        view.update_in(cx, |v, window, cx| {
            v.reply_editor().update(cx, |e, cx| e.set_text("Testo", window, cx));
            v.set_reply_all(true, cx);
            v.request_send(cx);
        });
        assert_eq!(
            view.read_with(cx, |v, _| v.send_summary()),
            Some("Reply all to team@example.com, j.doe@example.com, bob@example.com".into())
        );
        view.update(cx, |v, _| {
            if let Some(last) = v.thread.as_mut().and_then(|thread| thread.messages.last_mut()) {
                last.to.clear();
                last.cc.clear();
            }
        });
        assert_eq!(view.read_with(cx, |v, _| v.send_summary()), Some("Reply all to team@example.com".into()));
    }

    #[gpui::test]
    async fn a_claude_draft_is_loaded_and_sent_as_is_or_after_edits(cx: &mut TestAppContext) {
        let runner = gmail_runner();
        runner.on("gmail thread modify t-200", superzed_sources::test_support::ok("{}"));
        runner.on("gmail get m-900", superzed_sources::test_support::ok(r#"{"body":"Bozza di Claude\n"}"#));
        runner.on("gmail drafts send r-1", superzed_sources::test_support::ok("{}"));
        runner.on("gmail drafts update r-1", superzed_sources::test_support::ok("{}"));
        let (view, cx) = panel(cx, "j.doe@example.com", "from:@acme.example", runner.clone());
        view.update(cx, |v, cx| v.select("t-200".into(), cx));
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, cx| v.reply_editor().read(cx).text(cx)), "Bozza di Claude\n");
        assert_eq!(view.read_with(cx, |v, _| v.loaded_draft().map(|d| d.id.clone())), Some("r-1".into()));
        view.update(cx, |v, cx| {
            v.request_send(cx);
            v.confirm_send(cx);
        });
        cx.run_until_parked();
        assert_eq!(runner.count("gmail drafts send r-1"), 1);
        assert_eq!(runner.count("gmail drafts update"), 0, "bozza non toccata: si invia com'e'");
        assert_eq!(runner.count("gmail reply"), 0);
    }

    #[gpui::test]
    async fn an_edited_draft_is_updated_before_sending(cx: &mut TestAppContext) {
        let runner = gmail_runner();
        runner.on("gmail thread modify t-200", superzed_sources::test_support::ok("{}"));
        runner.on("gmail get m-900", superzed_sources::test_support::ok(r#"{"body":"Bozza di Claude\n"}"#));
        runner.on("gmail drafts send r-1", superzed_sources::test_support::ok("{}"));
        runner.on("gmail drafts update r-1", superzed_sources::test_support::ok("{}"));
        let (view, cx) = panel(cx, "j.doe@example.com", "from:@acme.example", runner.clone());
        view.update(cx, |v, cx| v.select("t-200".into(), cx));
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, _| v.loaded_draft().map(|d| d.id.clone())), Some("r-1".into()));
        view.update_in(cx, |v, window, cx| {
            v.reply_editor().update(cx, |e, cx| e.set_text("Bozza rivista", window, cx));
            v.request_send(cx);
        });
        assert_eq!(view.read_with(cx, |v, _| v.send_summary()), Some("Send Claude's draft (to the thread's recipients)".into()));
        view.update(cx, |v, cx| v.confirm_send(cx));
        cx.run_until_parked();
        let lines: Vec<String> = runner.calls().iter().map(|call| call.line()).collect();
        let update = lines.iter().position(|line| line.contains("gmail drafts update r-1 --thread-id t-200 --body-file -"));
        let send = lines.iter().position(|line| line.contains("gmail drafts send r-1"));
        assert!(update.is_some() && update < send, "{lines:?}");
        assert_eq!(runner.count("gmail drafts update r-1"), 1);
        assert_eq!(runner.count("gmail drafts send r-1"), 1);
        let update_call = runner.calls().into_iter().find(|call| call.line().contains("gmail drafts update r-1")).unwrap();
        assert_eq!(update_call.stdin.as_deref(), Some("Bozza rivista"));
        assert_eq!(runner.count("gmail reply"), 0);
        assert_eq!(view.read_with(cx, |v, cx| v.reply_editor().read(cx).text(cx)), "");
        assert!(view.read_with(cx, |v, _| v.loaded_draft().is_none()));
    }

    #[gpui::test]
    async fn prepare_draft_asks_claude_for_a_draft_reply_to_the_last_message(cx: &mut TestAppContext) {
        let runner = gmail_runner();
        runner.on("gmail thread modify t-200", superzed_sources::test_support::ok("{}"));
        runner.on("gmail drafts list", superzed_sources::test_support::ok(r#"{"drafts":[]}"#));
        let (view, cx) = panel(cx, "j.doe@example.com", "from:@acme.example", runner);
        let got = events(&view, cx);
        view.update(cx, |v, cx| v.select("t-200".into(), cx));
        cx.run_until_parked();
        view.update(cx, |v, cx| v.prepare_draft(cx));
        let MailPanelEvent::PrepareDraft { prompt } = got.borrow().last().cloned().unwrap() else {
            panic!("atteso PrepareDraft")
        };
        assert!(prompt.contains("gog --account j.doe@example.com gmail drafts reply m-202 --no-quote --body-file -"));
        assert!(prompt.contains("Non inviare"));
        assert!(prompt.contains(superzed_sources::DATA_START));
    }

    #[test]
    fn mail_data_reads_missing_keys_as_empty() {
        assert_eq!(mail_data(&serde_json::Value::Null), (String::new(), String::new()));
        assert_eq!(mail_data(&serde_json::json!({"account": "a", "query": "q"})), ("a".into(), "q".into()));
    }
}
