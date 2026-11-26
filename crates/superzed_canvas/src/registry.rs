//! Registro dei canvas aperti (uno per workspace) e instradamento delle notifiche
//! del canale `notify` (la chiave del nodo dice progetto e sorgente) e delle attese del
//! canale `wait`.
use crate::CanvasView;
use gpui::{App, Entity, Global, WeakEntity};

#[derive(Default)]
struct CanvasRegistry(Vec<WeakEntity<CanvasView>>);
impl Global for CanvasRegistry {}

pub fn register(canvas: WeakEntity<CanvasView>, cx: &mut App) {
    let registry = cx.default_global::<CanvasRegistry>();
    registry.0.retain(|c| c.upgrade().is_some());
    registry.0.push(canvas);
}

pub fn canvases(cx: &App) -> Vec<Entity<CanvasView>> {
    cx.try_global::<CanvasRegistry>()
        .map(|r| r.0.iter().filter_map(|c| c.upgrade()).collect())
        .unwrap_or_default()
}

/// Attesa del canale `wait`: al primo canvas che ha la chat; nessuno → una riga d'errore.
pub fn route_wait(key: &str, payload: serde_json::Value, reply: superzed_ipc::IpcReply, cx: &mut App) {
    for canvas in canvases(cx) {
        if canvas.update(cx, |c, cx| c.request_wait(key, payload.clone(), reply.clone(), cx)) {
            return;
        }
    }
    reply.send("error: this chat does not belong to an open project");
}

pub fn route_notify(key: &str, payload: serde_json::Value, cx: &mut App) {
    let input = superzed_notifications::parse_notify_payload(&payload);
    let origin = canvases(cx).into_iter().find_map(|c| c.update(cx, |c, cx| c.notification_origin(key, cx)));
    let (project_root, source) = origin.unwrap_or((None, "external".to_string()));
    if let Some(store) = superzed_notifications::NotificationStore::global(cx) {
        store.update(cx, |s, cx| {
            s.add(input.into_new(project_root, source), cx);
        });
    }
}
