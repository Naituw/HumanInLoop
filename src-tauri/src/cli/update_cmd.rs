#[cfg(windows)]
use std::time::{Duration, Instant};

pub fn dispatch(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("prepare") if args.len() == 1 => prepare(),
        _ => {
            eprintln!("Usage: AskHuman update prepare");
            1
        }
    }
}

#[cfg(windows)]
fn prepare() -> i32 {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("AskHuman update prepare: failed to create runtime: {error}");
            return 1;
        }
    };

    runtime.block_on(async {
        let kind = crate::update::detect_install_kind();
        let current_exe = std::env::current_exe().ok();

        if crate::client::request_status().await.is_some() {
            if !crate::client::request_stop(false).await {
                eprintln!("AskHuman update prepare: failed to request a graceful daemon stop");
                return 1;
            }
            eprintln!("AskHuman update prepare: waiting for the daemon to drain…");
            let mut last_progress = Instant::now() - Duration::from_secs(30);
            loop {
                let Some(status) = crate::client::request_status().await else {
                    break;
                };
                if last_progress.elapsed() >= Duration::from_secs(30) {
                    eprintln!(
                        "AskHuman update prepare: {} active request(s) remaining…",
                        status.active_requests
                    );
                    last_progress = Instant::now();
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }

        // A Settings Host may be finishing the update check whose HTTP client has a 30-second
        // timeout. Give graceful Tauri shutdown enough room to finish that bounded task.
        let deadline = Instant::now() + Duration::from_secs(45);
        loop {
            let sent = crate::gui_host::shutdown_if_running().await;
            if !sent
                && crate::ipc::transport::connect_role("gui-host")
                    .await
                    .is_err()
            {
                break;
            }
            if Instant::now() >= deadline {
                eprintln!(
                    "AskHuman update prepare: timed out closing the GUI Host at {}",
                    crate::ipc::transport::endpoint_path("gui-host").display()
                );
                return 1;
            }
            // Retry the idempotent request until the pipe disappears. This also covers an old Host
            // that closes without the acknowledgement introduced alongside `update prepare`.
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        // A concurrent caller could have started the daemon while the Host was closing. Never
        // report readiness while a process that may lock the installed executable is reachable.
        if crate::client::request_status().await.is_some() {
            eprintln!("AskHuman update prepare: the daemon restarted during preparation; retry");
            return 1;
        }

        println!("AskHuman update prepare: ready");
        match kind {
            crate::update::InstallKind::Npm => {
                println!("next: {}", crate::update::npm::NpmUpdater::manual_command());
            }
            crate::update::InstallKind::Direct => {
                println!(
                    "release: https://github.com/{}/{}/releases/latest",
                    crate::update::GITHUB_OWNER,
                    crate::update::GITHUB_REPO
                );
                if let Some(path) = current_exe {
                    println!("target: {}", path.display());
                }
                println!("next: extract the Windows zip and replace the target executable");
            }
        }
        0
    })
}

#[cfg(not(windows))]
fn prepare() -> i32 {
    eprintln!("AskHuman update prepare is only required on Windows");
    1
}

#[cfg(test)]
mod tests {
    #[test]
    fn unknown_subcommand_is_rejected() {
        assert_eq!(super::dispatch(&["unknown".to_string()]), 1);
    }
}
