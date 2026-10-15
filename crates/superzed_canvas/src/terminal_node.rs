use crate::node::NodeContent;
use crate::process;
use gpui::{AnyView, App, Entity, FocusHandle, Focusable as _, Pixels, Size, px};
use std::path::PathBuf;
use superzed_sessions::launch::{ClaudeLaunch, shell_quote, unset_inherited};
use task::SpawnInTerminal;
use terminal::Terminal;
use terminal_view::TerminalView;

pub fn login_shell_from(shell_env: Option<String>) -> String {
    match shell_env {
        Some(s) if !s.is_empty() => s,
        _ => "/bin/zsh".to_string(),
    }
}

pub fn login_shell() -> String {
    login_shell_from(std::env::var("SHELL").ok())
}

fn spawn_spec(shell: &str, script: String, label: &str, cwd: Option<PathBuf>) -> SpawnInTerminal {
    SpawnInTerminal {
        label: label.to_string(),
        full_label: label.to_string(),
        command_label: label.to_string(),
        command: Some(shell.to_string()),
        args: vec!["-l".to_string(), "-c".to_string(), script],
        cwd,
        use_new_terminal: true,
        allow_concurrent_runs: true,
        // i terminali dei nodi sono "task": senza Always non emetterebbero mai
        // CloseTerminal a processo finito
        hide: task::HideStrategy::Always,
        ..Default::default()
    }
}

/// Nodo terminale: shell di login pulita dagli account ereditati e da nodeterm.
pub fn shell_node_spec(shell: &str, cwd: Option<PathBuf>) -> SpawnInTerminal {
    let script = format!("{}; exec {} -l", unset_inherited(), shell_quote(shell));
    spawn_spec(shell, script, "shell", cwd)
}

/// Nodo claude: lancia `launch.command` e, quando claude esce (anche con ctrl-c),
/// resta una shell di login. `trap : INT`: ctrl-c arriva a tutto il process group e
/// senza trap la shell `-c` morirebbe insieme a claude; un trap con handler viene
/// resettato nei figli, quindi claude riceve ctrl-c normalmente.
pub fn claude_node_spec(shell: &str, cwd: Option<PathBuf>, launch: &ClaudeLaunch) -> SpawnInTerminal {
    // la cartella di superzed-notify davanti al PATH (claude e i suoi strumenti la vedono)
    let path = launch
        .path_prefix
        .as_ref()
        .map(|p| format!("export PATH={}:\"$PATH\"; ", shell_quote(&p.to_string_lossy())))
        .unwrap_or_default();
    // account non di sistema: claude legge credenziali e transcript da quella config dir
    let account = launch
        .config_dir
        .as_ref()
        .map(|d| format!("export CLAUDE_CONFIG_DIR={}; ", shell_quote(&d.to_string_lossy())))
        .unwrap_or_default();
    let script = format!("{}; {account}{path}trap : INT; {}; exec {} -l", unset_inherited(), launch.command, shell_quote(shell));
    let mut spec = spawn_spec(shell, script, "claude", cwd);
    spec.env.extend(launch.env.iter().cloned());
    spec
}

/// Variabili esportate nello script, non nell'ambiente del processo: la shell di login
/// rilegge il profilo dell'utente, che potrebbe ridefinirle.
pub fn prepend_exports(spec: &mut SpawnInTerminal, exports: &[(String, String)]) {
    if exports.is_empty() {
        return;
    }
    let prefix: String = exports.iter().map(|(name, value)| format!("export {name}={}; ", shell_quote(value))).collect();
    if let Some(script) = spec.args.get_mut(2) {
        script.insert_str(0, &prefix);
    }
}

/// Titolo che claude da' al terminale (OSC), senza il simbolo di stato davanti
/// (`✳` a riposo, la rotellina braille mentre lavora). `None` se non resta niente.
pub fn claude_title(osc: &str) -> Option<String> {
    let is_glyph = |c: char| c.is_whitespace() || ('\u{2800}'..='\u{28FF}').contains(&c) || "✳✻✶✢✽·*".contains(c);
    let title = osc.trim_start_matches(is_glyph).trim_end();
    (!title.is_empty()).then(|| title.to_string())
}

/// Zoom del font passato al terminale: a zoom 1 nessuno (si comporta come upstream).
fn font_zoom(zoom: f32) -> Option<f32> {
    ((zoom - 1.0).abs() > 1e-4).then_some(zoom)
}

/// A questo zoom del canvas il terminale non disegna il testo: il nodo mostra la sua
/// vista d'insieme (stessa condizione di `TerminalView::superzed_text_visible`).
pub fn overview_at(zoom: f32, cx: &App) -> bool {
    font_zoom(zoom).is_some_and(|z| !TerminalView::superzed_text_visible(z, cx))
}

/// Altezza della vista d'insieme in rem del suo testo: padding (2 x 0,75), titolo Large
/// su 2 righe (2 x 1,62), icona di stato (1), riga di meta Default (1,42), riga delle
/// date Small (1,25) e i tre spazi fra le righe (3 x 0,5): 9,9, arrotondato. A rem 16
/// sono 160 px.
pub const OVERVIEW_REF_H_REMS: f32 = 10.;
/// Larghezza minima della vista d'insieme in rem: padding e la riga delle date, due
/// etichette Small con icona ai lati («yesterday 09:10»). A rem 16 sono 208 px.
pub const OVERVIEW_REF_W_REMS: f32 = 13.;
/// Sotto questo rem il testo della vista d'insieme non si legge: niente testo, solo
/// sfondo, bordo e l'icona di stato. Lo zoom out si ferma prima (`viewport::min_zoom_for`).
pub const OVERVIEW_MIN_REM: Pixels = px(OVERVIEW_MIN_REM_PX);
/// `OVERVIEW_MIN_REM` in f32, per i calcoli costanti (`viewport::MAX_DYNAMIC_MIN_ZOOM`).
pub const OVERVIEW_MIN_REM_PX: f32 = 8.;

/// Rem della vista d'insieme per un nodo grande `node_screen_size` sullo schermo
/// (dimensione nel canvas per zoom): il rem dell'interfaccia se il contenuto previsto
/// (`OVERVIEW_REF_H_REMS` x `OVERVIEW_REF_W_REMS`) ci sta, altrimenti ridotto finche' ci
/// sta; `None` se ridotto sotto `OVERVIEW_MIN_REM`.
pub fn overview_rem(node_screen_size: Size<Pixels>, rem_ui: Pixels) -> Option<Pixels> {
    let fit = rem_ui
        .min(node_screen_size.height / OVERVIEW_REF_H_REMS)
        .min(node_screen_size.width / OVERVIEW_REF_W_REMS);
    (fit >= OVERVIEW_MIN_REM).then_some(fit)
}

pub struct TerminalNode {
    pub(crate) view: Entity<TerminalView>,
    pub(crate) terminal: Entity<Terminal>,
    /// Chiave della sessione claude di questo nodo, se ce n'e' una (Task 6, 2a).
    pub(crate) session_key: Option<String>,
}

impl NodeContent for TerminalNode {
    fn view(&self) -> AnyView {
        self.view.clone().into()
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.view.focus_handle(cx)
    }

    fn set_font_zoom(&self, zoom: f32, cx: &mut App) {
        let zoom = font_zoom(zoom);
        self.view.update(cx, |v, cx| v.superzed_set_font_zoom(zoom, cx));
    }

    fn on_close(&self, cx: &mut App) {
        process::terminate_terminal(self.terminal.read(cx), cx);
    }

    fn save_data(&self, cx: &App) -> serde_json::Value {
        self.session_key
            .as_deref()
            .and_then(|key| Some(superzed_sessions::SessionStore::global(cx)?.read(cx).get(key)?.save_data()))
            .unwrap_or(serde_json::Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn overview_rem_fits_the_node_on_screen() {
        assert_eq!(OVERVIEW_MIN_REM, px(8.));
        let rem = px(16.);
        // nodo grande: il contenuto a rem 16 (160 x 208 px) ci sta, rem pieno
        assert_eq!(overview_rem(gpui::size(px(360.), px(252.)), rem), Some(rem));
        assert_eq!(overview_rem(gpui::size(px(208.), px(160.)), rem), Some(rem), "al limite");
        // nodo medio (chat 800 x 560 a zoom 0,2): limitato dall'altezza
        let r = overview_rem(gpui::size(px(160.), px(112.)), rem).expect("rem ridotto");
        assert!((f32::from(r) - 112. / OVERVIEW_REF_H_REMS).abs() < 1e-4, "{r:?}");
        assert!(r * OVERVIEW_REF_H_REMS <= px(112.) + px(1e-3));
        // stretto e alto: limitato dalla larghezza
        let r = overview_rem(gpui::size(px(104.), px(400.)), rem).expect("rem ridotto");
        assert!((f32::from(r) - 8.).abs() < 1e-4, "{r:?}");
        // rem dell'interfaccia piu' grande: stesso nodo, il contenuto previsto cresce
        assert_eq!(overview_rem(gpui::size(px(360.), px(252.)), px(24.)), Some(px(24.)));
        assert_eq!(overview_rem(gpui::size(px(360.), px(252.)), px(32.)), Some(px(252. / OVERVIEW_REF_H_REMS)));
        // nodo minuscolo: testo illeggibile, nessuna vista d'insieme con testo
        assert_eq!(overview_rem(gpui::size(px(40.), px(28.)), rem), None);
        assert_eq!(overview_rem(gpui::size(px(400.), px(40.)), rem), None, "sotto 8 px di rem");
        assert_eq!(overview_rem(gpui::size(px(400.), px(7.5 * OVERVIEW_REF_H_REMS)), rem), None, "7,5 px: illeggibile");
        assert!(overview_rem(gpui::size(px(8. * OVERVIEW_REF_W_REMS), px(8. * OVERVIEW_REF_H_REMS)), rem).is_some(), "8 px esatti");
    }

    #[test]
    fn claude_title_drops_the_status_glyph() {
        assert_eq!(claude_title("✳ Fix login bug").as_deref(), Some("Fix login bug"));
        assert_eq!(claude_title("⠂ Refactor canvas").as_deref(), Some("Refactor canvas"));
        assert_eq!(claude_title("Claude Code").as_deref(), Some("Claude Code"));
        assert_eq!(claude_title("  ").as_deref(), None);
        assert_eq!(claude_title("").as_deref(), None);
        assert_eq!(claude_title("✳").as_deref(), None);
    }

    #[test]
    fn login_shell_falls_back_to_zsh() {
        assert_eq!(login_shell_from(Some("/opt/homebrew/bin/fish".into())), "/opt/homebrew/bin/fish");
        assert_eq!(login_shell_from(Some(String::new())), "/bin/zsh");
        assert_eq!(login_shell_from(None), "/bin/zsh");
    }

    #[test]
    fn exports_go_before_the_script() {
        let mut spec = claude_node_spec("/bin/zsh", None, &launch());
        let before = spec.args[2].clone();
        prepend_exports(&mut spec, &[("GOG_GMAIL_NO_SEND".into(), "1".into())]);
        assert_eq!(spec.args[2], format!("export GOG_GMAIL_NO_SEND='1'; {before}"));
        let unchanged = spec.args[2].clone();
        prepend_exports(&mut spec, &[]);
        assert_eq!(spec.args[2], unchanged);
    }

    fn launch() -> superzed_sessions::launch::ClaudeLaunch {
        superzed_sessions::launch::ClaudeLaunch {
            command: "claude --settings '/d/settings.json' --session-id 'abc'".into(),
            env: vec![("SUPERZED_NODE".into(), "k".into())],
            session_id: "abc".into(),
            path_prefix: None,
            config_dir: None,
        }
    }

    // Review Focus 2: config dir con spazi e apostrofi, esportato dopo l'unset e prima di claude
    #[test]
    fn account_config_dir_is_exported_quoted_before_claude() {
        let mut l = launch();
        l.config_dir = Some(PathBuf::from("/Users/u/Library/Application Support/it's/acc"));
        let script = claude_node_spec("/bin/zsh", None, &l).args[2].clone();
        let export = "export CLAUDE_CONFIG_DIR='/Users/u/Library/Application Support/it'\\''s/acc'; ";
        let at = script.find(export).expect(&script);
        assert!(at > script.find("unset CLAUDE_CONFIG_DIR").unwrap(), "dopo l'unset");
        assert!(at < script.find("claude --settings").unwrap(), "prima di claude");
        assert!(!claude_node_spec("/bin/zsh", None, &launch()).args[2].contains("export CLAUDE_CONFIG_DIR"), "sistema: nessun export");
    }

    #[test]
    fn claude_node_exports_the_notify_bin_on_path() {
        let mut l = launch();
        l.path_prefix = Some(PathBuf::from("/d/claude/bin"));
        let spec = claude_node_spec("/bin/zsh", None, &l);
        assert!(spec.args[2].contains("; export PATH='/d/claude/bin':\"$PATH\"; trap : INT; claude "), "{}", spec.args[2]);
    }

    #[test]
    fn shell_node_is_a_login_shell_without_inherited_env() {
        let spec = shell_node_spec("/bin/zsh", Some(PathBuf::from("/repo")));
        assert_eq!(spec.args[0..2], ["-l", "-c"]);
        assert_eq!(spec.args[2], format!("{}; exec '/bin/zsh' -l", superzed_sessions::launch::unset_inherited()));
        assert!(spec.args[2].contains("NODETERM_NODE_ID"));
        assert_eq!(spec.label, "shell");
        assert!(spec.env.is_empty());
        assert_eq!(spec.hide, task::HideStrategy::Always);
    }

    #[test]
    fn claude_node_runs_the_launch_command_and_keeps_a_shell() {
        let spec = claude_node_spec("/bin/zsh", None, &launch());
        assert_eq!(
            spec.args[2],
            format!(
                "{}; trap : INT; claude --settings '/d/settings.json' --session-id 'abc'; exec '/bin/zsh' -l",
                superzed_sessions::launch::unset_inherited()
            )
        );
        assert_eq!(spec.env.get("SUPERZED_NODE").map(String::as_str), Some("k"));
        assert_eq!(spec.label, "claude");
    }

    #[test]
    fn shell_paths_with_quotes_are_escaped() {
        let spec = claude_node_spec("/it's/sh", None, &launch());
        assert!(spec.args[2].ends_with("exec '/it'\\''s/sh' -l"));
    }
}
