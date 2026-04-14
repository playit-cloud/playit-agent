use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use kanal::{Receiver, Sender};
use tokio::task::LocalSet;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

use super::backend_actions::handle_request;
use super::protocol::{BackendRequest, BackendResponse};
use super::util::debug_log;

pub(super) const PROCESS_BACKEND_RESPONSES_MESSAGE: u32 = 0x8000 + 3;

pub(super) struct TrayBackend {
    request_tx: Sender<BackendRequest>,
    response_rx: Receiver<BackendResponse>,
    hwnd_bits: Arc<AtomicUsize>,
    _worker: thread::JoinHandle<()>,
}

impl TrayBackend {
    pub(super) fn new() -> Result<Self, String> {
        let (request_tx, request_rx) = kanal::bounded_async::<BackendRequest>(8);
        let (response_tx, response_rx) = kanal::bounded_async::<BackendResponse>(4);
        let hwnd_bits = Arc::new(AtomicUsize::new(0));
        let backend_hwnd_bits = hwnd_bits.clone();

        let worker = thread::Builder::new()
            .name("playitd-tray-backend".to_string())
            .spawn(move || {
                debug_log("backend: thread started");
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("failed to create tray backend runtime");

                let local = LocalSet::new();
                local.block_on(&runtime, async move {
                    let request_rx = request_rx;
                    let response_tx = response_tx;

                    while let Ok(request) = request_rx.recv().await {
                        debug_log(&format!("backend: received request {request:?}"));
                        if matches!(request, BackendRequest::Shutdown) {
                            debug_log("backend: received shutdown request");
                            break;
                        }

                        let Some(response) = handle_request(request).await else {
                            debug_log("backend: request handler returned no response");
                            continue;
                        };

                        if response_tx.send(response).await.is_err() {
                            debug_log("backend: failed to send response, channel closed");
                            break;
                        }
                        debug_log("backend: response sent to frontend");

                        let hwnd_bits = backend_hwnd_bits.load(Ordering::Relaxed);
                        if hwnd_bits != 0 {
                            unsafe {
                                let _ = PostMessageW(
                                    hwnd_bits as HWND,
                                    PROCESS_BACKEND_RESPONSES_MESSAGE,
                                    0,
                                    0,
                                );
                            }
                        } else {
                            debug_log("backend: hwnd not yet available, skipping wakeup");
                        }
                    }
                });

                debug_log("backend: thread exiting");
            })
            .map_err(|error| format!("Failed to spawn tray backend thread: {error}"))?;

        Ok(Self {
            request_tx: request_tx.to_sync(),
            response_rx: response_rx.to_sync(),
            hwnd_bits,
            _worker: worker,
        })
    }

    pub(super) fn set_hwnd(&self, hwnd: HWND) {
        self.hwnd_bits.store(hwnd as usize, Ordering::Relaxed);
        debug_log("backend: hwnd registered for frontend wakeups");
    }

    pub(super) fn try_send_request(&self, request: BackendRequest) -> Result<bool, String> {
        let result = self
            .request_tx
            .try_send(request.clone())
            .map_err(|error| format!("Tray backend request channel failed: {error}"));

        match &result {
            Ok(true) => debug_log(&format!("frontend->backend: queued request {request:?}")),
            Ok(false) => debug_log(&format!(
                "frontend->backend: backend queue full for {request:?}"
            )),
            Err(error) => debug_log(&format!(
                "frontend->backend: failed to queue request {request:?}: {error}"
            )),
        }

        result
    }

    pub(super) fn response_rx(&self) -> Receiver<BackendResponse> {
        self.response_rx.clone()
    }
}
