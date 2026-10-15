use crate::model::{DockKind, NodeFrame, NodeId, NodeKind};
use anyhow::Context as _;
use fs::{Fs, RenameOptions};
use gpui::{point, size};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const PROJECT_FILE_VERSION: u32 = 1;
const PROJECT_DIR: &str = ".superzed";
const PROJECT_FILE: &str = "project.json";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawProjectFile", into = "RawProjectFile")]
pub struct ProjectFile {
    pub version: u32,
    pub viewport: ViewportRecord,
    pub editor_width: Option<f32>,
    pub editor_font_size: Option<f32>,
    pub tab: TabRecord,
    pub nodes: Vec<NodeRecord>,
    /// Connessioni (vista Fonti) usate da questo progetto.
    pub sources: Vec<String>,
    /// Nodi di un tipo che questa versione non conosce (versione futura o precedente):
    /// non si mostrano, si riscrivono tali e quali in coda a `nodes`.
    pub foreign_nodes: Vec<serde_json::Value>,
}

/// Forma su disco: i nodi restano JSON finche' non si sa se il loro tipo e' noto.
#[derive(Clone, Serialize, Deserialize)]
struct RawProjectFile {
    version: u32,
    #[serde(default)]
    viewport: ViewportRecord,
    #[serde(default)]
    editor_width: Option<f32>,
    #[serde(default)]
    editor_font_size: Option<f32>,
    #[serde(default)]
    tab: TabRecord,
    #[serde(default)]
    nodes: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    sources: Vec<String>,
}

/// Record con un id numerico e un `kind` stringa che questa versione non conosce.
fn is_foreign_node(value: &serde_json::Value) -> bool {
    let known = |k: &str| {
        serde_json::from_value::<NodeKind>(serde_json::Value::String(k.to_string())).is_ok()
    };
    value.get("id").is_some_and(|id| id.as_u64().is_some())
        && value
            .get("kind")
            .and_then(|k| k.as_str())
            .is_some_and(|k| !known(k))
}

impl TryFrom<RawProjectFile> for ProjectFile {
    type Error = String;

    fn try_from(raw: RawProjectFile) -> Result<Self, String> {
        let mut nodes = Vec::new();
        let mut foreign_nodes = Vec::new();
        for (ix, value) in raw.nodes.into_iter().enumerate() {
            match serde_json::from_value::<NodeRecord>(value.clone()) {
                Ok(record) => nodes.push(record),
                Err(_) if is_foreign_node(&value) => foreign_nodes.push(value),
                Err(e) => return Err(format!("node {}: {e}", ix + 1)),
            }
        }
        Ok(ProjectFile {
            version: raw.version,
            viewport: raw.viewport,
            editor_width: raw.editor_width,
            editor_font_size: raw.editor_font_size,
            tab: raw.tab,
            nodes,
            sources: raw.sources,
            foreign_nodes,
        })
    }
}

impl From<ProjectFile> for RawProjectFile {
    fn from(file: ProjectFile) -> Self {
        let nodes = file
            .nodes
            .iter()
            .map(|n| serde_json::to_value(n).unwrap_or(serde_json::Value::Null))
            .chain(file.foreign_nodes)
            .collect();
        RawProjectFile {
            version: file.version,
            viewport: file.viewport,
            editor_width: file.editor_width,
            editor_font_size: file.editor_font_size,
            tab: file.tab,
            nodes,
            sources: file.sources,
        }
    }
}

impl Default for ProjectFile {
    fn default() -> Self {
        Self {
            version: PROJECT_FILE_VERSION,
            viewport: ViewportRecord::default(),
            editor_width: None,
            editor_font_size: None,
            tab: TabRecord::default(),
            nodes: Vec::new(),
            sources: Vec::new(),
            foreign_nodes: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ViewportRecord {
    pub offset: [f32; 2],
    pub zoom: f32,
}

impl Default for ViewportRecord {
    fn default() -> Self {
        Self { offset: [0., 0.], zoom: 1.0 }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TabRecord {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub color: Option<u8>,
}

/// Un `dock` sconosciuto (versione futura, file rovinato) vale "libera": la chat si
/// carica comunque, invece di rendere illeggibile tutto il file.
fn lenient_dock<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<DockKind>, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(value).ok().flatten())
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodeRecord {
    pub id: u64,
    pub kind: NodeKind,
    pub origin: [f32; 2],
    pub size: [f32; 2],
    pub title: String,
    #[serde(default)]
    pub color: Option<u8>,
    #[serde(default)]
    pub group: Option<String>,
    /// Contenitore a cui la chat e' agganciata.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "lenient_dock")]
    pub dock: Option<DockKind>,
    /// Formato vecchio (prima dei contenitori generici): si legge soltanto, come
    /// `dock: kanban`; si scrive solo `dock`.
    #[serde(default, skip_serializing)]
    pub in_kanban: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub golden: bool,
    /// Dati specifici del tipo di nodo (per la nota: `{"text": ...}`).
    #[serde(default)]
    pub data: serde_json::Value,
}

impl NodeRecord {
    pub fn from_frame(frame: &NodeFrame, data: serde_json::Value) -> Self {
        Self {
            id: frame.id.0,
            kind: frame.kind,
            origin: [frame.origin.x, frame.origin.y],
            size: [frame.size.width, frame.size.height],
            title: frame.title.clone(),
            color: frame.color,
            group: frame.group.clone(),
            dock: frame.dock,
            in_kanban: false,
            golden: frame.golden,
            data,
        }
    }

    pub fn to_frame(&self) -> NodeFrame {
        NodeFrame {
            id: NodeId(self.id),
            kind: self.kind,
            origin: point(self.origin[0], self.origin[1]),
            size: size(self.size[0], self.size[1]),
            title: self.title.clone(),
            color: self.color,
            group: self.group.clone(),
            dock: self.dock.or(self.in_kanban.then_some(DockKind::Kanban)),
            golden: self.golden,
        }
    }
}

pub fn project_file_path(root: &Path) -> PathBuf {
    root.join(PROJECT_DIR).join(PROJECT_FILE)
}

pub fn parse_project_file(text: &str) -> Result<ProjectFile, String> {
    let file: ProjectFile = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if file.version == 0 || file.version > PROJECT_FILE_VERSION {
        return Err(format!("unknown version {}", file.version));
    }
    Ok(file)
}

pub enum LoadOutcome {
    Missing,
    Loaded(ProjectFile),
    /// Il file non era leggibile: e' stato spostato in `backup`.
    /// Se `backup == project_file_path(root)` lo spostamento non e' riuscito: il file
    /// e' rimasto dov'era e il salvataggio deve restare bloccato (non va sovrascritto).
    Recovered { backup: PathBuf, reason: String },
}

pub async fn load_project_file(fs: &dyn Fs, root: &Path) -> LoadOutcome {
    let path = project_file_path(root);
    if !fs.is_file(&path).await {
        return LoadOutcome::Missing;
    }
    let reason = match fs.load(&path).await {
        Ok(text) => match parse_project_file(&text) {
            Ok(file) => return LoadOutcome::Loaded(file),
            Err(e) => e,
        },
        Err(e) => e.to_string(),
    };
    match move_to_backup(fs, &path).await {
        Ok(backup) => LoadOutcome::Recovered { backup, reason },
        // non si riesce a spostarlo: lo si lascia dov'e' e si parte vuoti; il
        // salvataggio resta bloccato (vedi CanvasView::save_blocked)
        Err(e) => LoadOutcome::Recovered {
            backup: path.clone(),
            reason: format!("{reason}; backup failed: {e}"),
        },
    }
}

/// `project.json.bak`, oppure `.bak.1` … `.bak.99` se esistono gia'.
async fn move_to_backup(fs: &dyn Fs, path: &Path) -> anyhow::Result<PathBuf> {
    let base = path.with_extension("json.bak");
    let mut candidate = base.clone();
    let mut n = 0;
    while fs.is_file(&candidate).await {
        n += 1;
        anyhow::ensure!(n < 100, "too many project.json backups");
        candidate = PathBuf::from(format!("{}.{n}", base.display()));
    }
    fs.rename(path, &candidate, RenameOptions::default())
        .await
        .context("renaming project.json")?;
    Ok(candidate)
}

pub async fn save_project_file(fs: &dyn Fs, root: &Path, file: &ProjectFile) -> anyhow::Result<()> {
    let path = project_file_path(root);
    let dir = path.parent().context("project.json has no folder")?;
    fs.create_dir(dir).await?;
    let text = serde_json::to_string_pretty(file)?;
    fs.atomic_write(path, text).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DockKind, NodeFrame, NodeId, NodeKind};
    use fs::FakeFs;
    use gpui::{TestAppContext, point, size};
    use serde_json::json;
    use std::path::Path;

    fn sample() -> ProjectFile {
        let frame = NodeFrame {
            id: NodeId(3),
            kind: NodeKind::Claude,
            origin: point(10., 20.),
            size: size(640., 400.),
            title: "claude".into(),
            color: Some(2),
            group: Some("backend".into()),
            dock: Some(DockKind::Kanban),
            golden: true,
        };
        ProjectFile {
            version: PROJECT_FILE_VERSION,
            viewport: ViewportRecord { offset: [5., -5.], zoom: 1.5 },
            editor_width: Some(700.),
            editor_font_size: Some(16.),
            tab: TabRecord { name: Some("app".into()), color: Some(4) },
            nodes: vec![NodeRecord::from_frame(&frame, json!({}))],
            sources: vec!["gh".into()],
            foreign_nodes: vec![],
        }
    }

    // Review Focus 3
    #[test]
    fn foreign_nodes_roundtrip_verbatim() {
        let text = r#"{"version":1,"nodes":[
            {"id":5,"kind":"note","origin":[0,0],"size":[300,200],"title":"n","data":{"text":"ciao"}},
            {"id":9,"kind":"lavagna","origin":[1,2],"size":[10,10],"title":"x","data":{"k":[1,2]},"extra":true}]}"#;
        let file = parse_project_file(text).unwrap();
        assert_eq!(file.nodes.len(), 1);
        assert_eq!(file.foreign_nodes.len(), 1);
        let out = serde_json::to_string(&file).unwrap();
        assert!(out.contains(r#""kind":"lavagna""#) && out.contains(r#""extra":true"#), "{out}");
        assert_eq!(parse_project_file(&out).unwrap(), file);
        // senza id o con un tipo non stringa resta un errore (file rovinato, non una versione futura)
        assert!(parse_project_file(r#"{"version":1,"nodes":[{"kind":"lavagna"}]}"#).is_err());
        assert!(parse_project_file(r#"{"version":1,"nodes":[{"id":1,"kind":"note"}]}"#).is_err(), "tipo noto ma record rotto");
    }

    #[test]
    fn source_nodes_and_project_sources() {
        let file = parse_project_file(r#"{"version":1,"sources":["gh","hx"],"nodes":[
            {"id":1,"kind":"source","origin":[0,0],"size":[560,440],"title":"app","data":{"connection":"gh","filter":{}}}]}"#).unwrap();
        assert_eq!(file.nodes[0].kind, NodeKind::Source);
        assert_eq!(file.sources, ["gh", "hx"]);
        let empty = serde_json::to_string(&parse_project_file(r#"{"version":1}"#).unwrap()).unwrap();
        assert!(!empty.contains("sources"), "omesso se vuoto: {empty}");
    }

    #[test]
    fn roundtrip_keeps_everything() {
        let text = serde_json::to_string_pretty(&sample()).unwrap();
        assert_eq!(parse_project_file(&text).unwrap(), sample());
        let frame = parse_project_file(&text).unwrap().nodes[0].to_frame();
        assert_eq!(frame.id, NodeId(3));
        assert_eq!(frame.kind, NodeKind::Claude);
        assert_eq!(frame.group.as_deref(), Some("backend"));
        assert_eq!(frame.dock, Some(DockKind::Kanban));
        assert!(frame.golden);
    }

    #[test]
    fn golden_is_written_only_when_set_and_old_records_read_false() {
        let file = parse_project_file(r#"{"version":1,"nodes":[{"id":1,"kind":"claude","origin":[0,0],"size":[10,10],"title":"c"}]}"#).unwrap();
        assert!(!file.nodes[0].to_frame().golden);
        assert!(!serde_json::to_string(&file).unwrap().contains("golden"));
        let text = serde_json::to_string(&sample()).unwrap();
        assert!(text.contains(r#""golden":true"#), "{text}");
    }

    // lo zoom e' solo del canvas: un `font_scale` salvato da una versione precedente
    // si ignora (il file si legge lo stesso)
    #[test]
    fn saved_font_scale_is_ignored() {
        let file = parse_project_file(r#"{"version":1,"nodes":[{"id":1,"kind":"shell","origin":[0,0],"size":[10,10],"title":"s","font_scale":1.5}]}"#).unwrap();
        assert_eq!(file.nodes[0].to_frame().title, "s");
        assert!(!serde_json::to_string(&file).unwrap().contains("font_scale"));
    }

    #[test]
    fn old_records_without_in_kanban_load_as_free() {
        let file = parse_project_file(r#"{"version":1,"nodes":[{"id":1,"kind":"claude","origin":[0,0],"size":[10,10],"title":"claude"}]}"#).unwrap();
        assert_eq!(file.nodes[0].to_frame().dock, None);
    }

    #[test]
    fn old_in_kanban_reads_as_kanban_dock_and_dock_is_written() {
        let file = parse_project_file(r#"{"version":1,"nodes":[{"id":1,"kind":"claude","origin":[0,0],"size":[10,10],"title":"c","in_kanban":true}]}"#).unwrap();
        let frame = file.nodes[0].to_frame();
        assert_eq!(frame.dock, Some(DockKind::Kanban));
        assert!(frame.in_kanban());
        let text = serde_json::to_string(&NodeRecord::from_frame(&frame, serde_json::Value::Null)).unwrap();
        assert!(text.contains(r#""dock":"kanban""#), "{text}");
        assert!(!text.contains("in_kanban"), "si scrive solo dock");
        let a = parse_project_file(r#"{"version":1,"nodes":[
            {"id":2,"kind":"automations","origin":[0,0],"size":[10,10],"title":"automazioni"},
            {"id":3,"kind":"claude","origin":[0,0],"size":[10,10],"title":"x","dock":"automations"}]}"#).unwrap();
        assert_eq!(a.nodes[0].kind, NodeKind::Automations);
        assert_eq!(a.nodes[1].to_frame().dock, Some(DockKind::Automations));
        let free = parse_project_file(r#"{"version":1,"nodes":[{"id":4,"kind":"shell","origin":[0,0],"size":[10,10],"title":"s"}]}"#).unwrap();
        assert_eq!(free.nodes[0].to_frame().dock, None);
    }

    // Revisione del Task 7: un `dock` sconosciuto (di una versione futura o rovinato)
    // rende libera quella chat, non illeggibile tutto il file.
    #[test]
    fn unknown_dock_reads_as_free() {
        let file = parse_project_file(r#"{"version":1,"nodes":[
            {"id":1,"kind":"claude","origin":[0,0],"size":[10,10],"title":"a","dock":"lavagna"},
            {"id":2,"kind":"claude","origin":[0,0],"size":[10,10],"title":"b","dock":42},
            {"id":3,"kind":"claude","origin":[0,0],"size":[10,10],"title":"c","dock":null},
            {"id":4,"kind":"claude","origin":[0,0],"size":[10,10],"title":"d","dock":"lavagna","in_kanban":true},
            {"id":5,"kind":"claude","origin":[0,0],"size":[10,10],"title":"e","dock":"automations"}]}"#)
        .unwrap();
        let docks: Vec<_> = file.nodes.iter().map(|n| n.to_frame().dock).collect();
        assert_eq!(docks, vec![None, None, None, Some(DockKind::Kanban), Some(DockKind::Automations)]);
    }

    #[test]
    fn missing_fields_get_defaults() {
        let file = parse_project_file(r#"{ "version": 1 }"#).unwrap();
        assert_eq!(file.viewport.zoom, 1.0);
        assert!(file.nodes.is_empty());
        assert_eq!(file.tab, TabRecord::default());
        let node = parse_project_file(
            r#"{ "version": 1, "nodes": [ { "id": 1, "kind": "shell", "origin": [0,0], "size": [300,200], "title": "shell" } ] }"#,
        )
        .unwrap()
        .nodes
        .remove(0);
        assert_eq!(node.data, serde_json::Value::Null);
    }

    #[test]
    fn unknown_version_and_garbage_are_errors() {
        assert!(parse_project_file(r#"{ "version": 99 }"#).unwrap_err().contains("99"));
        assert!(parse_project_file(r#"{ "version": 0 }"#).is_err());
        assert!(parse_project_file("{ not json").is_err());
        assert!(parse_project_file(r#"{ "version": 1, "nodes": [ { "kind": "rocket" } ] }"#).is_err());
    }

    #[gpui::test]
    async fn load_missing_file(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/repo", json!({})).await;
        assert!(matches!(load_project_file(fs.as_ref(), Path::new("/repo")).await, LoadOutcome::Missing));
    }

    #[gpui::test]
    async fn save_then_load(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/repo", json!({})).await;
        save_project_file(fs.as_ref(), Path::new("/repo"), &sample()).await.unwrap();
        match load_project_file(fs.as_ref(), Path::new("/repo")).await {
            LoadOutcome::Loaded(file) => assert_eq!(file, sample()),
            _ => panic!("atteso Loaded"),
        }
    }

    #[gpui::test]
    async fn corrupt_file_is_moved_to_bak_without_overwriting_older_backups(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/repo",
            json!({ ".superzed": { "project.json": "{ broken", "project.json.bak": "older" } }),
        )
        .await;
        let outcome = load_project_file(fs.as_ref(), Path::new("/repo")).await;
        let LoadOutcome::Recovered { backup, reason } = outcome else {
            panic!("atteso Recovered");
        };
        assert_eq!(backup, Path::new("/repo/.superzed/project.json.bak.1"));
        assert!(!reason.is_empty());
        assert_eq!(fs.load(&backup).await.unwrap(), "{ broken");
        assert_eq!(fs.load(Path::new("/repo/.superzed/project.json.bak")).await.unwrap(), "older");
        assert!(!fs.is_file(Path::new("/repo/.superzed/project.json")).await);
    }

    #[gpui::test]
    async fn future_version_is_moved_to_bak(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/repo", json!({ ".superzed": { "project.json": r#"{ "version": 2 }"# } }))
            .await;
        assert!(matches!(
            load_project_file(fs.as_ref(), Path::new("/repo")).await,
            LoadOutcome::Recovered { .. }
        ));
    }
}
