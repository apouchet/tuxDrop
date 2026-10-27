//! tuxdrop-share: the Windows Share sheet entry. The tuxDrop sparse package
//! (`packaging/windows`) declares it as a share target: Explorer starts it
//! with the shared files, it hands them to tuxdropd (starting it if needed),
//! which asks for the device, and exits. Started with paths as arguments, it
//! shares those instead.

#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

#[cfg(not(windows))]
fn main() {
    eprintln!("tuxdrop-share is the Windows Share sheet entry; use `tuxdrop share` instead");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() {
    if let Err(e) = share::run() {
        share::alert(&format!("{e:#}"));
        std::process::exit(1);
    }
}

#[cfg(windows)]
mod share {
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use anyhow::{Context, Result, bail};
    use tuxdrop_core::ipc::{self, Request, Response};
    use windows::ApplicationModel::Activation::{ActivationKind, ShareTargetActivatedEventArgs};
    use windows::ApplicationModel::AppInstance;
    use windows::ApplicationModel::DataTransfer::ShareTarget::ShareOperation;
    use windows::ApplicationModel::DataTransfer::StandardDataFormats;
    use windows::Win32::UI::WindowsAndMessaging::{
        ASFW_ANY, AllowSetForegroundWindow, MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MessageBoxW,
    };
    use windows::core::{HSTRING, Interface};

    /// How long a daemon started here has to open its control pipe.
    const DAEMON_START: Duration = Duration::from_secs(10);

    pub fn alert(message: &str) {
        // SAFETY: both strings outlive the call.
        unsafe {
            MessageBoxW(
                None,
                &HSTRING::from(message),
                &HSTRING::from("tuxDrop"),
                MB_OK | MB_ICONERROR | MB_SETFOREGROUND,
            );
        }
    }

    pub fn run() -> Result<()> {
        let Some(operation) = share_operation() else {
            let paths: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
            if paths.is_empty() {
                bail!("Share files with tuxDrop from Explorer: right-click, Share");
            }
            return hand_over(paths);
        };
        let shared = shared_paths(&operation);
        let _ = operation.ReportDataRetrieved();
        let result = shared.and_then(hand_over);
        // Ends the share; Windows may stop this program right after.
        if let Err(e) = &result {
            alert(&format!("{e:#}"));
        }
        let _ = operation.ReportCompleted();
        Ok(())
    }

    /// The share this program was started for, if any (only a program with
    /// the package's identity can be).
    fn share_operation() -> Option<ShareOperation> {
        let args = AppInstance::GetActivatedEventArgs().ok()?;
        if args.Kind().ok()? != ActivationKind::ShareTarget {
            return None;
        }
        args.cast::<ShareTargetActivatedEventArgs>()
            .ok()?
            .ShareOperation()
            .ok()
    }

    fn shared_paths(operation: &ShareOperation) -> Result<Vec<PathBuf>> {
        let data = operation.Data()?;
        if !data.Contains(&StandardDataFormats::StorageItems()?)? {
            bail!("tuxDrop can only share files and folders");
        }
        let items = data.GetStorageItemsAsync()?.join()?;
        let mut paths = Vec::new();
        for item in items {
            let path = item.Path()?.to_string_lossy();
            if path.is_empty() {
                bail!("{} is not a file on this PC", item.Name()?);
            }
            paths.push(PathBuf::from(path));
        }
        Ok(paths)
    }

    /// Gives `paths` to tuxdropd, which asks which device to send them to.
    fn hand_over(paths: Vec<PathBuf>) -> Result<()> {
        let paths = paths
            .into_iter()
            .map(|p| Ok(std::path::absolute(p)?.to_string_lossy().into_owned()))
            .collect::<Result<Vec<_>>>()?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(async {
            if !ipc::running().await? {
                start_daemon()?;
                let started = Instant::now();
                while !ipc::running().await? {
                    if started.elapsed() > DAEMON_START {
                        bail!("tuxdropd did not start, see %LOCALAPPDATA%\\tuxdrop\\tuxdropd.log");
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
            // Lets the daemon's device dialog come to the front: this
            // program, which the user just picked, has the right to.
            // SAFETY: no pointer involved.
            let _ = unsafe { AllowSetForegroundWindow(ASFW_ANY) };
            match ipc::call(&Request::Share { paths }).await? {
                Some(Response::Done) => Ok(()),
                Some(Response::Error(e)) => bail!(e),
                Some(other) => bail!("unexpected answer from tuxdropd: {other:?}"),
                None => bail!("tuxdropd stopped"),
            }
        })
    }

    /// Starts tuxdropd.exe from this program's folder, outside the share's
    /// job so that it keeps running.
    fn start_daemon() -> Result<()> {
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        let exe = std::env::current_exe()?
            .with_file_name("tuxdropd.exe");
        let mut command = std::process::Command::new(&exe);
        if command.creation_flags(CREATE_BREAKAWAY_FROM_JOB).spawn().is_err() {
            // The job may not allow breaking away.
            std::process::Command::new(&exe)
                .spawn()
                .with_context(|| format!("cannot start {}", exe.display()))?;
        }
        Ok(())
    }
}