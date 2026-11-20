//! The render thread.
use super::*;

/// Runs the renderer on its own thread, so the guest does not wait for recording.
#[derive(Debug)]
pub struct RenderThread {
    sender: Option<std::sync::mpsc::SyncSender<Vec<RenderCommand>>>,
    thread: Option<std::thread::JoinHandle<()>>,
    screens: ScreenImages,
}

/// Frames of commands the emulator may be ahead of the renderer.
const RENDER_QUEUE_LENGTH: usize = 3;

impl RenderThread {
    /// # Errors
    /// Fails if the thread cannot be spawned.
    pub fn spawn(mut renderer: LatteRenderer) -> std::io::Result<Self> {
        let screens = renderer.screens();
        let (sender, receiver) =
            std::sync::mpsc::sync_channel::<Vec<RenderCommand>>(RENDER_QUEUE_LENGTH);
        let thread =
            std::thread::Builder::new().name("shocora-render".into()).spawn(move || {
                loop {
                    let commands = match receiver.try_recv() {
                        Ok(commands) => commands,
                        Err(std::sync::mpsc::TryRecvError::Empty) => {
                            // Nothing new: the frame's last parts go to the GPU now.
                            renderer.settle();
                            match receiver.recv() {
                                Ok(commands) => commands,
                                Err(_) => break,
                            }
                        }
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                    };
                    renderer.execute(&commands);
                }
            })?;
        Ok(Self { sender: Some(sender), thread: Some(thread), screens })
    }

    #[inline]
    pub fn screens(&self) -> ScreenImages {
        self.screens.clone()
    }

    /// A clonable handle for the emulation thread.
    #[inline]
    pub fn sender(&self) -> Option<std::sync::mpsc::SyncSender<Vec<RenderCommand>>> {
        self.sender.clone()
    }
}

impl Drop for RenderThread {
    fn drop(&mut self) {
        self.sender = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
