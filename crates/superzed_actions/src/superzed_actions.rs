//! Azioni GPUI di superzed (namespace `superzed::`). Stanno in un crate leggero
//! cosi' le usano `workspace`, `terminal_view` e i crate superzed senza cicli.
use gpui::{Action, actions};
use schemars::JsonSchema;
use serde::Deserialize;

actions!(
    superzed,
    [
        /// Crea un nodo terminale (shell di login) al centro del canvas.
        NewTerminal,
        /// Crea un nodo claude: shell di login che avvia `claude`.
        NewClaudeNode,
        /// Crea un nodo nota al centro del canvas.
        NewNote,
        /// Porta il focus sul canvas.
        FocusCanvas,
        /// Porta il focus sull'area editor (apre il file finder se non ci sono file).
        FocusEditor,
        /// Passa alla tab di progetto successiva.
        NextProjectTab,
        /// Passa alla tab di progetto precedente.
        PrevProjectTab,
        /// Chiude la tab di progetto attiva, dopo conferma.
        CloseProjectTab,
        /// Ingrandisce il canvas.
        CanvasZoomIn,
        /// Riduce il canvas.
        CanvasZoomOut,
        /// Riporta lo zoom del canvas a 1.
        CanvasZoomReset,
        /// Porta in vista il nodo con il focus (o quello selezionato): centrato a zoom 1,
        /// o allo zoom che lo fa stare nel canvas visibile.
        ZoomToFocusedNode,
        /// Chiude i nodi selezionati (o quello con il focus), dopo conferma.
        CloseNode,
        /// Seleziona tutti i nodi del canvas.
        SelectAllNodes,
        /// Toglie la selezione.
        DeselectNodes,
        /// Porta il focus dentro il nodo selezionato.
        FocusSelectedNode,
        /// Rinomina il nodo selezionato.
        RenameNode,
        /// Passa al colore successivo per i nodi selezionati.
        CycleNodeColor,
        /// Rende golden (o normale) il nodo con il focus o i nodi selezionati: bordo
        /// dorato, in cima alle Sessioni, risparmiato da «Close idle chats».
        ToggleGolden,
        /// Rinomina la tab di progetto attiva.
        RenameProjectTab,
        /// Passa al colore successivo per la tab di progetto attiva.
        CycleProjectTabColor,
        /// Apre il kanban del progetto (o porta il focus su quello esistente).
        OpenKanban,
        /// Sposta la card selezionata del kanban nella colonna successiva.
        MoveCardNext,
        /// Sposta la card selezionata del kanban nella colonna precedente.
        MoveCardPrev,
        /// Apre il picker degli account claude e crea una chat sull'account scelto.
        NewClaudeNodePick,
        /// Apre il picker dei prompt prefabbricati e crea una chat con il prompt scelto.
        NewClaudeNodeFromPrompt,
        /// Focus sulla chat sopra: nel kanban la precedente della colonna, nelle
        /// Automazioni la fascia precedente, fra i nodi liberi il piu' vicino verso l'alto.
        FocusChatUp,
        /// Focus sulla chat sotto (come `FocusChatUp`, verso il basso).
        FocusChatDown,
        /// Focus sulla chat a sinistra: nel kanban la colonna precedente non vuota, alla
        /// chat piu' vicina in altezza.
        FocusChatLeft,
        /// Focus sulla chat a destra (come `FocusChatLeft`, verso destra).
        FocusChatRight,
        /// Passa all'area successiva del canvas: kanban → Automazioni → nodi liberi.
        FocusNextArea,
        /// Passa all'area precedente del canvas: kanban → nodi liberi → Automazioni.
        FocusPreviousArea,
        /// Apre il pannello Sessions con la sezione «Limits» aperta (clic sui limiti
        /// della status bar).
        ShowAccountLimits,
    ]
);

/// Attiva la tab di progetto n (1-based).
#[derive(Clone, Debug, Default, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = superzed)]
pub struct ActivateProjectTab(pub usize);

/// Mostra la vista n della sidebar: 1 file, 2 git, 3 sessioni, 4 ricerca, 5 notifiche, 6 fonti.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = superzed)]
pub struct SidebarView(pub usize);

/// Sposta i nodi selezionati di (dx, dy), in coordinate del canvas.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = superzed)]
#[serde(deny_unknown_fields)]
pub struct MoveSelectedNodes {
    pub dx: f32,
    pub dy: f32,
}

/// Tutte le azioni superzed, per i test del keymap.
pub const ALL_ACTION_NAMES: &[&str] = &[
    "superzed::NewTerminal",
    "superzed::NewClaudeNode",
    "superzed::NewNote",
    "superzed::FocusCanvas",
    "superzed::FocusEditor",
    "superzed::NextProjectTab",
    "superzed::PrevProjectTab",
    "superzed::CloseProjectTab",
    "superzed::CanvasZoomIn",
    "superzed::CanvasZoomOut",
    "superzed::CanvasZoomReset",
    "superzed::ZoomToFocusedNode",
    "superzed::CloseNode",
    "superzed::SelectAllNodes",
    "superzed::DeselectNodes",
    "superzed::FocusSelectedNode",
    "superzed::RenameNode",
    "superzed::CycleNodeColor",
    "superzed::ToggleGolden",
    "superzed::RenameProjectTab",
    "superzed::CycleProjectTabColor",
    "superzed::ActivateProjectTab",
    "superzed::SidebarView",
    "superzed::MoveSelectedNodes",
    "superzed::OpenKanban",
    "superzed::MoveCardNext",
    "superzed::MoveCardPrev",
    "superzed::NewClaudeNodePick",
    "superzed::NewClaudeNodeFromPrompt",
    "superzed::FocusChatUp",
    "superzed::FocusChatDown",
    "superzed::FocusChatLeft",
    "superzed::FocusChatRight",
    "superzed::FocusNextArea",
    "superzed::FocusPreviousArea",
    "superzed::ShowAccountLimits",
];

/// Non registra nulla: le azioni si registrano da sole. Serve a linkare il crate.
pub fn init(_cx: &mut gpui::App) {}

#[cfg(test)]
mod tests {
    use super::ALL_ACTION_NAMES;
    use gpui::TestAppContext;

    // ogni azione superzed registrata e' nell'elenco, e viceversa: l'elenco serve
    // ai test del keymap (Task 18) per verificare che ogni azione abbia un binding
    #[gpui::test]
    fn every_superzed_action_is_listed(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let mut registered: Vec<&str> = cx
                .all_action_names()
                .iter()
                .copied()
                .filter(|name| name.starts_with("superzed::"))
                .collect();
            registered.sort_unstable();
            let mut listed = ALL_ACTION_NAMES.to_vec();
            listed.sort_unstable();
            assert_eq!(registered, listed);
            assert!(!listed.is_empty(), "nessuna azione superzed registrata");
        });
    }
}
