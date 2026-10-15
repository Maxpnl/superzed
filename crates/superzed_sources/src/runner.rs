//! Esecuzione di comandi esterni (`gh`, `ssh`) dietro un trait: nei test `FakeRunner`.
use futures::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _, future::BoxFuture};
use std::time::Duration;
use util::command::{Stdio, new_command};

/// Tempo massimo di un comando (`gh`, `ssh`): poi il processo si uccide.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
/// Limite di stdout e stderr catturati, ciascuno.
pub const MAX_OUTPUT: usize = 16 * 1024 * 1024;

/// Cartelle messe davanti al `PATH`: un'app lanciata dal Finder ha solo quello di sistema.
pub const EXTRA_PATH: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin"];
const DEFAULT_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

#[derive(Clone, Debug, PartialEq)]
pub struct RunOutput {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RunError {
    /// Programma non trovato nel `PATH`.
    NotFound(String),
    Failed(String),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::NotFound(p) => write!(f, "“{p}” not found"),
            RunError::Failed(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for RunError {}

pub trait Runner: Send + Sync + 'static {
    fn run(&self, program: &str, args: &[String], stdin: Option<String>) -> BoxFuture<'static, Result<RunOutput, RunError>>;
}

pub fn augmented_path(current: Option<&str>) -> String {
    let current = current.filter(|p| !p.is_empty()).unwrap_or(DEFAULT_PATH);
    let mut parts: Vec<&str> = EXTRA_PATH.to_vec();
    for p in current.split(':') {
        if !p.is_empty() && !parts.contains(&p) {
            parts.push(p);
        }
    }
    parts.join(":")
}

pub struct ProcessRunner;

impl Runner for ProcessRunner {
    fn run(&self, program: &str, args: &[String], stdin: Option<String>) -> BoxFuture<'static, Result<RunOutput, RunError>> {
        run_process(program, args, stdin, DEFAULT_TIMEOUT)
    }
}

/// Esegue un processo locale: stdin scritto mentre si leggono stdout e stderr (niente
/// blocco con pipe piene), processo ucciso allo scadere di `timeout`, output entro `MAX_OUTPUT`.
pub fn run_process(
    program: &str,
    args: &[String],
    stdin: Option<String>,
    timeout: Duration,
) -> BoxFuture<'static, Result<RunOutput, RunError>> {
    run_limited(program, args, stdin, timeout, MAX_OUTPUT)
}

fn run_limited(
    program: &str,
    args: &[String],
    stdin: Option<String>,
    timeout: Duration,
    max_output: usize,
) -> BoxFuture<'static, Result<RunOutput, RunError>> {
    let mut cmd = new_command(program);
    cmd.args(args)
        .env("PATH", augmented_path(std::env::var("PATH").ok().as_deref()))
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let spawned = cmd.spawn();
    let program = program.to_string();
    Box::pin(async move {
        let mut child = match spawned {
            Ok(child) => child,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(RunError::NotFound(program)),
            Err(e) => return Err(RunError::Failed(format!("{program}: {e}"))),
        };
        let input = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let status = child.status();
        let write = async move {
            if let (Some(text), Some(mut input)) = (stdin, input) {
                match input.write_all(text.as_bytes()).await {
                    // Il processo ha chiuso stdin senza leggere tutto: conta il suo esito.
                    Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => {
                        return Err(format!("writing to stdin: {e}"));
                    }
                    _ => {}
                }
                input.close().await.ok();
            }
            Ok(())
        };
        let work = async move {
            let ((), out, err) =
                futures::future::try_join3(write, read_capped(stdout, max_output), read_capped(stderr, max_output)).await?;
            let status = status.await.map_err(|e| e.to_string())?;
            Ok((status, out, err))
        };
        // Processo vero, gia' non deterministico; il `Runner` non ha un executor di gpui
        // (nei test deterministici si usa `FakeRunner`).
        #[allow(clippy::disallowed_methods, reason = "timeout di un processo reale, fuori dall'executor di gpui")]
        let deadline = async move {
            smol::Timer::after(timeout).await;
            Err("command timed out".to_string())
        };
        match smol::future::or(work, deadline).await {
            Ok((status, out, err)) => Ok(RunOutput {
                success: status.success(),
                code: status.code(),
                stdout: String::from_utf8_lossy(&out).into_owned(),
                stderr: String::from_utf8_lossy(&err).into_owned(),
            }),
            Err(e) => {
                child.kill().ok();
                Err(RunError::Failed(format!("{program}: {e}")))
            }
        }
    })
}

async fn read_capped(pipe: Option<impl AsyncRead + Unpin>, max: usize) -> Result<Vec<u8>, String> {
    let mut data = Vec::new();
    if let Some(pipe) = pipe {
        pipe.take(max as u64 + 1).read_to_end(&mut data).await.map_err(|e| format!("reading the output: {e}"))?;
    }
    if data.len() > max {
        return Err("output too large".into());
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_gets_homebrew_first_without_duplicates() {
        assert_eq!(augmented_path(Some("/usr/bin:/bin")), "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin");
        assert_eq!(augmented_path(Some("/usr/local/bin:/usr/bin")), "/opt/homebrew/bin:/usr/local/bin:/usr/bin");
        assert_eq!(augmented_path(None), "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin");
    }

    #[test]
    fn process_runner_passes_stdin_and_reports_failures() {
        let r = ProcessRunner;
        let out = futures::executor::block_on(r.run("/bin/cat", &[], Some("SELECT 1".into()))).unwrap();
        assert_eq!((out.success, out.stdout.as_str()), (true, "SELECT 1"));
        let args = vec!["-c".to_string(), "echo oops >&2; exit 3".to_string()];
        let out = futures::executor::block_on(r.run("/bin/sh", &args, None)).unwrap();
        assert_eq!((out.success, out.code, out.stderr.trim()), (false, Some(3), "oops"));
        assert_eq!(
            futures::executor::block_on(r.run("superzed-nessun-programma", &[], None)),
            Err(RunError::NotFound("superzed-nessun-programma".into()))
        );
    }

    #[test]
    fn large_stdin_and_stdout_do_not_deadlock() {
        let input: String = (0..(1 << 20)).map(|i| (b'a' + (i % 26) as u8) as char).collect();
        let out = futures::executor::block_on(run_process("/bin/cat", &[], Some(input.clone()), DEFAULT_TIMEOUT)).unwrap();
        assert!(out.success);
        assert_eq!(out.stdout.len(), input.len());
        assert!(out.stdout == input);
    }

    #[test]
    fn slow_commands_time_out() {
        let args = vec!["5".to_string()];
        let started = std::time::Instant::now();
        let result = futures::executor::block_on(run_process("/bin/sleep", &args, None, Duration::from_millis(200)));
        assert_eq!(result, Err(RunError::Failed("/bin/sleep: command timed out".into())));
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn oversized_output_is_refused() {
        let input = "x".repeat(1000);
        let result = futures::executor::block_on(run_limited("/bin/cat", &[], Some(input), DEFAULT_TIMEOUT, 100));
        assert_eq!(result, Err(RunError::Failed("/bin/cat: output too large".into())));
        let out = futures::executor::block_on(run_limited("/bin/cat", &[], Some("x".repeat(100)), DEFAULT_TIMEOUT, 100)).unwrap();
        assert_eq!(out.stdout.len(), 100, "esattamente al limite va bene");
    }
}
