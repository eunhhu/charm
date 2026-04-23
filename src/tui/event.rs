use crate::runtime::session_runtime::SessionRuntime;
use crate::runtime::types::RuntimeEvent;
use std::sync::mpsc;
use std::thread;

pub enum AppEvent {
    Event(RuntimeEvent),
    Error(anyhow::Error),
    Terminated,
}

pub struct EventBridge {
    events_rx: mpsc::Receiver<AppEvent>,
    pub input_tx: mpsc::Sender<String>,
}

impl EventBridge {
    pub fn spawn_runtime(
        mut runtime: SessionRuntime,
        rt: tokio::runtime::Runtime,
    ) -> (Self, thread::JoinHandle<()>) {
        let (input_tx, input_rx) = mpsc::channel::<String>();
        let (events_tx, events_rx) = mpsc::channel::<AppEvent>();

        let handle = thread::spawn(move || {
            while let Ok(input) = input_rx.recv() {
                let (stream_tx, stream_rx): (
                    mpsc::Sender<RuntimeEvent>,
                    mpsc::Receiver<RuntimeEvent>,
                ) = mpsc::channel();

                let events_tx_clone = events_tx.clone();
                let fwd_handle = thread::spawn(move || {
                    for event in stream_rx.iter() {
                        if events_tx_clone.send(AppEvent::Event(event)).is_err() {
                            return;
                        }
                    }
                });

                let result = rt.block_on(runtime.submit_input_streaming(&input, stream_tx));
                let _ = fwd_handle.join();

                if let Err(e) = result {
                    let _ = events_tx.send(AppEvent::Error(e));
                    break;
                }
            }
            let _ = events_tx.send(AppEvent::Terminated);
        });

        let bridge = Self {
            events_rx,
            input_tx: input_tx.clone(),
        };
        (bridge, handle)
    }

    pub fn try_recv(&self) -> Option<AppEvent> {
        self.events_rx.try_recv().ok()
    }
}
