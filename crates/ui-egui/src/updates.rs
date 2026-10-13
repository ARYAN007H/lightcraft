//! Native self-update state owned by the frontend.
//!
//! Downloading and extracting run on a named worker. The UI only observes messages from that
//! worker, so a slow network or a large release never stalls editing.

use std::path::PathBuf;

/// What the update affordance should say.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Idle,
    Checking,
    Staging,
    Ready {
        version: String,
        staged: PathBuf,
    },
    UpToDate,
    Throttled,
    Error(String),
}

/// State for Help ▸ Check for Updates. It deliberately contains no project state.
pub struct UpdateState {
    pub phase: Phase,
    #[cfg(not(target_arch = "wasm32"))]
    updater: Option<lightcraft_updater::Updater>,
    #[cfg(not(target_arch = "wasm32"))]
    check_state: Option<PathBuf>,
    #[cfg(not(target_arch = "wasm32"))]
    receiver: Option<std::sync::mpsc::Receiver<Event>>,
}

impl Default for UpdateState {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            #[cfg(not(target_arch = "wasm32"))]
            updater: None,
            #[cfg(not(target_arch = "wasm32"))]
            check_state: None,
            #[cfg(not(target_arch = "wasm32"))]
            receiver: None,
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
enum Event {
    Staging,
    Ready { version: String, staged: PathBuf },
    UpToDate,
    Throttled,
    Error(String),
}

impl UpdateState {
    /// Give the native frontend its already-validated executable and its app-data locations.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn configure(&mut self, updater: lightcraft_updater::Updater, check_state: PathBuf) {
        self.updater = Some(updater);
        self.check_state = Some(check_state);
    }

    /// Start a check and, when a newer release is found, stage it. A staged release is never
    /// installed by this worker.
    pub fn start_check(&mut self) -> Result<(), String> {
        if matches!(self.phase, Phase::Checking | Phase::Staging) {
            return Err("an update check is already running".into());
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let updater = self.updater.clone().ok_or("automatic updates are not configured for this application")?;
            let check_state = self.check_state.clone().ok_or("the update check location is unavailable")?;
            let (tx, rx) = std::sync::mpsc::channel();
            let spawned = std::thread::Builder::new().name("self-update".into()).spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                    match updater.check(Some((&check_state, now, std::time::Duration::from_secs(24 * 60 * 60)))) {
                        Ok(lightcraft_updater::Availability::Available(release)) => {
                            let _ = tx.send(Event::Staging);
                            updater
                                .stage(&release, &mut |_, _| true)
                                .map(|staged| Event::Ready { version: release.version.to_string(), staged })
                                .map_err(|e| e.to_string())
                        }
                        Ok(lightcraft_updater::Availability::UpToDate) => Ok(Event::UpToDate),
                        Ok(lightcraft_updater::Availability::Throttled) => Ok(Event::Throttled),
                        Err(e) => Err(e.to_string()),
                    }
                }));
                match result {
                    Ok(Ok(event)) => {
                        let _ = tx.send(event);
                    }
                    Ok(Err(error)) => {
                        let _ = tx.send(Event::Error(error));
                    }
                    Err(_) => {
                        let _ = tx.send(Event::Error("the update worker stopped unexpectedly; no update was installed".into()));
                    }
                }
            });
            if let Err(e) = spawned {
                return Err(format!("could not start the update worker: {e}"));
            }
            self.receiver = Some(rx);
            self.phase = Phase::Checking;
            Ok(())
        }
        #[cfg(target_arch = "wasm32")]
        Err("automatic updates are only available in the desktop app".into())
    }

    /// Consume completed worker messages. Returns true when the caller should repaint.
    pub fn poll(&mut self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut changed = false;
            if let Some(receiver) = self.receiver.take() {
                let mut keep = true;
                loop {
                    match receiver.try_recv() {
                        Ok(event) => {
                            changed = true;
                            self.phase = match event {
                                Event::Staging => Phase::Staging,
                                Event::Ready { version, staged } => Phase::Ready { version, staged },
                                Event::UpToDate => {
                                    keep = false;
                                    Phase::UpToDate
                                }
                                Event::Throttled => {
                                    keep = false;
                                    Phase::Throttled
                                }
                                Event::Error(error) => {
                                    keep = false;
                                    Phase::Error(error)
                                }
                            };
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            if keep {
                                changed = true;
                                keep = false;
                                self.phase = Phase::Error("the update worker stopped before reporting a result".into());
                            }
                            break;
                        }
                    }
                }
                if keep {
                    self.receiver = Some(receiver);
                }
            }
            changed
        }
        #[cfg(target_arch = "wasm32")]
        false
    }

    pub fn working(&self) -> bool {
        matches!(self.phase, Phase::Checking | Phase::Staging)
    }

    pub fn ready_version(&self) -> Option<&str> {
        match &self.phase {
            Phase::Ready { version, .. } => Some(version),
            _ => None,
        }
    }

    /// Spawn this executable in its small apply-helper mode.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn spawn_apply_helper(&self) -> Result<(), String> {
        let Phase::Ready { staged, .. } = &self.phase else {
            return Err("there is no staged update to apply".into());
        };
        let updater = self.updater.as_ref().ok_or("automatic updates are not configured for this application")?;
        std::process::Command::new(&updater.exe)
            .arg("--update-apply")
            .arg("--pid")
            .arg(std::process::id().to_string())
            .arg("--staged")
            .arg(staged)
            .arg("--target")
            .arg(&updater.exe)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("could not start the update helper: {e}"))
    }
}
