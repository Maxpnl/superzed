//! Worktree di un repository, letti con `git` attraverso il runner: sporco e distanza dal ramo base.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use superzed_sources::Runner;

#[derive(Clone, Debug, PartialEq)]
pub enum Head {
    Branch(String),
    Detached(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Worktree {
    pub path: PathBuf,
    pub head: Head,
    pub main: bool,
    /// Cartella sparita (`prunable`): nessun altro dato.
    pub missing: bool,
    pub dirty: usize,
    pub ahead: Option<u32>,
    pub behind: Option<u32>,
    pub last_commit: Option<i64>,
}

impl Worktree {
    pub fn merged(&self) -> bool {
        !self.main && !self.missing && self.ahead == Some(0) && self.dirty == 0 && matches!(self.head, Head::Branch(_))
    }
}

const SHORT_HASH_LEN: usize = 7;

pub fn parse_worktree_list(text: &str) -> Vec<Worktree> {
    let mut worktrees = Vec::new();
    for (index, block) in text.split("\n\n").enumerate() {
        let mut path = None;
        let mut hash = String::new();
        let mut branch = None;
        let mut bare = false;
        let mut missing = false;
        for line in block.lines() {
            if let Some(value) = line.strip_prefix("worktree ") {
                path = Some(PathBuf::from(value));
            } else if let Some(value) = line.strip_prefix("HEAD ") {
                hash = value.chars().take(SHORT_HASH_LEN).collect();
            } else if let Some(value) = line.strip_prefix("branch ") {
                branch = Some(value.strip_prefix("refs/heads/").unwrap_or(value).to_string());
            } else if line == "bare" {
                bare = true;
            } else if line == "prunable" || line.starts_with("prunable ") {
                missing = true;
            }
        }
        let Some(path) = path else { continue };
        if bare {
            continue;
        }
        worktrees.push(Worktree {
            path,
            head: branch.map_or(Head::Detached(hash), Head::Branch),
            main: index == 0,
            missing,
            dirty: 0,
            ahead: None,
            behind: None,
            last_commit: None,
        });
    }
    worktrees
}

/// `rev-list --left-right --count base...HEAD` stampa "<solo nel base>\t<solo in HEAD>".
pub fn parse_left_right(text: &str) -> Option<(u32, u32)> {
    let mut fields = text.split_whitespace();
    let behind = fields.next()?.parse().ok()?;
    let ahead = fields.next()?.parse().ok()?;
    fields.next().is_none().then_some((behind, ahead))
}

fn git_arguments(directory: &Path, args: &[&str]) -> Vec<String> {
    let mut full_args = vec!["-C".to_string(), directory.to_string_lossy().into_owned()];
    full_args.extend(args.iter().map(|arg| arg.to_string()));
    full_args
}

/// Stdout di `git -C <cartella> <args>`, o `None` se il comando non parte o fallisce.
async fn git_output(runner: &Arc<dyn Runner>, directory: &Path, args: &[&str]) -> Option<String> {
    match runner.run("git", &git_arguments(directory, args), None).await {
        Ok(output) if output.success => Some(output.stdout),
        _ => None,
    }
}

pub async fn read_worktrees(runner: &Arc<dyn Runner>, root: &Path) -> Result<Vec<Worktree>, String> {
    let listing = match runner.run("git", &git_arguments(root, &["worktree", "list", "--porcelain"]), None).await {
        Ok(output) if output.success => output.stdout,
        Ok(output) => return Err(format!("git worktree list: {}", output.stderr.trim())),
        Err(error) => return Err(format!("git worktree list: {error}")),
    };
    let mut worktrees = parse_worktree_list(&listing);

    let base = git_output(runner, root, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .await
        .map(|stdout| stdout.trim().to_string())
        .filter(|base| !base.is_empty());

    for worktree in worktrees.iter_mut().filter(|worktree| !worktree.missing) {
        let directory = worktree.path.clone();
        if let Some(status) = git_output(runner, &directory, &["status", "--porcelain"]).await {
            worktree.dirty = status.lines().filter(|line| !line.trim().is_empty()).count();
        }
        if let Some(base) = &base {
            let range = format!("{base}...HEAD");
            let counts = git_output(runner, &directory, &["rev-list", "--left-right", "--count", &range]).await;
            if let Some((behind, ahead)) = counts.as_deref().and_then(parse_left_right) {
                worktree.behind = Some(behind);
                worktree.ahead = Some(ahead);
            }
        }
        worktree.last_commit =
            git_output(runner, &directory, &["log", "-1", "--format=%ct"]).await.and_then(|stdout| stdout.trim().parse().ok());
        if matches!(worktree.head, Head::Detached(_))
            && let Some(tag) = git_output(runner, &directory, &["describe", "--tags", "--exact-match"]).await
        {
            let tag = tag.trim();
            if !tag.is_empty() {
                worktree.head = Head::Detached(tag.to_string());
            }
        }
    }
    Ok(worktrees)
}

#[cfg(test)]
mod tests {
    use super::*;
    use superzed_sources::test_support::{FakeRunner, fail, ok};

    fn runner(r: &Arc<FakeRunner>) -> Arc<dyn Runner> {
        r.clone()
    }

    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        futures::executor::block_on(future)
    }

    const LIST: &str = "worktree /repo\nHEAD 1111111111111111111111111111111111111111\nbranch refs/heads/main\n\nworktree /repo.worktrees/bills\nHEAD 2222222222222222222222222222222222222222\nbranch refs/heads/feat/bills-at-source\n\nworktree /home/alice/gpm\nHEAD 3333333333333333333333333333333333333333\ndetached\n\nworktree /repo.worktrees/gone\nHEAD 4444444444444444444444444444444444444444\nbranch refs/heads/feat/gone\nprunable gitdir file points to non-existent location\n\nworktree /srv/bare.git\nbare\n";

    #[test]
    fn the_porcelain_list_is_parsed() {
        let w = parse_worktree_list(LIST);
        assert_eq!(w.len(), 4, "il bare non conta");
        assert_eq!((w[0].main, &w[0].head), (true, &Head::Branch("main".into())));
        assert_eq!(w[1].head, Head::Branch("feat/bills-at-source".into()));
        assert_eq!(w[2].head, Head::Detached("3333333".into()));
        assert!(w[3].missing && !w[1].missing);
        assert_eq!(parse_left_right("41\t0\n"), Some((41, 0)));
        assert_eq!(parse_left_right("x"), None);
    }

    #[test]
    fn every_worktree_gets_its_numbers_and_a_broken_one_does_not_stop_the_rest() {
        let r = FakeRunner::new();
        r.on("worktree list --porcelain", ok(LIST));
        r.on("symbolic-ref --short refs/remotes/origin/HEAD", ok("origin/main\n"));
        r.on("-C /repo.worktrees/bills status --porcelain", ok(""));
        r.on("-C /repo.worktrees/bills rev-list --left-right --count origin/main...HEAD", ok("41\t0\n"));
        r.on("-C /repo.worktrees/bills log -1 --format=%ct", ok("1795400000\n"));
        r.on("-C /home/alice/gpm status --porcelain", fail(128, "fatal: not a git repository"));
        r.on("-C /home/alice/gpm describe --tags --exact-match", ok("1.0.96\n"));
        r.on("-C /repo status --porcelain", ok(" M a.rs\n?? b.rs\n"));
        let w = block_on(read_worktrees(&runner(&r), Path::new("/repo"))).unwrap();
        assert_eq!((w[0].dirty, w[0].merged()), (2, false));
        assert_eq!((w[1].ahead, w[1].behind, w[1].last_commit, w[1].merged()), (Some(0), Some(41), Some(1795400000), true));
        assert_eq!((w[2].head.clone(), w[2].dirty, w[2].ahead, w[2].merged()), (Head::Detached("1.0.96".into()), 0, None, false));
        assert_eq!(r.count("-C /repo.worktrees/gone"), 0, "cartella sparita: nessun comando");
    }

    #[test]
    fn a_failed_describe_keeps_the_short_hash() {
        let r = FakeRunner::new();
        r.on("worktree list --porcelain", ok(LIST));
        let w = block_on(read_worktrees(&runner(&r), Path::new("/repo"))).unwrap();
        assert_eq!(w[2].head, Head::Detached("3333333".into()));
    }

    #[test]
    fn without_origin_head_there_is_no_ahead_or_behind_and_no_list_is_an_error() {
        let r = FakeRunner::new();
        r.on("worktree list --porcelain", ok(LIST));
        r.on("status --porcelain", ok(""));
        let w = block_on(read_worktrees(&runner(&r), Path::new("/repo"))).unwrap();
        assert!(w.iter().all(|worktree| worktree.ahead.is_none() && worktree.behind.is_none()));
        assert_eq!(r.count("rev-list"), 0);

        let r = FakeRunner::new();
        r.on("worktree list --porcelain", fail(128, "fatal: not a git repository\n"));
        let error = block_on(read_worktrees(&runner(&r), Path::new("/repo"))).unwrap_err();
        assert_eq!(error, "git worktree list: fatal: not a git repository");
    }
}
