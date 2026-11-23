use crate::{
    Backend, Checkpoint, CheckpointError, Client, Error, Frames, Limits, PresentationError,
    UiHandler,
};

use nvim_rpc_plugin::{ConnectionError, Handler};

use tokio::io::{AsyncRead, AsyncWrite};

use std::fmt;

/// Configures UI state and the shared RPC connection.
#[derive(Debug, Clone, Copy, Default)]
pub struct ConnectionOptions {
    /// Bounds retained UI state and pending ordered events.
    pub limits: Limits,
    /// Bounds RPC messages, requests, replies, and queued callbacks.
    pub rpc: nvim_rpc_plugin::ConnectionOptions,
}

/// An error starting a shared RPC connection with a restored checkpoint.
#[derive(Debug)]
pub enum ConnectCheckpointError {
    /// The checkpoint exceeds the configured UI limits or contains invalid state.
    Checkpoint(CheckpointError),
    /// The shared RPC connection failed to start.
    Connection(ConnectionError),
}

impl fmt::Display for ConnectCheckpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Checkpoint(e) => e.fmt(f),
            Self::Connection(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for ConnectCheckpointError {}

/// A UI attachment using the plugin library's RPC connection.
///
/// The shared connection receives messages and runs application handlers.
/// Frames contain owned snapshots and can be presented on another thread.
/// Dropping this value stops RPC without terminating an external process.
pub struct Connection<W = tokio::process::ChildStdin> {
    /// UI operations using the shared RPC client.
    pub client: Client<W>,
    /// Receives the latest pending state and all ordered events.
    ///
    /// UI processing errors appear here. Use [`Self::wait()`] for RPC failures,
    /// or [`Self::present_next()`] to observe both through one operation.
    pub frames: Frames,
    rpc: nvim_rpc_plugin::Connection<W>,
    terminal_reported: bool,
}

impl<W> Connection<W> {
    /// Receives a frame and presents it on the calling thread.
    ///
    /// Returns `false` after the stream ends. Reports UI or RPC failure once,
    /// then returns `false` on subsequent calls. Cancelling a pending receive
    /// or shutdown wait leaves it available for retry. A backend error consumes
    /// that frame and leaves the connection active; account for its events and
    /// compare the next state with the last painted state before resuming.
    pub async fn present_next<B: Backend + ?Sized>(
        &mut self,
        backend: &mut B,
    ) -> Result<bool, PresentationError<B::Error>> {
        if self.terminal_reported {
            return Ok(false);
        }
        match self.frames.recv().await {
            Some(Ok(frame)) => {
                backend.present(frame).map_err(PresentationError::Backend)?;
                Ok(true)
            }
            Some(Err(e)) => {
                self.terminal_reported = true;
                Err(PresentationError::Session(e.into()))
            }
            None => {
                let result = self.rpc.wait().await;
                self.terminal_reported = true;
                result.map_err(|e| PresentationError::Session(e.into()))?;
                Ok(false)
            }
        }
    }

    /// Requests RPC shutdown and wakes pending API calls.
    ///
    /// Pending frames remain available. Does not detach the UI or terminate
    /// an external process. Use [`Self::wait()`] to finish cleanup.
    pub fn stop(&mut self) {
        self.rpc.stop();
    }

    /// Requests shutdown as in [`Self::stop()`].
    pub fn close(&mut self) {
        self.stop();
    }

    /// Waits for shared RPC task and writer cleanup.
    ///
    /// Delegates to [`nvim_rpc_plugin::Connection::wait()`], including its
    /// cancellation and repeated-wait behavior. UI errors are reported by
    /// [`Self::frames`]. This method does not request shutdown.
    pub async fn wait(&mut self) -> Result<(), nvim_rpc_plugin::ConnectionError> {
        self.rpc.wait().await
    }

    /// Requests shutdown and waits for RPC cleanup.
    pub async fn close_and_wait(&mut self) -> Result<(), nvim_rpc_plugin::ConnectionError> {
        self.rpc.close_and_wait().await
    }
}

impl<W: AsyncWrite + Unpin + Send + 'static> Connection<W> {
    /// Detaches the UI while leaving the shared RPC connection active.
    pub async fn detach_ui(&self) -> Result<(), crate::ClientError> {
        self.client.detach().await
    }
}

/// Starts a shared RPC connection with UI and application notification handling.
///
/// `handler` handles application requests and non-UI notifications. Use
/// [`crate::NoHandler`] when no application RPC methods are needed. Attach
/// the UI with [`Client::attach()`], then present frames on the toolkit thread.
/// RPC dispatch and response matching continue independently of drawing.
///
/// Returns an error for invalid RPC limits or a missing Tokio runtime.
pub fn connect<R, W, H>(
    reader: R,
    writer: W,
    handler: H,
    options: ConnectionOptions,
) -> Result<Connection<W>, Error>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
    H: Handler<W>,
{
    let (handler, frames) = UiHandler::new(handler, options.limits);
    Ok(start(reader, writer, handler, frames, options.rpc)?)
}

/// Starts an RPC connection with a checkpoint from the same Neovim process.
///
/// Validates the checkpoint against `options.limits` before starting the
/// receive task. Attach the UI with [`Client::attach()`] as with [`connect()`].
/// The first published frame requests a full repaint.
pub fn connect_with_checkpoint<R, W, H>(
    reader: R,
    writer: W,
    handler: H,
    options: ConnectionOptions,
    checkpoint: Checkpoint,
) -> Result<Connection<W>, ConnectCheckpointError>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
    H: Handler<W>,
{
    let (handler, frames) = UiHandler::from_checkpoint(handler, checkpoint, options.limits)
        .map_err(ConnectCheckpointError::Checkpoint)?;
    start(reader, writer, handler, frames, options.rpc).map_err(ConnectCheckpointError::Connection)
}

/// Starts an RPC connection using [`Model::with_checkpoint_reference()`](crate::Model::with_checkpoint_reference).
///
/// Validates the checkpoint before starting the receive task. Attach the UI
/// with [`Client::attach()`] as with [`connect()`].
pub fn connect_with_checkpoint_reference<R, W, H>(
    reader: R,
    writer: W,
    handler: H,
    options: ConnectionOptions,
    checkpoint: Checkpoint,
) -> Result<Connection<W>, ConnectCheckpointError>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
    H: Handler<W>,
{
    let (handler, frames) =
        UiHandler::with_checkpoint_reference(handler, checkpoint, options.limits)
            .map_err(ConnectCheckpointError::Checkpoint)?;
    start(reader, writer, handler, frames, options.rpc).map_err(ConnectCheckpointError::Connection)
}

fn start<R, W, H>(
    reader: R,
    writer: W,
    handler: UiHandler<H>,
    frames: Frames,
    options: nvim_rpc_plugin::ConnectionOptions,
) -> Result<Connection<W>, ConnectionError>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
    H: Handler<W>,
{
    let rpc = nvim_rpc_plugin::connect(reader, writer, handler, options)?;
    let client = Client::new(rpc.client.clone());
    Ok(Connection {
        client,
        frames,
        rpc,
        terminal_reported: false,
    })
}
