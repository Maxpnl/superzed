//! Config claude-pm del progetto: owner, Project e repo (da `superzed_sources`) piu' le aree.
use std::path::Path;
use superzed_sources::github::{PM_CONFIG, PmConfig, parse_pm_config};

/// Chiavi di primo livello sotto `areas:` nel frontmatter (mappa, lista o `[a, b]` in linea).
pub fn parse_areas(text: &str) -> Vec<String> {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return Vec::new();
    }
    let unquote = |s: &str| s.trim().trim_matches(['"', '\'']).to_string();
    let mut areas: Vec<String> = Vec::new();
    let mut inside = false;
    let mut level: Option<usize> = None;
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if indent == 0 {
            inside = false;
            if let Some((k, v)) = line.split_once(':')
                && k.trim() == "areas"
            {
                let v = v.trim();
                if v.starts_with('[') {
                    areas.extend(v.trim_matches(['[', ']']).split(',').map(unquote).filter(|s| !s.is_empty()));
                } else {
                    inside = v.is_empty();
                    level = None;
                }
            }
            continue;
        }
        if !inside {
            continue;
        }
        // solo il primo livello sotto `areas:` (i pattern annidati non sono aree)
        if indent != *level.get_or_insert(indent) {
            continue;
        }
        let name = match t.strip_prefix("- ") {
            Some(item) => unquote(item),
            None => t.split_once(':').map(|(k, _)| unquote(k)).unwrap_or_default(),
        };
        if !name.is_empty() && !areas.contains(&name) {
            areas.push(name);
        }
    }
    areas
}

/// `<root>/.claude/claude-pm.local.md`: config e aree. File assente → errore da mostrare
/// nella card (la pipeline si ferma).
pub fn load_pm(root: &Path) -> Result<(PmConfig, Vec<String>), String> {
    let path = root.join(PM_CONFIG);
    let text = std::fs::read_to_string(&path).map_err(|_| format!("claude-pm configuration missing: {}", path.display()))?;
    Ok((parse_pm_config(&text)?, parse_areas(&text)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{PM_TEXT, write_pm};

    #[test]
    fn areas_from_the_frontmatter() {
        assert_eq!(parse_areas(PM_TEXT), ["frontend", "backend"]);
        let quoted = "---\nareas:\n  \"provisioning\": []\n  # commento\n  frontend:\n    - x\nrepo: a/b\n---\n";
        assert_eq!(parse_areas(quoted), ["provisioning", "frontend"]);
        assert_eq!(parse_areas("---\nareas:\n  - a\n  - 'b'\n---\n"), ["a", "b"]);
        assert_eq!(parse_areas("---\nareas: [a, \"b\"]\n---\n"), ["a", "b"]);
        assert!(parse_areas("niente frontmatter\nareas:\n  a: 1\n").is_empty());
        assert!(parse_areas("---\nrepo: a/b\n---\nareas:\n  a: 1\n").is_empty(), "solo dentro il frontmatter");
    }

    #[test]
    fn load_pm_reads_config_and_areas() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_pm(dir.path()).unwrap_err().starts_with("claude-pm configuration missing: "));
        write_pm(dir.path());
        let (cfg, areas) = load_pm(dir.path()).unwrap();
        assert_eq!((cfg.owner.as_str(), cfg.project_number, cfg.repo.as_str()), ("acme", 3, "acme/acme_app"));
        assert_eq!(areas, ["frontend", "backend"]);
        std::fs::write(dir.path().join(PM_CONFIG), "---\nrepo: a/b\n---\n").unwrap();
        assert!(load_pm(dir.path()).unwrap_err().contains("project_owner"));
    }
}
