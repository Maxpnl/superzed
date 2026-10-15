//! La visuale segue la chat con il focus quando cambia colonna del kanban (Task 15).
//!
//! A ogni relayout del kanban si confronta la fase di ogni chat agganciata con quella
//! dell'ultimo relayout: se la chat seguita (quella con il focus, altrimenti la
//! selezionata) e' passata a un'altra colonna e non e' interamente visibile, la viewport
//! si sposta per centrarla, zoom invariato, con l'animazione annullabile della
//! navigazione. Un rilascio di un drag non la segue: li' l'utente sta gia' guardando.
use super::*;

impl CanvasView {
    /// Chat del kanban da seguire: quella con il focus del terminale (la sessione che lo
    /// store sa guardata); se nessuna chat di questo canvas ha il focus, la selezionata
    /// in primo piano. Il focus su una chat libera non segue niente.
    fn follow_target(&self, cx: &App) -> Option<NodeId> {
        let docked = |id: NodeId| self.model.frame(id).is_some_and(|f| f.kind == NodeKind::Claude && f.in_kanban());
        let store = superzed_sessions::SessionStore::global(cx);
        let focused = store.as_ref().and_then(|s| s.read(cx).focused_key()).and_then(|k| self.node_for_session_key(k));
        match focused {
            Some(id) => docked(id).then_some(id),
            None => {
                self.model.frames().iter().rev().find(|f| self.model.is_selected(f.id) && docked(f.id)).map(|f| f.id)
            }
        }
    }

    /// Prima del relayout del kanban: ricorda le fasi di questo layout e dice se la chat
    /// da seguire ha cambiato colonna dall'ultimo (non se e' appena arrivata nel kanban,
    /// ne' durante il rilascio di un drag, ne' mentre un drag e' in corso: in entrambi i
    /// casi l'utente sta gia' guardando/manipolando la vista di suo).
    pub(super) fn column_change_to_follow(
        &mut self,
        chats: &[(NodeId, superzed_sessions::Phase)],
        cx: &App,
    ) -> Option<NodeId> {
        let target = self.follow_target(cx);
        let previous = std::mem::replace(&mut self.kanban_phases, chats.iter().copied().collect());
        let id = target?;
        let now = chats.iter().find(|(n, _)| *n == id)?.1;
        (!self.dropping && self.drag.is_none() && previous.get(&id).is_some_and(|before| *before != now)).then_some(id)
    }

    /// Dopo il relayout: se la chat non e' interamente visibile la viewport si sposta per
    /// centrarla (zoom invariato, animazione annullabile della navigazione). Il focus
    /// resta dov'e'.
    pub(super) fn follow_chat(&mut self, id: NodeId, cx: &mut Context<Self>) {
        let Some((origin, size)) = self.model.frame(id).map(|f| (f.origin, f.size)) else {
            return;
        };
        let view = self.canvas_bounds.get().size;
        let view = gpui::size(f32::from(view.width), f32::from(view.height));
        // canvas non ancora disegnato: non c'e' una vista da centrare
        if view.width <= 0. || view.height <= 0. {
            return;
        }
        if let Some(delta) = navigation::center_pan(self.viewport, origin, size, view) {
            self.animate_pan(delta, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::navigation::NAV_PAN_MS;
    use super::super::tests::{close_all, hook, kanban_with_claude_chats, redraw, store};
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    use serde_json::json;
    use superzed_sessions::Phase;

    fn view_size(canvas: &Entity<CanvasView>, cx: &mut VisualTestContext) -> Size<f32> {
        canvas.read_with(cx, |c, _| {
            let s = c.canvas_bounds.get().size;
            size(f32::from(s.width), f32::from(s.height))
        })
    }

    fn rect(canvas: &Entity<CanvasView>, id: NodeId, cx: &mut VisualTestContext) -> (Point<f32>, Size<f32>) {
        canvas.read_with(cx, |c, _| c.model.frame(id).map(|f| (f.origin, f.size)).unwrap())
    }

    fn centered(canvas: &Entity<CanvasView>, id: NodeId, cx: &mut VisualTestContext) -> bool {
        let (o, s) = rect(canvas, id, cx);
        canvas.read_with(cx, |c, _| {
            let p = c.viewport.to_screen(point(o.x + s.width / 2., o.y + s.height / 2.));
            let center = c.local_center();
            (p.x - center.x).abs() < 0.5 && (p.y - center.y).abs() < 0.5
        })
    }

    fn fully_visible(canvas: &Entity<CanvasView>, id: NodeId, cx: &mut VisualTestContext) -> bool {
        let (o, s) = rect(canvas, id, cx);
        let view = view_size(canvas, cx);
        canvas.read_with(cx, |c, _| navigation::center_pan(c.viewport, o, s, view).is_none())
    }

    // due chat in Nuove; zoom `zoom` e viewport centrata sulla prima
    async fn kanban_centered_on_new(
        zoom: f32,
        cx: &mut TestAppContext,
    ) -> (Entity<CanvasView>, Vec<NodeId>, &'static mut VisualTestContext) {
        let (canvas, _kanban, chats, cx) = kanban_with_claude_chats(2, cx).await;
        canvas.update(cx, |c, cx| {
            c.dock(chats[0], Phase::New, cx);
            c.dock(chats[1], Phase::New, cx);
        });
        cx.run_until_parked();
        redraw(cx);
        let (o, s) = rect(&canvas, chats[0], cx);
        canvas.update(cx, |c, cx| {
            c.viewport.zoom = zoom;
            let center = c.local_center();
            c.viewport.offset =
                point(center.x - (o.x + s.width / 2.) * zoom, center.y - (o.y + s.height / 2.) * zoom);
            cx.notify();
        });
        redraw(cx);
        (canvas, chats, cx)
    }

    fn focus_terminal(canvas: &Entity<CanvasView>, id: NodeId, cx: &mut VisualTestContext) {
        canvas.update_in(cx, |c, window, cx| c.focus_node(id, window, cx));
        redraw(cx);
    }

    fn terminal_focused(canvas: &Entity<CanvasView>, id: NodeId, cx: &mut VisualTestContext) -> bool {
        let view = canvas.read_with(cx, |c, _| c.terminals.get(&id).map(|(v, _)| v.clone()).unwrap());
        cx.update(|window, cx| view.focus_handle(cx).is_focused(window))
    }

    // un hook porta la sessione `key` nella fase Piano
    fn hook_to_plan(key: &str, cx: &mut VisualTestContext) {
        store(cx).update(cx, |s, cx| {
            s.apply(
                hook(
                    key,
                    json!({"hook_event_name": "PreToolUse", "tool_name": "Skill",
                        "tool_input": {"skill": "superpowers:writing-plans"}}),
                ),
                cx,
            )
        });
    }

    fn settle(cx: &mut VisualTestContext) {
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(NAV_PAN_MS + 50));
        redraw(cx);
    }

    fn phase(canvas: &Entity<CanvasView>, id: NodeId, cx: &mut VisualTestContext) -> Phase {
        canvas.read_with(cx, |c, cx| c.docked_chats(cx).into_iter().find(|(n, _)| *n == id).unwrap().1)
    }

    #[gpui::test]
    async fn the_view_follows_the_focused_chat_to_an_off_screen_column(cx: &mut TestAppContext) {
        let (canvas, chats, cx) = kanban_centered_on_new(1., cx).await;
        focus_terminal(&canvas, chats[0], cx);
        let zoom = canvas.read_with(cx, |c, _| c.viewport.zoom);
        hook_to_plan("kc-0", cx);
        cx.run_until_parked();
        assert_eq!(phase(&canvas, chats[0], cx), Phase::Plan);
        settle(cx);
        assert!(centered(&canvas, chats[0], cx), "la viewport e' centrata sulla chat nella nuova colonna");
        assert_eq!(canvas.read_with(cx, |c, _| c.viewport.zoom), zoom, "zoom invariato");
        assert!(terminal_focused(&canvas, chats[0], cx), "il focus resta nel terminale della chat");
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn the_view_follows_the_selected_chat_when_no_chat_has_focus(cx: &mut TestAppContext) {
        let (canvas, chats, cx) = kanban_centered_on_new(1., cx).await;
        canvas.update_in(cx, |c, window, cx| {
            c.model.select(chats[0], false);
            window.focus(&c.focus_handle, cx);
        });
        redraw(cx);
        hook_to_plan("kc-0", cx);
        settle(cx);
        assert!(centered(&canvas, chats[0], cx), "nessuna chat con il focus: segue la selezionata");
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn a_chat_without_focus_or_selection_does_not_move_the_view(cx: &mut TestAppContext) {
        let (canvas, chats, cx) = kanban_centered_on_new(1., cx).await;
        canvas.update_in(cx, |c, window, cx| {
            c.model.clear_selection();
            window.focus(&c.focus_handle, cx);
        });
        redraw(cx);
        let before = canvas.read_with(cx, |c, _| c.viewport);
        hook_to_plan("kc-0", cx);
        settle(cx);
        assert_eq!(phase(&canvas, chats[0], cx), Phase::Plan);
        assert_eq!(canvas.read_with(cx, |c, _| c.viewport), before, "viewport invariata");
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn a_focused_chat_that_stays_visible_does_not_move_the_view(cx: &mut TestAppContext) {
        // zoom basso: tutto il kanban nella vista
        let (canvas, chats, cx) = kanban_centered_on_new(0.05, cx).await;
        focus_terminal(&canvas, chats[0], cx);
        let before = canvas.read_with(cx, |c, _| c.viewport);
        hook_to_plan("kc-0", cx);
        settle(cx);
        assert_eq!(phase(&canvas, chats[0], cx), Phase::Plan);
        assert!(fully_visible(&canvas, chats[0], cx), "la chat resta interamente visibile");
        assert_eq!(canvas.read_with(cx, |c, _| c.viewport), before, "viewport invariata");
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn the_view_follows_a_cleared_chat_back_to_new(cx: &mut TestAppContext) {
        let (canvas, chats, cx) = kanban_centered_on_new(1., cx).await;
        focus_terminal(&canvas, chats[0], cx);
        hook_to_plan("kc-0", cx);
        settle(cx);
        // ci si sposta altrove, poi /clear la riporta in Nuove
        canvas.update(cx, |c, cx| {
            c.viewport.pan(point(-5000., 0.));
            cx.notify();
        });
        redraw(cx);
        store(cx).update(cx, |s, cx| {
            s.apply(hook("kc-0", json!({"hook_event_name": "SessionStart", "source": "clear", "session_id": "s2"})), cx)
        });
        settle(cx);
        assert_eq!(phase(&canvas, chats[0], cx), Phase::New);
        assert!(centered(&canvas, chats[0], cx), "/clear: la vista segue la chat in Nuove");
        close_all(&canvas, cx);
    }

    // Final fix 5, punto 1: un drag (spazio + trascinamento) in corso non deve essere
    // scavalcato da un follow (nav_pan) quando un hook cambia fase della chat seguita.
    #[gpui::test]
    async fn a_phase_change_during_a_pan_drag_does_not_move_the_view_beyond_the_drag(cx: &mut TestAppContext) {
        let (canvas, chats, cx) = kanban_centered_on_new(1., cx).await;
        focus_terminal(&canvas, chats[0], cx);
        // il drag e' gia' in corso (spazio + trascinamento sullo sfondo o sul corpo del
        // kanban): l'utente ha gia' spostato la viewport del proprio delta
        canvas.update(cx, |c, cx| {
            c.drag = Some(DragState::Pan { last: point(px(0.), px(0.)) });
            c.viewport.pan(point(40., 20.));
            cx.notify();
        });
        redraw(cx);
        let during_drag = canvas.read_with(cx, |c, _| c.viewport);
        hook_to_plan("kc-0", cx);
        settle(cx);
        assert_eq!(phase(&canvas, chats[0], cx), Phase::Plan);
        assert_eq!(
            canvas.read_with(cx, |c, _| c.viewport),
            during_drag,
            "un drag in corso: la vista non deve spostarsi oltre il pan dell'utente"
        );
        canvas.update(cx, |c, _| c.drag = None);
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn dropping_a_dragged_chat_on_another_column_does_not_move_the_view(cx: &mut TestAppContext) {
        let (canvas, chats, cx) = kanban_centered_on_new(1., cx).await;
        let on_new = canvas.read_with(cx, |c, _| c.viewport);
        // la seconda chat in Piano segna dov'e' la colonna
        canvas.update(cx, |c, cx| c.set_session_phase(chats[1], Phase::Plan, cx));
        // (la seconda chat, l'ultima aperta, aveva il focus: la vista l'ha seguita e la
        // si riporta su Nuove, con Piano fuori schermo)
        settle(cx);
        focus_terminal(&canvas, chats[0], cx);
        canvas.update(cx, |c, cx| {
            c.viewport = on_new;
            cx.notify();
        });
        redraw(cx);
        let before = canvas.read_with(cx, |c, _| c.viewport);
        let (plan_origin, _) = rect(&canvas, chats[1], cx);
        canvas.update(cx, |c, cx| {
            c.model.select(chats[0], false);
            c.drag = Some(DragState::Move { last: point(px(0.), px(0.)), moved: true });
            if let Some(f) = c.model.frame_mut(chats[0]) {
                f.origin = point(plan_origin.x, plan_origin.y + 20.);
            }
            c.finish_node_drag(cx);
        });
        settle(cx);
        assert_eq!(phase(&canvas, chats[0], cx), Phase::Plan, "agganciata a Piano dal rilascio");
        assert_eq!(canvas.read_with(cx, |c, _| c.viewport), before, "rilascio di un drag: viewport invariata");
        close_all(&canvas, cx);
    }
}
