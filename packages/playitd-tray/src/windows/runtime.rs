use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use tokio::sync::mpsc;
use tokio::task::LocalSet;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

use super::actions::run_background_action_async;
use super::state::{AppEvent, BackgroundAction, BackgroundActionResult};

const PROCESS_EVENTS_MESSAGE: u32 = 0x8000 + 2;

#[derive(Clone, Debug)]
pub(super) enum AsyncCommand {
    RunBackgroundAction(BackgroundAction),
}

#[derive(Clone, Debug)]
enum AsyncCompletion {
    BackgroundActionFinished {
        action: BackgroundAction,
        result: BackgroundActionResult,
    },
}

pub(super) struct TrayRuntime {
    _worker: thread::JoinHandle<()>,
    sender: mpsc::UnboundedSender<AsyncCommand>,
    hwnd_bits: Arc<AtomicUsize>,
}

impl TrayRuntime {
    pub(super) fn new(event_queue: Arc<Mutex<VecDeque<AppEvent>>>) -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("Failed to create tray runtime: {error}"))?;

        let (sender, mut receiver) = mpsc::unbounded_channel();
        let hwnd_bits = Arc::new(AtomicUsize::new(0));
        let completion_hwnd = hwnd_bits.clone();

        let worker = thread::Builder::new()
            .name("playitd-tray-runtime".to_string())
            .spawn(move || {
                let local = LocalSet::new();
                local.block_on(&runtime, async move {
                    while let Some(command) = receiver.recv().await {
                        let completion = match command {
                            AsyncCommand::RunBackgroundAction(action) => {
                                let result = run_background_action_async(action.clone()).await;
                                AsyncCompletion::BackgroundActionFinished { action, result }
                            }
                        };

                        if let Ok(mut queue) = event_queue.lock() {
                            match completion {
                                AsyncCompletion::BackgroundActionFinished { action, result } => {
                                    queue.push_back(AppEvent::BackgroundActionFinished {
                                        action,
                                        result,
                                    });
                                }
                            }
                        }

                        let hwnd_bits = completion_hwnd.load(Ordering::Relaxed);
                        if hwnd_bits != 0 {
                            unsafe {
                                let _ =
                                    PostMessageW(hwnd_bits as HWND, PROCESS_EVENTS_MESSAGE, 0, 0);
                            }
                        }
                    }
                });
            })
            .map_err(|error| format!("Failed to spawn tray runtime thread: {error}"))?;

        Ok(Self {
            _worker: worker,
            sender,
            hwnd_bits,
        })
    }

    pub(super) fn set_hwnd(&self, hwnd: HWND) {
        self.hwnd_bits.store(hwnd as usize, Ordering::Relaxed);
    }

    pub(super) fn dispatch_background_action(
        &self,
        action: BackgroundAction,
    ) -> Result<(), String> {
        self.sender
            .send(AsyncCommand::RunBackgroundAction(action))
            .map_err(|_| "Playit tray runtime is no longer running".to_string())
    }
}
