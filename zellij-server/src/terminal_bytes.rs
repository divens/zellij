use crate::{
    os_input_output::AsyncReader,
    screen::ScreenInstruction,
    thread_bus::ThreadSenders,
};
use async_std::task;
use std::{
    io::Read,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use zellij_utils::{
    errors::{get_current_ctx, prelude::*, ContextType},
    logging::debug_to_file,
};

/// An `AsyncReader` that wraps a sync `Box<dyn Read + Send>` by offloading
/// blocking reads to the async runtime's blocking thread pool via `spawn_blocking`.
/// This prevents PTY reads from starving async worker threads.
struct SyncReadAsyncReader {
    reader: Arc<Mutex<Box<dyn Read + Send>>>,
}

#[async_trait::async_trait]
impl AsyncReader for SyncReadAsyncReader {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        let reader = self.reader.clone();
        let buf_len = buf.len();
        let (n, data) = task::spawn_blocking(move || {
            let mut reader_guard = reader.lock().unwrap();
            let mut temp_buf = vec![0u8; buf_len];
            match reader_guard.read(&mut temp_buf) {
                Ok(n) => Ok((n, temp_buf)),
                Err(e) => Err(e),
            }
        })
        .await?;
        buf[..n].copy_from_slice(&data[..n]);
        Ok(n)
    }
}

pub(crate) struct TerminalBytes {
    terminal_id: u32,
    senders: ThreadSenders,
    async_reader: Box<dyn AsyncReader>,
    debug: bool,
}

impl TerminalBytes {
    pub fn new(
        terminal_id: u32,
        reader: Box<dyn Read + Send>,
        senders: ThreadSenders,
        debug: bool,
    ) -> Self {
        TerminalBytes {
            terminal_id,
            senders,
            debug,
            async_reader: Box::new(SyncReadAsyncReader {
                reader: Arc::new(Mutex::new(reader)),
            }),
        }
    }
    pub async fn listen(&mut self) -> Result<()> {
        // This function reads bytes from the pty and then sends them as
        // ScreenInstruction::PtyBytes to screen to be parsed there
        // We also send a separate instruction to Screen to render as ScreenInstruction::Render
        //
        // We endeavour to send a Render instruction to screen immediately after having send bytes
        // to parse - this is so that the rendering is quick and smooth. However, this can cause
        // latency if the screen is backed up. For this reason, if we detect a peak in the time it
        // takes to send the render instruction, we assume the screen thread is backed up and so
        // only send a render instruction sparingly, giving screen time to process bytes and render
        // while still allowing the user to see an indication that things are happening (the
        // sparing render instructions)
        let err_context = || "failed to listen for bytes from PTY".to_string();

        let mut err_ctx = get_current_ctx();
        err_ctx.add_call(ContextType::AsyncTask);
        let mut buf = [0u8; 65536];
        loop {
            match self.async_reader.read(&mut buf).await {
                Ok(0) => break, // EOF
                Err(err) => {
                    log::error!("{}", err);
                    break;
                },
                Ok(n_bytes) => {
                    let bytes = &buf[..n_bytes];
                    if self.debug {
                        let _ = debug_to_file(bytes, self.terminal_id as i32);
                    }
                    self.async_send_to_screen(ScreenInstruction::PtyBytes(
                        self.terminal_id,
                        bytes.to_vec(),
                    ))
                    .await
                    .with_context(err_context)?;
                },
            }
        }

        // Ignore any errors that happen here.
        // We only leave the loop above when the pane exits. This can happen in a lot of ways, but
        // the most problematic is when quitting zellij with `Ctrl+q`. That is because the channel
        // for `Screen` will have exited already, so this send *will* fail. This isn't a problem
        // per-se because the application terminates anyway, but it will print a lengthy error
        // message into the log for every pane that was still active when we quit the application.
        // This:
        //
        // 1. Makes the log rather pointless, because even when the application exits "normally",
        //    there will be errors inside and
        // 2. Leaves the impression we have a bug in the code and can't terminate properly
        //
        // FIXME: Ideally we detect whether the application is being quit and only ignore the error
        // in that particular case?
        let _ = self.async_send_to_screen(ScreenInstruction::Render).await;

        Ok(())
    }
    async fn async_send_to_screen(
        &self,
        screen_instruction: ScreenInstruction,
    ) -> Result<Duration> {
        // returns the time it blocked the thread for
        let sent_at = Instant::now();
        let senders = self.senders.clone();
        task::spawn_blocking(move || senders.send_to_screen(screen_instruction))
            .await
            .context("failed to async-send to screen")?;
        Ok(sent_at.elapsed())
    }
}
