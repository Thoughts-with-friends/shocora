//! GPU interrupts a title asked for with `GX2SubmitUserTimeStamp`: raised once the GPU has
//! executed the commands before them, not when they are submitted.
impl crate::CafeOs {
    /// Raises the timestamp interrupts whose commands the GPU has executed.
    pub fn fire_gpu_interrupts(&mut self) {
        if self.gx2.pending_interrupts.is_empty() {
            return;
        }
        let retired = self
            .coreinit
            .threads
            .gpu_retired
            .as_ref()
            .map_or(u64::MAX, |retired| retired.load(std::sync::atomic::Ordering::Acquire));
        let mut due = Vec::new();
        self.gx2.pending_interrupts.retain(|&(stamp, kind)| {
            let done = stamp <= retired;
            if done {
                due.push(kind);
            }
            !done
        });
        for kind in due {
            super::command::notify_event(self, kind);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };

    use super::super::command::event;
    use crate::CafeOs;

    /// Intent: a timestamp interrupt waits for the GPU to reach its commands.
    /// Pass: no handler is queued while the GPU is behind; one is once it has caught up.
    #[test]
    fn interrupts_wait_for_the_gpu() {
        let mut os = CafeOs::default();
        os.gx2.event_callbacks[event::TIMESTAMP_BOTTOM as usize] = (0x0100_0000, 7);
        let retired = Arc::new(AtomicU64::new(4));
        os.coreinit.threads.gpu_retired = Some(Arc::clone(&retired));
        os.gx2.pending_interrupts.push((5, event::TIMESTAMP_BOTTOM));

        os.fire_gpu_interrupts();
        assert_eq!(os.callbacks.pending(), 0);

        retired.store(5, Ordering::Release);
        os.fire_gpu_interrupts();
        assert_eq!(os.callbacks.pending(), 1);
        assert!(os.gx2.pending_interrupts.is_empty());
    }
}
