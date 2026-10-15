//! Scroll a due dita sul canvas: pan del canvas o scroll del nodo sotto il cursore.
//!
//! La scelta si fa in fase di cattura sul canvas, prima che il nodo veda l'evento: se
//! il canvas fa pan ferma la propagazione e il terminale non riceve niente. Lo scroll va
//! al nodo solo se il cursore e' sul corpo (sotto la barra del titolo) del nodo in primo
//! piano che ha il focus e che non e' in vista d'insieme; kanban e Automazioni mai.
//!
//! Aggancio del gesto: un pan continua a spostare il canvas anche quando sotto il cursore
//! passa un nodo con il focus (il cursore sta fermo, e' il canvas che si muove). Fine del
//! gesto: gpui su macOS da' `TouchPhase::Started` a dita appoggiate (MayBegin/Began) e
//! `Ended` al sollevamento, ma l'inerzia che segue arriva come `Moved` (NSEventPhaseNone
//! con momentumPhase, che gpui non espone) e la rotella di un mouse e' sempre `Moved`.
//! Quindi il gesto agganciato finisce al prossimo `Started` (nuovo gesto) oppure dopo
//! `PAN_LATCH_MS` senza eventi di scroll: `Ended` da solo non sgancia, cosi' l'inerzia
//! dopo il sollevamento resta al canvas.
use super::*;
use gpui::{DispatchPhase, Hitbox, TouchPhase};
use std::time::Instant;

/// Pausa senza scroll dopo cui il gesto di pan agganciato e' finito (inerzia compresa).
pub const PAN_LATCH_MS: u64 = 200;

/// Gesto di pan in corso: istante dell'ultimo scroll usato dal canvas.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PanLatch {
    last: Option<Instant>,
}

impl PanLatch {
    /// Il pan agganciato vale ancora per un evento di fase `phase` a `now`.
    pub fn active(&self, phase: TouchPhase, now: Instant) -> bool {
        phase != TouchPhase::Started
            && self
                .last
                .is_some_and(|last| now.saturating_duration_since(last) < Duration::from_millis(PAN_LATCH_MS))
    }

    /// Il canvas ha usato uno scroll a `now`: il gesto resta agganciato.
    pub fn hold(&mut self, now: Instant) {
        self.last = Some(now);
    }

    /// Lo scroll e' andato a un nodo: nessun gesto agganciato.
    pub fn release(&mut self) {
        self.last = None;
    }
}

/// Registra (nel paint del canvas) il listener di scroll in fase di cattura. `hitbox`:
/// quella del canvas, cosi' un modale sopra il canvas tiene il suo scroll.
pub(super) fn register_scroll_capture(this: WeakEntity<CanvasView>, hitbox: Hitbox, window: &mut Window) {
    window.on_mouse_event(move |e: &ScrollWheelEvent, phase, window, cx| {
        if phase == DispatchPhase::Capture
            && hitbox.should_handle_scroll(window)
            && this.update(cx, |c, cx| c.route_scroll(e, window, cx)).unwrap_or(false)
        {
            cx.stop_propagation();
        }
    });
}

impl CanvasView {
    /// Scroll senza cmd sopra il canvas: `true` se il canvas l'ha usato per il pan (e il
    /// nodo sotto non deve riceverlo). Cmd+scroll resta allo zoom (`on_scroll`).
    pub(super) fn route_scroll(&mut self, e: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if e.modifiers.platform {
            return false;
        }
        let now = cx.background_executor().now();
        if !self.pan_latch.active(e.touch_phase, now) && self.scroll_owner(e.position, window, cx).is_some() {
            self.pan_latch.release();
            return false;
        }
        self.pan_latch.hold(now);
        self.pan_by_scroll(e, window, cx);
        true
    }

    /// Pan del canvas per uno scroll.
    pub(super) fn pan_by_scroll(&mut self, e: &ScrollWheelEvent, window: &Window, cx: &mut Context<Self>) {
        // uno scroll dell'utente cancella un pan di navigazione ancora in corso, altrimenti
        // i due si sommerebbero per il resto dei suoi 160 ms
        self.nav_pan = Task::ready(());
        let delta = e.delta.pixel_delta(window.line_height());
        self.viewport.pan(point(f32::from(delta.x), f32::from(delta.y)));
        self.viewport_changed(cx);
    }

    /// Nodo a cui spetta lo scroll a `position` (finestra): il nodo piu' in alto sotto il
    /// cursore, se il cursore e' sul suo corpo, ha il focus e non e' in vista d'insieme.
    pub(super) fn scroll_owner(&self, position: Point<Pixels>, window: &Window, cx: &App) -> Option<NodeId> {
        let local = self.window_to_local(position);
        let zoom = self.viewport.zoom;
        let origin = |f: &NodeFrame| self.viewport.to_screen(f.origin);
        // l'ultimo disegnato e' quello in primo piano
        let frame = self.model.frames().iter().rev().find(|f| {
            let p = origin(f);
            (p.x..p.x + f.size.width * zoom).contains(&local.x) && (p.y..p.y + f.size.height * zoom).contains(&local.y)
        })?;
        if matches!(frame.kind, NodeKind::Kanban | NodeKind::Automations) || self.node_overview(frame.id, cx) {
            return None;
        }
        // la barra del titolo e' in rem scalati dallo zoom (`render_node`)
        let title = TITLE_BAR_REMS * f32::from(window.rem_size()) * zoom;
        if local.y < origin(frame).y + title {
            return None;
        }
        let focus = self.contents.get(&frame.id)?.focus_handle(cx);
        focus.contains_focused(window, cx).then_some(frame.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::tests::{close_all, redraw, zsh_spec};
    use crate::test_support::workspace_with_repo;
    use gpui::{Modifiers, ScrollDelta, TestAppContext, VisualTestContext};
    use serde_json::json;

    fn at(ms: u64, base: Instant) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn latch_holds_until_a_pause_or_a_new_gesture() {
        let t0 = Instant::now();
        let mut latch = PanLatch::default();
        assert!(!latch.active(TouchPhase::Moved, t0), "nessun gesto");
        latch.hold(t0);
        assert!(latch.active(TouchPhase::Moved, at(PAN_LATCH_MS - 1, t0)));
        // il sollevamento delle dita non sgancia: l'inerzia arriva dopo come Moved
        assert!(latch.active(TouchPhase::Ended, at(10, t0)));
        latch.hold(at(150, t0));
        assert!(latch.active(TouchPhase::Moved, at(300, t0)), "la pausa si conta dall'ultimo scroll");
        assert!(!latch.active(TouchPhase::Moved, at(150 + PAN_LATCH_MS, t0)), "pausa: gesto finito");
        assert!(!latch.active(TouchPhase::Started, at(160, t0)), "dita appoggiate: gesto nuovo");
        latch.release();
        assert!(!latch.active(TouchPhase::Moved, at(160, t0)));
    }

    fn wheel(p: Point<Pixels>, dy: f32, phase: TouchPhase) -> ScrollWheelEvent {
        ScrollWheelEvent {
            position: p,
            delta: ScrollDelta::Pixels(point(px(0.), px(dy))),
            modifiers: Modifiers::default(),
            touch_phase: phase,
        }
    }

    struct Shell {
        canvas: Entity<CanvasView>,
        id: NodeId,
        terminal: Entity<Terminal>,
        body: Point<Pixels>,
        title: Point<Pixels>,
        background: Point<Pixels>,
    }

    // un terminale vero (zsh) con 300 righe di storia da scorrere, il punto al centro
    // del suo corpo, uno sulla barra del titolo e uno sullo sfondo del canvas
    async fn shell_with_history(cx: &mut TestAppContext) -> (Shell, &'static mut VisualTestContext) {
        cx.executor().allow_parking();
        let (_ws, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        canvas.update_in(cx, |c, window, cx| {
            c.open_terminal_with_spec(NodeKind::Shell, None, zsh_spec(), None, window, cx)
        });
        cx.run_until_parked();
        let (id, terminal) = canvas.read_with(cx, |c, _| {
            let (id, (_, t)) = c.terminals.iter().next().expect("terminale");
            (*id, t.clone())
        });
        let lines: String = (1..=300).map(|i| format!("line {i}\n")).collect();
        terminal.update(cx, |t, cx| t.write_output(lines.as_bytes(), cx));
        redraw(cx);
        let body = cx.debug_bounds(super::super::tests::leak(format!("superzed-node-body-{}", id.0))).expect("corpo");
        let title = cx.debug_bounds(super::super::tests::leak(format!("superzed-node-title-{}", id.0))).expect("titolo");
        let b = canvas.read_with(cx, |c, _| c.canvas_bounds.get());
        let background = b.origin + point(px(3.), px(3.));
        assert!(!body.contains(&background) && !title.contains(&background), "sfondo fuori dal nodo");
        let shell = Shell { canvas, id, terminal, body: body.center(), title: title.center(), background };
        (shell, cx)
    }

    impl Shell {
        fn offset(&self, cx: &mut VisualTestContext) -> Point<f32> {
            self.canvas.read_with(cx, |c, _| c.viewport().offset)
        }
        fn history_offset(&self, cx: &mut VisualTestContext) -> usize {
            redraw(cx);
            self.terminal.read_with(cx, |t, _| t.last_content().display_offset)
        }
        fn focus_terminal(&self, cx: &mut VisualTestContext) {
            let id = self.id;
            self.canvas.update_in(cx, |c, window, cx| c.focus_node(id, window, cx));
            redraw(cx);
        }
        fn focus_canvas(&self, cx: &mut VisualTestContext) {
            self.canvas.update_in(cx, |c, window, cx| window.focus(&c.focus_handle, cx));
            redraw(cx);
        }
        // inizio di un gesto come su macOS: dita appoggiate (Started, senza spostamento),
        // poi il primo movimento (il terminale ignora il delta di Started)
        fn gesture(&self, p: Point<Pixels>, dy: f32, cx: &mut VisualTestContext) {
            self.scroll(p, 0., TouchPhase::Started, cx);
            self.scroll(p, dy, TouchPhase::Moved, cx);
        }
        // scroll e ridisegno: il nodo si sposta con il pan come nell'app
        fn scroll(&self, p: Point<Pixels>, dy: f32, phase: TouchPhase, cx: &mut VisualTestContext) {
            cx.simulate_event(wheel(p, dy, phase));
            redraw(cx);
        }
    }

    // (a) chat senza focus: il canvas si sposta, la storia del terminale no
    #[gpui::test]
    async fn scroll_over_an_unfocused_chat_pans(cx: &mut TestAppContext) {
        let (s, cx) = shell_with_history(cx).await;
        s.focus_canvas(cx);
        let before = s.offset(cx);
        s.gesture(s.body, 200., cx);
        assert_eq!(s.offset(cx), point(before.x, before.y + 200.), "pan del canvas");
        assert_eq!(s.history_offset(cx), 0, "la storia non scorre");
        close_all(&s.canvas, cx);
    }

    // (b) chat con il focus: scorre il terminale, il canvas resta; la barra del titolo fa pan
    #[gpui::test]
    async fn scroll_over_the_focused_chat_scrolls_the_terminal(cx: &mut TestAppContext) {
        let (s, cx) = shell_with_history(cx).await;
        s.focus_terminal(cx);
        let before = s.offset(cx);
        s.gesture(s.body, 200., cx);
        assert_eq!(s.offset(cx), before, "niente pan");
        let scrolled = s.history_offset(cx);
        assert!(scrolled > 0, "la storia scorre");
        s.gesture(s.title, 50., cx);
        assert_eq!(s.offset(cx), point(before.x, before.y + 50.), "barra del titolo: pan");
        assert_eq!(s.history_offset(cx), scrolled);
        close_all(&s.canvas, cx);
    }

    // chat con il focus in vista d'insieme: il terminale non si legge, lo scroll fa pan
    #[gpui::test]
    async fn scroll_over_a_focused_chat_in_overview_pans(cx: &mut TestAppContext) {
        let (s, cx) = shell_with_history(cx).await;
        s.focus_terminal(cx);
        s.canvas.update(cx, |c, cx| {
            let f = 0.2 / c.viewport().zoom;
            c.zoom_by(point(0., 0.), f, cx)
        });
        redraw(cx);
        let id = s.id;
        assert!(s.canvas.read_with(cx, |c, cx| c.node_overview(id, cx)), "vista d'insieme");
        let node = cx.debug_bounds(super::super::tests::leak(format!("superzed-node-overview-{}", id.0))).expect("nodo");
        let before = s.offset(cx);
        s.gesture(node.center(), 40., cx);
        assert_eq!(s.offset(cx), point(before.x, before.y + 40.), "pan del canvas");
        assert_eq!(s.history_offset(cx), 0);
        close_all(&s.canvas, cx);
    }

    // (c) gesto iniziato sullo sfondo che passa sopra la chat con il focus: il canvas
    // continua a spostarsi fino alla fine del gesto, inerzia compresa
    #[gpui::test]
    async fn pan_gesture_started_on_the_background_stays_on_the_canvas(cx: &mut TestAppContext) {
        let (s, cx) = shell_with_history(cx).await;
        s.focus_terminal(cx);
        let before = s.offset(cx);
        s.gesture(s.background, 20., cx);
        s.scroll(s.body, 30., TouchPhase::Moved, cx);
        s.scroll(s.body, 30., TouchPhase::Moved, cx);
        s.scroll(s.body, 0., TouchPhase::Ended, cx);
        // inerzia dopo il sollevamento: Moved, a meno di PAN_LATCH_MS dall'ultimo
        cx.executor().advance_clock(Duration::from_millis(PAN_LATCH_MS - 50));
        s.scroll(s.body, 10., TouchPhase::Moved, cx);
        assert_eq!(s.offset(cx), point(before.x, before.y + 90.), "tutto il gesto al canvas");
        assert_eq!(s.history_offset(cx), 0, "il terminale non ha ricevuto niente");
        close_all(&s.canvas, cx);
    }

    // (d) dopo la fine del gesto (nuove dita o pausa) lo scroll sulla chat con il focus
    // torna al terminale
    #[gpui::test]
    async fn after_the_gesture_the_focused_chat_scrolls_again(cx: &mut TestAppContext) {
        let (s, cx) = shell_with_history(cx).await;
        s.focus_terminal(cx);
        s.gesture(s.background, 20., cx);
        s.scroll(s.body, 0., TouchPhase::Ended, cx);
        let panned = s.offset(cx);
        s.gesture(s.body, 200., cx);
        assert_eq!(s.offset(cx), panned, "gesto nuovo: niente pan");
        let scrolled = s.history_offset(cx);
        assert!(scrolled > 0, "gesto nuovo: la storia scorre");
        // rotella del mouse (sempre Moved): finisce con la pausa
        s.scroll(s.background, 20., TouchPhase::Moved, cx);
        let panned = s.offset(cx);
        cx.executor().advance_clock(Duration::from_millis(PAN_LATCH_MS + 1));
        s.scroll(s.body, -100., TouchPhase::Moved, cx);
        assert_eq!(s.offset(cx), panned, "dopo la pausa: niente pan");
        assert!(s.history_offset(cx) < scrolled, "dopo la pausa: la storia scorre");
        close_all(&s.canvas, cx);
    }
}
