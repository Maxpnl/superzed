// Terminazione dei processi di un nodo terminale: la shell del PTY e' leader di
// sessione e di process group (setsid in alacritty), quindi pgid = pid della shell.
// `claude` lanciato da `sh -c` resta in quel gruppo; i job di una shell interattiva
// hanno gruppi propri, raggiungibili tramite il gruppo in primo piano (tcgetpgrp).
use gpui::App;
use std::time::Duration;
use terminal::Terminal;

pub(crate) const KILL_GRACE: Duration = Duration::from_millis(500);

/// Toglie pgid non validi, init, il nostro gruppo e i duplicati.
pub(crate) fn filter_groups(pgids: &[i32]) -> Vec<i32> {
    // SAFETY: getpgrp non ha precondizioni
    let own = unsafe { libc::getpgrp() };
    let mut out = Vec::new();
    for &pg in pgids {
        if pg > 1 && pg != own && !out.contains(&pg) {
            out.push(pg);
        }
    }
    out
}

pub fn signal_groups(pgids: &[i32], signal: i32) {
    for pg in filter_groups(pgids) {
        // SAFETY: killpg su un gruppo esistente o gia' terminato (ESRCH ignorato)
        unsafe {
            libc::killpg(pg, signal);
        }
    }
}

/// Gruppo della shell e gruppo in primo piano del terminale.
pub fn terminal_process_groups(terminal: &Terminal) -> Vec<i32> {
    let mut groups = Vec::new();
    if let Some(getter) = terminal.pid_getter() {
        groups.push(getter.fallback_pid().as_u32() as i32);
    }
    if let Some(fg) = terminal.pid() {
        groups.push(fg.as_u32() as i32);
    }
    filter_groups(&groups)
}

/// SIGHUP a tutti i gruppi del terminale, poi SIGKILL a quelli ancora vivi.
pub fn terminate_terminal(terminal: &Terminal, cx: &App) {
    let groups = terminal_process_groups(terminal);
    if groups.is_empty() {
        return;
    }
    signal_groups(&groups, libc::SIGHUP);
    let timer = cx.background_executor().timer(KILL_GRACE);
    cx.background_executor()
        .spawn(async move {
            timer.await;
            signal_groups(&groups, libc::SIGKILL);
        })
        .detach();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt as _;
    use std::process::Command;
    use std::time::{Duration, Instant};

    fn group_alive(pgid: i32) -> bool {
        // SAFETY: segnale 0 = solo controllo di esistenza
        unsafe { libc::killpg(pgid, 0) == 0 }
    }

    // Review Focus 1: shell + figlio in background nello stesso process group
    #[test]
    #[allow(clippy::disallowed_methods, reason="test sincrono su un processo reale: non gira sull'executor di gpui")]
    fn close_kills_whole_process_group() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "sleep 60 & sleep 60"])
            .process_group(0)
            .spawn()
            .unwrap();
        let pgid = child.id() as i32;
        std::thread::sleep(Duration::from_millis(200));
        assert!(group_alive(pgid));
        signal_groups(&[pgid], libc::SIGHUP);
        child.wait().unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while group_alive(pgid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!group_alive(pgid), "il figlio in background e' ancora vivo");
    }

    #[test]
    fn never_signals_init_or_our_own_group() {
        // SAFETY: getpgrp non ha precondizioni
        let own = unsafe { libc::getpgrp() };
        // se il filtro mancasse, il processo di test riceverebbe SIGCONT (innocuo) o peggio
        assert_eq!(filter_groups(&[0, 1, -5, own, own]), Vec::<i32>::new());
        assert_eq!(filter_groups(&[4242, 4242, 4343]), vec![4242, 4343]);
    }
}
