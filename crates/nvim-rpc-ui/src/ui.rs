use crate::{Backend, Checkpoint, CheckpointError, Limits, Model, RedrawError};

use nvim_rpc::rpc::Notification;

/// A backend and the Neovim UI state used to present redraw notifications.
///
/// Use this driver with an application-managed RPC receive loop. Pass redraw
/// notifications to [`Self::handle_notification()`] on the backend's UI thread.
/// Each Neovim `flush` calls [`Backend::present()`] with a complete frame.
///
/// With the shared RPC connection, use
/// [`Connection::present_next()`](crate::Connection::present_next) or
/// [`Embedded::present_next()`](crate::Embedded::present_next); its UI handler
/// already maintains the model.
pub struct Ui<B> {
    backend: B,
    model: Model,
}

impl<B> Ui<B> {
    /// Creates a UI using the default state limits.
    pub fn new(backend: B) -> Self {
        Self::with_limits(backend, Limits::default())
    }

    /// Creates a UI with explicit limits for retained state and pending events.
    pub fn with_limits(backend: B, limits: Limits) -> Self {
        Self {
            backend,
            model: Model::new(limits),
        }
    }

    /// Creates a UI with the model restored by [`Model::from_checkpoint()`].
    pub fn from_checkpoint(
        backend: B,
        checkpoint: Checkpoint,
        limits: Limits,
    ) -> Result<Self, CheckpointError> {
        Ok(Self {
            backend,
            model: Model::from_checkpoint(checkpoint, limits)?,
        })
    }

    /// Exports the last flushed or restored state through [`Model::checkpoint()`].
    pub fn checkpoint(&self) -> Option<Checkpoint> {
        self.model.checkpoint()
    }

    /// Borrows the application's backend.
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// Mutably borrows the backend for toolkit-specific operations.
    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    /// Returns the backend and discards the retained UI state.
    pub fn into_backend(self) -> B {
        self.backend
    }

    /// Reports whether an earlier redraw or presentation error stopped this UI.
    pub fn is_failed(&self) -> bool {
        self.model.is_failed()
    }
}

impl<B: Backend> Ui<B> {
    /// Applies a UI notification and presents its completed frames.
    ///
    /// Handles `redraw` and `nvim_error_event` in receive order, returning
    /// `false` for other notification methods. Borrows the notification without
    /// consuming its parameters, so the dispatcher can route unhandled methods.
    /// Has the same failure contract as [`Self::redraw()`]. See
    /// [`Model::apply_notification()`] for API-error frames between flushes.
    pub fn handle_notification<S: AsRef<[u8]>>(
        &mut self,
        notification: &Notification<S>,
    ) -> Result<bool, RedrawError<B::Error>> {
        self.model.apply_notification_with(
            notification.method.as_slice(),
            notification.params.with_borrowed_buffer(),
            |frame, _| self.backend.present(frame),
        )
    }

    /// Applies a redraw notification and presents each completed frame.
    ///
    /// Call in wire order on the backend's owning thread. Notifications without
    /// a flush update retained state without calling the backend. A notification
    /// containing several flushes presents each frame in order.
    ///
    /// The first protocol or backend error stops processing immediately. The
    /// remaining notification may contain unapplied updates; create a fresh UI
    /// attachment before resuming. Later calls return [`crate::ProtocolError::Failed`].
    pub fn redraw(
        &mut self,
        notification: Notification<&[u8]>,
    ) -> Result<(), RedrawError<B::Error>> {
        self.model
            .apply_redraw_with(notification, |frame, _| self.backend.present(frame))
    }
}
