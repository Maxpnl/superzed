//! Suggerimento delle scorciatoie tenendo premuto cmd, ctrl o alt.
//!
//! Con il focus sul canvas (o in un suo nodo), uno di questi modificatori tenuto senza
//! altri tasti per `HINT_DELAY_MS` mostra in basso a destra un pannello con le
//! scorciatoie superzed disponibili nel contesto di focus, lette dal keymap attivo, che
//! contengono i modificatori tenuti. Sparisce al rilascio di tutti e tre o al primo tasto
//! non modificatore (cmd-c, ctrl-c e le scorciatoie non lo fanno mai comparire). Shift da
//! solo non lo apre (si tiene per le maiuscole e per la selezione multipla), ma
//! aggiunto agli altri restringe l'elenco.
use super::*;
use gpui::{Action, Animation, AnimationExt as _, BoxShadow, Keystroke, Modifiers};
use ui::ElevationIndex;

/// Quanto tenere premuto il modificatore da solo prima che compaia il pannello.
pub const HINT_DELAY_MS: u64 = 400;
/// Dissolvenza in entrata e in uscita (solo senza "riduci movimento").
pub const HINT_FADE_MS: u64 = 120;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HintGroup {
    Navigate,
    Areas,
    Canvas,
    Node,
    Tabs,
    Views,
}

impl HintGroup {
    pub const ORDER: [HintGroup; 6] = [
        HintGroup::Navigate,
        HintGroup::Areas,
        HintGroup::Canvas,
        HintGroup::Node,
        HintGroup::Tabs,
        HintGroup::Views,
    ];

    pub fn title(self) -> &'static str {
        match self {
            HintGroup::Navigate => "Navigate",
            HintGroup::Areas => "Areas",
            HintGroup::Canvas => "Canvas",
            HintGroup::Node => "Node",
            HintGroup::Tabs => "Project tab",
            HintGroup::Views => "Views",
        }
    }
}

/// Una voce dell'elenco con la combinazione trovata nel keymap (`None`: nessun binding
/// nel contesto di focus, la voce non si mostra). `arrow`: una delle quattro frecce di
/// navigazione tra le chat, che si raggruppano in una riga sola.
#[derive(Clone, Debug, PartialEq)]
pub struct HintEntry {
    pub group: HintGroup,
    pub label: &'static str,
    pub arrow: bool,
    pub keystroke: Option<Keystroke>,
}

/// Una riga del pannello: modificatori e uno o piu' tasti (le frecce raggruppate).
#[derive(Clone, Debug, PartialEq)]
pub struct HintRow {
    pub label: &'static str,
    pub modifiers: Modifiers,
    pub keys: Vec<String>,
}

pub const ARROWS_LABEL: &str = "Move between chats";

/// Solo i modificatori che contano per il filtro (fn e caps lock no).
fn relevant(m: Modifiers) -> Modifiers {
    Modifiers { control: m.control, alt: m.alt, shift: m.shift, platform: m.platform, function: false }
}

/// Modificatori che, tenuti, aprono il pannello.
pub fn opens_hints(m: Modifiers) -> bool {
    m.platform || m.control || m.alt
}

/// Righe da mostrare, per gruppo nell'ordine di `HintGroup::ORDER` (gruppi vuoti omessi):
/// solo le voci con un binding i cui modificatori contengono quelli tenuti; le frecce di
/// navigazione rimaste, se hanno tutte gli stessi modificatori, diventano una riga sola
/// (`ARROWS_LABEL`) al posto della prima.
pub fn hint_rows(entries: &[HintEntry], held: Modifiers) -> Vec<(HintGroup, Vec<HintRow>)> {
    let held = relevant(held);
    let visible: Vec<(&HintEntry, &Keystroke)> = entries
        .iter()
        .filter_map(|e| e.keystroke.as_ref().map(|k| (e, k)))
        .filter(|(_, k)| held.is_subset_of(&relevant(k.modifiers)))
        .collect();
    let arrows: Vec<&Keystroke> = visible.iter().filter(|(e, _)| e.arrow).map(|(_, k)| *k).collect();
    let merge_arrows = arrows.len() > 1 && arrows.iter().all(|k| relevant(k.modifiers) == relevant(arrows[0].modifiers));
    HintGroup::ORDER
        .into_iter()
        .filter_map(|group| {
            let mut rows = Vec::new();
            let mut arrows_done = false;
            for (entry, keystroke) in visible.iter().filter(|(e, _)| e.group == group) {
                if entry.arrow && merge_arrows {
                    if !arrows_done {
                        arrows_done = true;
                        rows.push(HintRow {
                            label: ARROWS_LABEL,
                            modifiers: relevant(arrows[0].modifiers),
                            keys: arrows.iter().map(|k| k.key.clone()).collect(),
                        });
                    }
                    continue;
                }
                rows.push(HintRow {
                    label: entry.label,
                    modifiers: relevant(keystroke.modifiers),
                    keys: vec![keystroke.key.clone()],
                });
            }
            (!rows.is_empty()).then_some((group, rows))
        })
        .collect()
}

/// Oltre queste righe (intestazioni comprese) i gruppi vanno su due colonne.
pub const ONE_COLUMN_MAX_LINES: usize = 16;

/// Gruppi divisi in colonne, nell'ordine: una sola se l'elenco e' corto, altrimenti due,
/// tagliate tra un gruppo e l'altro dove le altezze (righe + intestazione) si equilibrano.
pub fn hint_columns(groups: Vec<(HintGroup, Vec<HintRow>)>) -> Vec<Vec<(HintGroup, Vec<HintRow>)>> {
    let lines: Vec<usize> = groups.iter().map(|(_, rows)| rows.len() + 1).collect();
    let total: usize = lines.iter().sum();
    if total <= ONE_COLUMN_MAX_LINES || groups.len() < 2 {
        return vec![groups];
    }
    // taglio dopo `cut` gruppi (1..len): la colonna piu' alta la piu' bassa possibile
    let cut = (1..groups.len())
        .min_by_key(|&cut| {
            let left: usize = lines[..cut].iter().sum();
            left.max(total - left)
        })
        .unwrap_or(1);
    let mut left = groups;
    let right = left.split_off(cut);
    vec![left, right]
}

/// Simbolo macOS di un tasto: frecce, invio, esc... come simboli, lettere maiuscole.
pub fn key_symbol(key: &str) -> String {
    match key {
        "up" => "↑".into(),
        "down" => "↓".into(),
        "left" => "←".into(),
        "right" => "→".into(),
        "enter" => "⏎".into(),
        "escape" => "⎋".into(),
        "backspace" => "⌫".into(),
        "delete" => "⌦".into(),
        "tab" => "⇥".into(),
        "space" => "Space".into(),
        "pageup" => "PgUp".into(),
        "pagedown" => "PgDn".into(),
        "home" => "Home".into(),
        "end" => "End".into(),
        key => key.to_uppercase(),
    }
}

/// Tasti modificatori di una combinazione nell'ordine macOS (⌃ ⌥ ⇧ ⌘), ognuno con
/// "tenuto premuto adesso".
pub fn modifier_keys(m: Modifiers, held: Modifiers) -> Vec<(&'static str, bool)> {
    [
        (m.control, "⌃", held.control),
        (m.alt, "⌥", held.alt),
        (m.shift, "⇧", held.shift),
        (m.platform, "⌘", held.platform),
    ]
    .into_iter()
    .filter(|(on, _, _)| *on)
    .map(|(_, symbol, pressed)| (symbol, pressed))
    .collect()
}

/// L'elenco fisso delle azioni suggerite.
fn catalog() -> Vec<(HintGroup, &'static str, bool, Box<dyn Action>)> {
    use superzed_actions as a;
    use HintGroup::*;
    let mut list: Vec<(HintGroup, &'static str, bool, Box<dyn Action>)> = vec![
        (Navigate, "Chat above", true, Box::new(a::FocusChatUp)),
        (Navigate, "Chat below", true, Box::new(a::FocusChatDown)),
        (Navigate, "Chat on the left", true, Box::new(a::FocusChatLeft)),
        (Navigate, "Chat on the right", true, Box::new(a::FocusChatRight)),
        (Navigate, "Focus canvas", false, Box::new(a::FocusCanvas)),
        (Navigate, "Focus editor", false, Box::new(a::FocusEditor)),
        (Areas, "Next area", false, Box::new(a::FocusNextArea)),
        (Areas, "Previous area", false, Box::new(a::FocusPreviousArea)),
        (Canvas, "New Claude chat", false, Box::new(a::NewClaudeNode)),
        (Canvas, "New chat, choose account", false, Box::new(a::NewClaudeNodePick)),
        (Canvas, "New chat from a prompt", false, Box::new(a::NewClaudeNodeFromPrompt)),
        (Canvas, "New terminal", false, Box::new(a::NewTerminal)),
        (Canvas, "New note", false, Box::new(a::NewNote)),
        (Canvas, "Select all nodes", false, Box::new(a::SelectAllNodes)),
        (Canvas, "Zoom in", false, Box::new(a::CanvasZoomIn)),
        (Canvas, "Zoom out", false, Box::new(a::CanvasZoomOut)),
        (Canvas, "Reset zoom", false, Box::new(a::CanvasZoomReset)),
        (Canvas, "Zoom to focused node", false, Box::new(a::ZoomToFocusedNode)),
        (Canvas, "Open kanban", false, Box::new(a::OpenKanban)),
        (Node, "Rename", false, Box::new(a::RenameNode)),
        (Node, "Next color", false, Box::new(a::CycleNodeColor)),
        (Node, "Golden", false, Box::new(a::ToggleGolden)),
        (Node, "Close", false, Box::new(a::CloseNode)),
        // solo nel kanban (o in una sua chat)
        (Node, "Move to next phase", false, Box::new(a::MoveCardNext)),
        (Node, "Move to previous phase", false, Box::new(a::MoveCardPrev)),
        (Tabs, "Next project tab", false, Box::new(a::NextProjectTab)),
        (Tabs, "Previous project tab", false, Box::new(a::PrevProjectTab)),
        // ctrl-1...ctrl-9 come una riga sola: la prima basta a mostrare la combinazione
        (Tabs, "Project tab 1…9", false, Box::new(a::ActivateProjectTab(1))),
        (Tabs, "Next project tab color", false, Box::new(a::CycleProjectTabColor)),
        (Tabs, "Close project tab", false, Box::new(a::CloseProjectTab)),
    ];
    let views = ["Files", "Git", "Sessions", "Search", "Notifications", "Sources"];
    for (ix, name) in views.into_iter().enumerate() {
        list.push((Views, name, false, Box::new(a::SidebarView(ix + 1))));
    }
    list
}

/// Voci del catalogo con la combinazione del keymap attivo nel contesto di `focused`
/// (solo binding di un tasto).
pub fn resolve_entries(window: &Window, focused: &FocusHandle) -> Vec<HintEntry> {
    catalog()
        .into_iter()
        .map(|(group, label, arrow, action)| HintEntry {
            group,
            label,
            arrow,
            keystroke: window
                .highest_precedence_binding_for_action_in(action.as_ref(), focused)
                .filter(|b| b.keystrokes().len() == 1)
                .map(|b| b.keystrokes()[0].inner().clone()),
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum Phase {
    /// Nessun modificatore che apre il pannello premuto.
    #[default]
    Idle,
    /// Modificatore premuto senza altri tasti, si aspetta `HINT_DELAY_MS`.
    Pending,
    Shown,
    /// Dissolvenza in uscita; `dismissed`: chiuso da un tasto con il modificatore ancora
    /// premuto.
    Fading { dismissed: bool },
    /// Chiuso (o mai aperto) da un tasto: si riapre solo dopo aver rilasciato i
    /// modificatori.
    Dismissed,
}

/// Stato del pannello, dentro `CanvasView`.
#[derive(Default)]
pub struct ShortcutHints {
    phase: Phase,
    /// Modificatori tenuti adesso.
    held: Modifiers,
    /// Timer del ritardo o della dissolvenza in uscita; sostituirlo lo annulla.
    task: Option<Task<()>>,
    /// Voci risolte nel keymap quando il pannello compare, con il focus di allora (il
    /// disegno non rifà le ricerche a ogni frame).
    entries: Option<(FocusHandle, Vec<HintEntry>)>,
}

impl ShortcutHints {
    pub fn is_visible(&self) -> bool {
        matches!(self.phase, Phase::Shown | Phase::Fading { .. })
    }
}

impl CanvasView {
    /// Modificatori cambiati con il focus nel canvas: cmd, ctrl o alt premuto avvia il
    /// ritardo, rilasciati tutti chiude; gli altri cambi aggiornano il filtro dal vivo.
    pub(super) fn hints_modifiers_changed(&mut self, modifiers: Modifiers, window: &mut Window, cx: &mut Context<Self>) {
        let hints = &mut self.shortcut_hints;
        if !opens_hints(modifiers) {
            // `held` resta quello di prima: la dissolvenza in uscita mostra lo stesso elenco
            if hints.phase == Phase::Shown {
                self.hints_fade_out(false, window, cx);
            } else if let Phase::Fading { .. } = hints.phase {
                hints.phase = Phase::Fading { dismissed: false };
            } else {
                hints.phase = Phase::Idle;
                hints.task = None;
                hints.entries = None;
            }
            cx.notify();
            return;
        }
        hints.held = relevant(modifiers);
        match hints.phase {
            Phase::Idle | Phase::Fading { dismissed: false } => {
                hints.phase = Phase::Pending;
                hints.task = Some(cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor().timer(Duration::from_millis(HINT_DELAY_MS)).await;
                    this.update_in(cx, |c, window, cx| {
                        if c.shortcut_hints.phase != Phase::Pending {
                            return;
                        }
                        c.shortcut_hints.task = None;
                        // il rilascio puo' essere andato altrove (un'altra app, un'altra
                        // finestra): contano i modificatori reali di adesso
                        if !window.is_window_active() || !opens_hints(window.modifiers()) {
                            c.shortcut_hints.phase = Phase::Idle;
                            return;
                        }
                        c.shortcut_hints.phase = Phase::Shown;
                        let focused = window.focused(cx).unwrap_or_else(|| c.focus_handle.clone());
                        c.shortcut_hints.entries = Some((focused.clone(), resolve_entries(window, &focused)));
                        cx.notify();
                    })
                    .ok();
                }));
            }
            Phase::Shown => cx.notify(),
            Phase::Pending | Phase::Fading { dismissed: true } | Phase::Dismissed => {}
        }
    }

    /// Il focus esce dal canvas o la finestra cambia attivazione: il rilascio non
    /// arrivera' qui, lo stato ricomincia da capo (niente pannello rimasto aperto o
    /// chiuso per sempre).
    pub(super) fn hints_reset(&mut self, cx: &mut Context<Self>) {
        let hints = &mut self.shortcut_hints;
        if hints.phase != Phase::Idle {
            hints.phase = Phase::Idle;
            hints.task = None;
            hints.entries = None;
            cx.notify();
        }
    }

    /// Un tasto non modificatore (anche una scorciatoia): il pannello si chiude e non
    /// compare piu' finche' il modificatore resta premuto.
    pub(super) fn hints_key_pressed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.shortcut_hints.phase {
            Phase::Pending => {
                self.shortcut_hints.phase = Phase::Dismissed;
                self.shortcut_hints.task = None;
            }
            Phase::Shown => self.hints_fade_out(true, window, cx),
            Phase::Fading { .. } => self.shortcut_hints.phase = Phase::Fading { dismissed: true },
            Phase::Idle | Phase::Dismissed => return,
        }
        cx.notify();
    }

    fn hints_fade_out(&mut self, dismissed: bool, window: &mut Window, cx: &mut Context<Self>) {
        let hints = &mut self.shortcut_hints;
        if cx.reduce_motion() {
            hints.phase = if dismissed { Phase::Dismissed } else { Phase::Idle };
            hints.task = None;
            return;
        }
        hints.phase = Phase::Fading { dismissed };
        hints.task = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(HINT_FADE_MS)).await;
            this.update(cx, |c, cx| {
                if let Phase::Fading { dismissed } = c.shortcut_hints.phase {
                    c.shortcut_hints.phase = if dismissed { Phase::Dismissed } else { Phase::Idle };
                    c.shortcut_hints.task = None;
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// Righe del pannello se e' visibile (e il focus e' ancora nel canvas, la finestra e'
    /// attiva e, fuori dalla dissolvenza in uscita, il modificatore e' davvero premuto).
    pub(super) fn shortcut_hint_rows(&self, window: &Window, cx: &App) -> Option<Vec<(HintGroup, Vec<HintRow>)>> {
        let hints = &self.shortcut_hints;
        if !hints.is_visible()
            || !window.is_window_active()
            || (hints.phase == Phase::Shown && !opens_hints(window.modifiers()))
            || !self.focus_handle.contains_focused(window, cx)
        {
            return None;
        }
        // il contesto del nodo con il focus (il contesto corrente del frame non lo e' fuori
        // dal disegno)
        let focused = window.focused(cx).unwrap_or_else(|| self.focus_handle.clone());
        let rows = match &hints.entries {
            Some((handle, entries)) if *handle == focused => hint_rows(entries, hints.held),
            _ => hint_rows(&resolve_entries(window, &focused), hints.held),
        };
        Some(rows)
    }

    /// Il pannello in basso a destra: nessun listener, quindi nessun hitbox (mouse e
    /// scroll arrivano al canvas e ai nodi sotto).
    pub(super) fn render_shortcut_hints(&self, window: &Window, cx: &App) -> Option<AnyElement> {
        let groups = self.shortcut_hint_rows(window, cx)?;
        if groups.is_empty() {
            return None;
        }
        let colors = cx.theme().colors();
        let held = self.shortcut_hints.held;
        let panel = h_flex()
            .debug_selector(|| "superzed-shortcut-hints".into())
            .items_start()
            .absolute()
            .bottom(rems(1.))
            .right(rems(1.))
            .gap(rems(2.))
            .p(rems(1.))
            .rounded_lg()
            .border_1()
            .border_color(colors.border_variant)
            .bg(colors.elevated_surface_background.opacity(0.85))
            .shadow(ElevationIndex::ElevatedSurface.shadow(cx))
            .children(hint_columns(groups).into_iter().map(|column| {
                v_flex().gap(rems(0.75)).children(column.into_iter().map(|(group, rows)| {
                v_flex()
                    .gap(rems(0.375))
                    .child(Label::new(group.title()).size(LabelSize::Small).color(Color::Muted))
                    .children(rows.into_iter().map(|row| {
                        h_flex()
                            .gap(rems(0.75))
                            .child(
                                h_flex()
                                    .min_w(rems(7.5))
                                    .gap(rems(0.25))
                                    .children(
                                        modifier_keys(row.modifiers, held)
                                            .into_iter()
                                            .map(|(symbol, pressed)| keycap(symbol.to_string(), pressed, cx)),
                                    )
                                    .children(row.keys.iter().map(|k| keycap(key_symbol(k), false, cx))),
                            )
                            .child(Label::new(row.label).size(LabelSize::Default))
                    }))
                }))
            }));
        let element = match (self.shortcut_hints.phase, cx.reduce_motion()) {
            (_, true) => panel.into_any_element(),
            (Phase::Fading { .. }, false) => panel
                .with_animation(
                    "superzed-shortcut-hints-out",
                    Animation::new(Duration::from_millis(HINT_FADE_MS)),
                    |el, delta| el.opacity(1. - delta),
                )
                .into_any_element(),
            (_, false) => panel
                .with_animation(
                    "superzed-shortcut-hints-in",
                    Animation::new(Duration::from_millis(HINT_FADE_MS)),
                    |el, delta| el.opacity(delta),
                )
                .into_any_element(),
        };
        Some(element)
    }
}

/// Un tasto disegnato come un keycap vero: sotto al cappuccio un bordo inferiore piu'
/// spesso e piu' scuro, e un'ombra; premuto, il bordo inferiore si assottiglia (il tasto
/// "scende") e lo sfondo e' quello attivo.
fn keycap(text: String, pressed: bool, cx: &App) -> Div {
    let colors = cx.theme().colors();
    let shadow: Vec<BoxShadow> = ElevationIndex::ElevatedSurface.shadow(cx);
    // il nero dell'ombra del tema sul bordo: lo stesso bordo, piu' scuro
    let lip = shadow.first().map_or(colors.border, |s| colors.border.blend(s.color.alpha(0.35)));
    let cap = h_flex()
        .justify_center()
        .min_w(rems(1.75))
        .h(rems(1.75))
        .px(rems(0.375))
        .rounded_md()
        .border_1()
        .border_color(colors.border)
        .bg(if pressed { colors.element_active } else { colors.element_background })
        .child(Label::new(text).size(LabelSize::Default).color(if pressed { Color::Accent } else { Color::Default }));
    div()
        .rounded_md()
        .bg(lip)
        .when(pressed, |d| d.mt(px(1.5)).pb(px(1.)))
        .when(!pressed, |d| d.pb(px(2.5)).shadow(shadow))
        .child(cap)
}

#[cfg(test)]
mod tests {
    use super::super::navigation::tests::bind_real_keymap;
    use super::super::tests::{close_all, kanban_with_claude_chats, redraw};
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};

    fn ks(s: &str) -> Option<Keystroke> {
        Some(Keystroke::parse(s).unwrap())
    }

    fn entry(group: HintGroup, label: &'static str, arrow: bool, keystroke: Option<Keystroke>) -> HintEntry {
        HintEntry { group, label, arrow, keystroke }
    }

    fn sample() -> Vec<HintEntry> {
        use HintGroup::*;
        vec![
            entry(Navigate, "Chat above", true, ks("cmd-up")),
            entry(Navigate, "Chat below", true, ks("cmd-down")),
            entry(Navigate, "Chat on the left", true, ks("cmd-left")),
            entry(Navigate, "Chat on the right", true, ks("cmd-right")),
            entry(Areas, "Next area", false, ks("cmd-alt-right")),
            entry(Areas, "Previous area", false, ks("cmd-alt-left")),
            entry(Canvas, "Zoom in", false, ks("cmd-=")),
            entry(Canvas, "Open kanban", false, ks("cmd-alt-k")),
            entry(Canvas, "New terminal", false, None),
            entry(Node, "Rename", false, ks("f2")),
            entry(Node, "Close", false, ks("cmd-w")),
        ]
    }

    fn labels(rows: &[(HintGroup, Vec<HintRow>)]) -> Vec<&'static str> {
        rows.iter().flat_map(|(_, r)| r.iter().map(|r| r.label)).collect()
    }

    #[test]
    fn cmd_alone_shows_every_cmd_binding_with_the_arrows_merged() {
        let rows = hint_rows(&sample(), Modifiers::command());
        let groups: Vec<_> = rows.iter().map(|(g, _)| *g).collect();
        assert_eq!(groups, vec![HintGroup::Navigate, HintGroup::Areas, HintGroup::Canvas, HintGroup::Node]);
        assert_eq!(
            labels(&rows),
            vec![ARROWS_LABEL, "Next area", "Previous area", "Zoom in", "Open kanban", "Close"],
            "senza binding (New terminal) e senza cmd (Rename, f2) omesse"
        );
        let arrows = &rows[0].1[0];
        assert_eq!(arrows.modifiers, Modifiers::command());
        assert_eq!(arrows.keys, vec!["up", "down", "left", "right"]);
        assert_eq!(rows[1].1[0].keys, vec!["right"]);
        assert_eq!(rows[1].1[0].modifiers, Modifiers { platform: true, alt: true, ..Default::default() });
    }

    #[test]
    fn cmd_alt_narrows_to_the_bindings_with_both() {
        let held = Modifiers { platform: true, alt: true, ..Default::default() };
        let rows = hint_rows(&sample(), held);
        assert_eq!(labels(&rows), vec!["Next area", "Previous area", "Open kanban"]);
        assert!(hint_rows(&sample(), Modifiers::command_shift()).is_empty());
        // fn e caps lock non restringono
        let with_fn = Modifiers { platform: true, function: true, ..Default::default() };
        assert_eq!(labels(&hint_rows(&sample(), with_fn)).len(), 6);
    }

    #[test]
    fn ctrl_or_alt_alone_open_it_shift_alone_does_not() {
        assert!(opens_hints(Modifiers::command()));
        assert!(opens_hints(Modifiers::control()));
        assert!(opens_hints(Modifiers::alt()));
        assert!(!opens_hints(Modifiers::shift()));
        assert!(!opens_hints(Modifiers { function: true, ..Default::default() }));
        use HintGroup::*;
        let entries = vec![
            entry(Tabs, "Next project tab", false, ks("ctrl-tab")),
            entry(Tabs, "Previous project tab", false, ks("ctrl-shift-tab")),
            entry(Node, "Next color", false, ks("alt-c")),
            entry(Tabs, "Next project tab color", false, ks("alt-shift-c")),
            entry(Areas, "Next area", false, ks("cmd-alt-right")),
            entry(Node, "Close", false, ks("cmd-w")),
        ];
        assert_eq!(labels(&hint_rows(&entries, Modifiers::control())), vec!["Next project tab", "Previous project tab"]);
        assert_eq!(
            labels(&hint_rows(&entries, Modifiers::alt())),
            vec!["Next area", "Next color", "Next project tab color"]
        );
    }

    #[test]
    fn arrows_with_different_modifiers_stay_separate() {
        use HintGroup::*;
        let entries = vec![
            entry(Navigate, "Chat above", true, ks("cmd-up")),
            entry(Navigate, "Chat below", true, ks("cmd-down")),
            entry(Navigate, "Chat on the left", true, ks("cmd-shift-left")),
            entry(Navigate, "Chat on the right", true, None),
        ];
        assert_eq!(
            labels(&hint_rows(&entries, Modifiers::command())),
            vec!["Chat above", "Chat below", "Chat on the left"]
        );
        // restano solo su e giu', con gli stessi modificatori: una riga
        let two = &entries[..2];
        let rows = hint_rows(two, Modifiers::command());
        assert_eq!(labels(&rows), vec![ARROWS_LABEL]);
        assert_eq!(rows[0].1[0].keys, vec!["up", "down"]);
    }

    #[test]
    fn a_long_list_goes_on_two_balanced_columns_split_between_groups() {
        use HintGroup::*;
        let row = |label| HintRow { label, modifiers: Modifiers::command(), keys: vec!["n".into()] };
        let group = |g, n: usize| (g, (0..n).map(|_| row("x")).collect::<Vec<_>>());
        let short = vec![group(Navigate, 3), group(Canvas, 4)];
        assert_eq!(hint_columns(short).len(), 1, "corto: una colonna");
        // righe+intestazione: 2, 3, 9, 3, 2, 7 = 26 -> 14 | 12
        let long = vec![group(Navigate, 1), group(Areas, 2), group(Canvas, 8), group(Node, 2), group(Tabs, 1), group(Views, 6)];
        let columns = hint_columns(long);
        let titles: Vec<Vec<HintGroup>> = columns.iter().map(|c| c.iter().map(|(g, _)| *g).collect()).collect();
        assert_eq!(titles, vec![vec![Navigate, Areas, Canvas], vec![Node, Tabs, Views]]);
    }

    #[test]
    fn keys_as_macos_symbols() {
        assert_eq!(key_symbol("up"), "↑");
        assert_eq!(key_symbol("right"), "→");
        assert_eq!(key_symbol("enter"), "⏎");
        assert_eq!(key_symbol("escape"), "⎋");
        assert_eq!(key_symbol("k"), "K");
        assert_eq!(key_symbol("f2"), "F2");
        assert_eq!(key_symbol("0"), "0");
        let cmd_alt_shift = Modifiers { platform: true, alt: true, shift: true, ..Default::default() };
        assert_eq!(
            modifier_keys(cmd_alt_shift, Modifiers::command()),
            vec![("⌥", false), ("⇧", false), ("⌘", true)],
            "ordine macOS, premuto solo cmd"
        );
    }

    // ----- GPUI: keymap vero, focus nel terminale di una chat del kanban -----

    fn cmd_alt() -> Modifiers {
        Modifiers { platform: true, alt: true, ..Default::default() }
    }

    fn panel_shown(cx: &mut VisualTestContext) -> bool {
        redraw(cx);
        cx.debug_bounds("superzed-shortcut-hints").is_some()
    }

    fn wait(cx: &mut VisualTestContext, ms: u64) {
        cx.executor().advance_clock(Duration::from_millis(ms));
        cx.run_until_parked();
    }

    fn shown_labels(canvas: &Entity<CanvasView>, cx: &mut VisualTestContext) -> Vec<&'static str> {
        canvas.update_in(cx, |c, window, cx| labels(&c.shortcut_hint_rows(window, cx).unwrap_or_default()))
    }

    async fn chat_with_focus(cx: &mut TestAppContext) -> (Entity<CanvasView>, &'static mut VisualTestContext) {
        let (canvas, _kanban, chats, cx) = kanban_with_claude_chats(2, cx).await;
        canvas.update(cx, |c, cx| {
            c.dock(chats[0], superzed_sessions::Phase::New, cx);
            c.dock(chats[1], superzed_sessions::Phase::New, cx);
        });
        cx.run_until_parked();
        bind_real_keymap(cx);
        activate(cx);
        canvas.update_in(cx, |c, window, cx| c.focus_node(chats[0], window, cx));
        redraw(cx);
        (canvas, cx)
    }

    fn activate(cx: &mut VisualTestContext) {
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        assert!(cx.update(|window, _| window.is_window_active()));
    }

    #[gpui::test]
    async fn focus_leaving_the_canvas_with_cmd_held_resets_the_panel(cx: &mut TestAppContext) {
        let (canvas, cx) = chat_with_focus(cx).await;
        cx.simulate_modifiers_change(Modifiers::command());
        wait(cx, HINT_DELAY_MS + 50);
        assert!(panel_shown(cx));
        // con cmd premuto il focus esce dal canvas (un prompt del workspace); il rilascio
        // arriva al prompt, poi esc lo chiude e il focus torna nella chat
        let workspace = canvas.read_with(cx, |c, _| c.workspace.upgrade().unwrap());
        workspace.update_in(cx, |ws, window, cx| crate::text_prompt::text_prompt(ws, "Name", "", |_, _, _| {}, window, cx));
        redraw(cx);
        let inside = |cx: &mut VisualTestContext| canvas.update_in(cx, |c, window, cx| c.focus_handle.contains_focused(window, cx));
        assert!(!inside(cx), "focus nel prompt");
        cx.simulate_modifiers_change(Modifiers::default());
        cx.simulate_keystrokes("escape");
        redraw(cx);
        assert!(inside(cx), "focus di nuovo nel canvas");
        wait(cx, HINT_DELAY_MS + HINT_FADE_MS);
        assert!(!panel_shown(cx), "tornando nel canvas senza tasti premuti niente pannello");
        // e cmd tenuto di nuovo lo mostra
        cx.simulate_modifiers_change(Modifiers::command());
        wait(cx, HINT_DELAY_MS + 50);
        assert!(panel_shown(cx));
        cx.simulate_modifiers_change(Modifiers::default());
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn after_cmd_w_and_the_confirm_the_next_cmd_hold_shows_the_panel(cx: &mut TestAppContext) {
        let (canvas, cx) = chat_with_focus(cx).await;
        cx.simulate_modifiers_change(Modifiers::command());
        cx.simulate_keystrokes("cmd-w");
        cx.run_until_parked();
        redraw(cx);
        // cmd rilasciato con il focus nella conferma, poi "y"
        cx.simulate_modifiers_change(Modifiers::default());
        cx.simulate_keystrokes("y");
        cx.run_until_parked();
        redraw(cx);
        assert_eq!(canvas.read_with(cx, |c, _| c.terminals.len()), 1, "chat chiusa");
        assert!(canvas.update_in(cx, |c, window, cx| c.focus_handle.contains_focused(window, cx)), "focus alla vicina");
        cx.simulate_modifiers_change(Modifiers::command());
        wait(cx, HINT_DELAY_MS + 50);
        assert!(panel_shown(cx), "il primo cmd tenuto dopo la chiusura mostra il pannello");
        cx.simulate_modifiers_change(Modifiers::default());
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn window_deactivated_before_the_delay_never_shows_it(cx: &mut TestAppContext) {
        let (canvas, cx) = chat_with_focus(cx).await;
        cx.simulate_modifiers_change(Modifiers::command());
        wait(cx, 100);
        // cmd-tab: la finestra perde l'attivazione, il rilascio va all'altra app
        cx.deactivate_window();
        wait(cx, HINT_DELAY_MS + 50);
        assert!(!panel_shown(cx), "finestra inattiva: niente pannello");
        activate(cx);
        wait(cx, HINT_DELAY_MS + 50);
        assert!(!panel_shown(cx), "tornando nella finestra senza tasti premuti niente pannello");
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn holding_cmd_shows_the_panel_until_released(cx: &mut TestAppContext) {
        let (canvas, cx) = chat_with_focus(cx).await;
        cx.simulate_modifiers_change(Modifiers::command());
        wait(cx, HINT_DELAY_MS - 100);
        assert!(!panel_shown(cx), "prima del ritardo niente");
        wait(cx, 150);
        assert!(panel_shown(cx), "cmd tenuto oltre il ritardo");
        let shown = shown_labels(&canvas, cx);
        assert!(shown.contains(&ARROWS_LABEL));
        // dal keymap vero, con il focus in una chat del kanban
        for label in [
            "New chat, choose account",
            "New chat from a prompt",
            "New note",
            "Focus canvas",
            "Move to next phase",
            "Close project tab",
        ] {
            assert!(shown.contains(&label), "{label}: {shown:?}");
        }
        cx.simulate_modifiers_change(Modifiers::default());
        assert_eq!(
            shown_labels(&canvas, cx),
            shown,
            "durante la dissolvenza in uscita lo stesso elenco (non quello senza modificatori)"
        );
        wait(cx, HINT_FADE_MS + 50);
        assert!(!panel_shown(cx), "cmd rilasciato");
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn a_key_pressed_with_cmd_before_the_delay_never_shows_it(cx: &mut TestAppContext) {
        let (canvas, cx) = chat_with_focus(cx).await;
        cx.simulate_modifiers_change(Modifiers::command());
        wait(cx, 100);
        cx.simulate_keystrokes("cmd-c");
        wait(cx, HINT_DELAY_MS + 200);
        assert!(!panel_shown(cx), "cmd-c: mai");
        // con cmd ancora premuto non ricompare; rilasciato e ripremuto si'
        cx.simulate_modifiers_change(cmd_alt());
        wait(cx, HINT_DELAY_MS + 100);
        assert!(!panel_shown(cx));
        cx.simulate_modifiers_change(Modifiers::default());
        cx.simulate_modifiers_change(Modifiers::command());
        wait(cx, HINT_DELAY_MS + 50);
        assert!(panel_shown(cx));
        // una scorciatoia premuta lo chiude
        cx.simulate_keystrokes("cmd-0");
        wait(cx, HINT_FADE_MS + 50);
        assert!(!panel_shown(cx), "chiuso dalla scorciatoia");
        cx.simulate_modifiers_change(Modifiers::default());
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn holding_ctrl_or_alt_shows_their_bindings_shift_alone_nothing(cx: &mut TestAppContext) {
        let (canvas, cx) = chat_with_focus(cx).await;
        cx.simulate_modifiers_change(Modifiers::shift());
        wait(cx, HINT_DELAY_MS + 50);
        assert!(!panel_shown(cx), "shift da solo: mai");
        cx.simulate_modifiers_change(Modifiers::default());

        cx.simulate_modifiers_change(Modifiers::control());
        wait(cx, HINT_DELAY_MS + 50);
        assert!(panel_shown(cx), "ctrl tenuto oltre il ritardo");
        let shown = shown_labels(&canvas, cx);
        for label in ["Next project tab", "Previous project tab", "Project tab 1…9"] {
            assert!(shown.contains(&label), "{label}: {shown:?}");
        }
        assert!(!shown.contains(&"Close project tab"), "cmd-shift-w non contiene ctrl: {shown:?}");
        cx.simulate_modifiers_change(Modifiers::default());
        wait(cx, HINT_FADE_MS + 50);
        assert!(!panel_shown(cx), "ctrl rilasciato");

        cx.simulate_modifiers_change(Modifiers::alt());
        wait(cx, HINT_DELAY_MS + 50);
        assert!(panel_shown(cx), "alt tenuto oltre il ritardo");
        let shown = shown_labels(&canvas, cx);
        for label in ["Next project tab color", "Next area"] {
            assert!(shown.contains(&label), "{label}: {shown:?}");
        }
        // passando da alt a cmd senza lasciare tutto il pannello resta e cambia elenco
        cx.simulate_modifiers_change(cmd_alt());
        cx.simulate_modifiers_change(Modifiers::command());
        assert!(panel_shown(cx));
        assert!(shown_labels(&canvas, cx).contains(&"New chat from a prompt"));
        cx.simulate_modifiers_change(Modifiers::default());
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn cmd_alt_narrows_the_list_live(cx: &mut TestAppContext) {
        let (canvas, cx) = chat_with_focus(cx).await;
        cx.simulate_modifiers_change(Modifiers::command());
        wait(cx, HINT_DELAY_MS + 50);
        assert!(panel_shown(cx));
        let all = shown_labels(&canvas, cx);
        assert!(all.contains(&ARROWS_LABEL) && all.contains(&"Next area"), "{all:?}");
        cx.simulate_modifiers_change(cmd_alt());
        assert!(panel_shown(cx), "resta visibile con cmd premuto");
        let narrowed = shown_labels(&canvas, cx);
        assert!(narrowed.contains(&"Next area"), "{narrowed:?}");
        assert!(!narrowed.contains(&ARROWS_LABEL), "{narrowed:?}");
        cx.simulate_modifiers_change(Modifiers::default());
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn the_panel_lets_scroll_through_to_the_canvas(cx: &mut TestAppContext) {
        let (canvas, cx) = chat_with_focus(cx).await;
        cx.simulate_modifiers_change(Modifiers::command());
        wait(cx, HINT_DELAY_MS + 50);
        assert!(panel_shown(cx));
        let panel = cx.debug_bounds("superzed-shortcut-hints").unwrap();
        let before = canvas.read_with(cx, |c, _| c.viewport().offset);
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: panel.center(),
            delta: gpui::ScrollDelta::Pixels(point(px(0.), px(30.))),
            modifiers: Modifiers::default(),
            touch_phase: gpui::TouchPhase::Moved,
        });
        let after = canvas.read_with(cx, |c, _| c.viewport().offset);
        assert_ne!(before, after, "lo scroll sopra il pannello muove il canvas");
        cx.simulate_modifiers_change(Modifiers::default());
        close_all(&canvas, cx);
    }
}
