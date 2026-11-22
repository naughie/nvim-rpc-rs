//! UI frame delivery through the plugin RPC dispatcher.

mod error;
pub use error::Error;
mod frames;
use frames::Sender;
pub use frames::{Coalescing, Frames};

use crate::{Checkpoint, CheckpointError, Limits, Model, RedrawError};

use nvim_rpc_plugin::{Array, FromParams, Handler, Neovim};

use tokio::io::AsyncWrite;

use std::sync::{Mutex, MutexGuard};

/// Adds UI notification handling to an application's RPC handler.
///
/// Requests and non-UI notifications retain [`Handler`]'s execution order.
/// `redraw` and `nvim_error_event` notifications are consumed by the UI model
/// before later callbacks are dispatched. Invalid UI or application notification
/// arguments close the shared connection; [`Frames::recv()`] drains the pending
/// frame before reporting the error.
pub struct UiHandler<H> {
    handler: H,
    model: Mutex<Model>,
    frames: Sender,
}

impl<H> UiHandler<H> {
    /// Wraps a handler and returns its bounded frame receiver.
    ///
    /// Dropping the receiver ends the connection when the next frame is
    /// published. Dropping this handler finishes the stream after its pending
    /// frame. `limits` controls the model, coalesced event count, and incremental
    /// damage bounds. Select the coalescing policy on the returned [`Frames`].
    pub fn new(handler: H, limits: Limits) -> (Self, Frames) {
        Self::with_model(handler, Model::new(limits), limits)
    }

    /// Wraps a handler with the model restored by [`Model::from_checkpoint()`].
    ///
    /// Uses the same frame delivery limits as [`Self::new()`].
    pub fn from_checkpoint(
        handler: H,
        checkpoint: Checkpoint,
        limits: Limits,
    ) -> Result<(Self, Frames), CheckpointError> {
        let model = Model::from_checkpoint(checkpoint, limits)?;
        Ok(Self::with_model(handler, model, limits))
    }

    fn with_model(handler: H, model: Model, limits: Limits) -> (Self, Frames) {
        let (frames, receiver) = Frames::channel(
            limits.max_pending_events,
            limits.max_grids,
            model.checkpoint(),
        );
        (
            Self {
                handler,
                model: Mutex::new(model),
                frames,
            },
            receiver,
        )
    }

    fn model(&self) -> MutexGuard<'_, Model> {
        self.model.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl<W, H> Handler<W> for UiHandler<H>
where
    W: AsyncWrite + Unpin + Send + 'static,
    H: Handler<W>,
{
    type Request = H::Request;
    type Response = H::Response;
    type Notification = Array;

    async fn on_request(
        &self,
        method: &[u8],
        params: Self::Request,
        nvim: Neovim<W>,
    ) -> Self::Response {
        self.handler.on_request(method, params, nvim).await
    }

    async fn on_notification(&self, method: &[u8], params: Array, nvim: Neovim<W>) {
        let args = params.with_borrowed_buffer();
        let result = self
            .model()
            .apply_notification_with(method, args, |frame, flushed| {
                self.frames.send(frame, flushed)
            });
        let error = match result {
            Ok(true) => return,
            Ok(false) => match H::Notification::from_params(params) {
                Ok(params) => {
                    self.handler.on_notification(method, params, nvim).await;
                    return;
                }
                Err(e) => Error::Notification(e),
            },
            Err(RedrawError::Protocol(e)) => Error::Protocol(e),
            Err(RedrawError::Backend(e)) => e,
        };
        self.frames.fail(error);
        nvim.close();
    }
}
