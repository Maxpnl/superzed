//! Repo GitHub del progetto, letto dal remote `origin` della config di git.
use std::path::{Path, PathBuf};
use superzed_sources::github::PmConfig;

/// `owner/nome` da un URL di remote GitHub (scp-like, https o ssh); altri host o forme: `None`.
pub fn parse_github_repo(url: &str) -> Option<String> {
    let url = url.trim();
    let path = match url.strip_prefix("git@github.com:") {
        Some(path) => path,
        None => {
            let (_, rest) = url.split_once("://")?;
            let (authority, path) = rest.split_once('/')?;
            let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
            if host != "github.com" {
                return None;
            }
            path
        }
    };
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.split_once('/')?;
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return None;
    }
    Some(format!("{owner}/{name}"))
}

/// Cartella con il file `config` di git: in un worktree collegato `.git` e' un file
/// `gitdir: <admin>` e la config vive nella cartella indicata da `<admin>/commondir`.
fn common_git_dir(root: &Path) -> Result<PathBuf, String> {
    let dot_git = root.join(".git");
    if dot_git.is_dir() {
        return Ok(dot_git);
    }
    let pointer = std::fs::read_to_string(&dot_git).map_err(|_| "no GitHub remote: not a git repository".to_string())?;
    let admin = pointer
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))
        .map(|path| root.join(path.trim()))
        .ok_or_else(|| "no GitHub remote: not a git repository".to_string())?;
    match std::fs::read_to_string(admin.join("commondir")) {
        Ok(common) => Ok(admin.join(common.trim())),
        Err(_) => Ok(admin),
    }
}

/// `owner/nome` del remote `origin` di `root`; l'errore e' da mostrare nella card.
pub fn repo_of(root: &Path) -> Result<String, String> {
    let config_path = common_git_dir(root)?.join("config");
    let config = std::fs::read_to_string(&config_path).map_err(|e| format!("no GitHub remote: {}: {e}", config_path.display()))?;
    let mut in_origin = false;
    let mut origin_url = None;
    for line in config.lines().map(str::trim) {
        if line.starts_with('[') {
            in_origin = line == "[remote \"origin\"]";
        } else if in_origin
            && origin_url.is_none()
            && let Some((key, value)) = line.split_once('=')
            && key.trim() == "url"
        {
            origin_url = Some(value.trim().to_string());
        }
    }
    let url = origin_url.ok_or_else(|| "no GitHub remote: origin is not configured".to_string())?;
    parse_github_repo(&url).ok_or_else(|| format!("no GitHub remote: origin is {url}"))
}

/// Config per `gh`: owner e repo dall'origin; senza Project (numero 0) e senza soglia di stale.
pub fn config_for(root: &Path) -> Result<PmConfig, String> {
    let repo = repo_of(root)?;
    let owner = repo.split_once('/').map(|(owner, _)| owner.to_string()).unwrap_or_default();
    Ok(PmConfig { owner, project_number: 0, repo, stale_days: 0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_urls_become_owner_slash_name() {
        for url in [
            "git@github.com:acme/acme_app.git",
            "https://github.com/acme/acme_app",
            "https://github.com/acme/acme_app.git",
            "ssh://git@github.com/acme/acme_app.git",
            "https://alice@github.com/acme/acme_app/",
        ] {
            assert_eq!(parse_github_repo(url).as_deref(), Some("acme/acme_app"), "{url}");
        }
        for url in ["git@gitlab.com:acme/acme_app.git", "https://github.com/acme", "/srv/git/acme_app.git", ""] {
            assert_eq!(parse_github_repo(url), None, "{url}");
        }
    }

    #[test]
    fn repo_of_reads_origin_from_git_config() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(repo_of(dir.path()).unwrap_err(), "no GitHub remote: not a git repository");
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(
            dir.path().join(".git/config"),
            "[remote \"upstream\"]\n\turl = git@github.com:other/x.git\n[remote \"origin\"]\n\turl = git@github.com:acme/acme_app.git\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n",
        )
        .unwrap();
        assert_eq!(repo_of(dir.path()).unwrap(), "acme/acme_app");
        std::fs::write(dir.path().join(".git/config"), "[remote \"origin\"]\n\turl = git@gitlab.com:acme/acme_app.git\n").unwrap();
        assert_eq!(repo_of(dir.path()).unwrap_err(), "no GitHub remote: origin is git@gitlab.com:acme/acme_app.git");
        std::fs::write(dir.path().join(".git/config"), "[core]\n\tbare = false\n").unwrap();
        assert_eq!(repo_of(dir.path()).unwrap_err(), "no GitHub remote: origin is not configured");
    }

    #[test]
    fn repo_of_follows_a_linked_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main");
        let worktree = dir.path().join("wt");
        let admin = main.join(".git/worktrees/wt");
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(main.join(".git/config"), "[remote \"origin\"]\n\turl = git@github.com:acme/acme_app.git\n").unwrap();
        std::fs::write(admin.join("commondir"), "../..\n").unwrap();
        std::fs::write(worktree.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
        assert_eq!(repo_of(&worktree).unwrap(), "acme/acme_app");
    }

    #[test]
    fn config_for_splits_owner_and_repo() {
        let dir = tempfile::tempdir().unwrap();
        crate::test_support::write_git_origin(dir.path());
        let config = config_for(dir.path()).unwrap();
        assert_eq!((config.owner.as_str(), config.repo.as_str()), ("acme", "acme/acme_app"));
    }
}
