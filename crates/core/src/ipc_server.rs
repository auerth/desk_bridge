//! UI protocol server: serves newline-delimited JSON over any async byte stream (named pipe on
//! Windows, in-memory duplex in tests).

use crate::engine::EngineHandle;
use crate::ipc::{ReplyEnvelope, RequestEnvelope, ServerMessage};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::broadcast::error::RecvError;

/// Serves one client until it disconnects. Sends the current state first, then pushes.
pub async fn serve_connection<S>(stream: S, engine: EngineHandle)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (rd, mut wr) = tokio::io::split(stream);
    let mut lines = BufReader::new(rd).lines();
    let mut pushes = engine.subscribe();
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

    // writer task
    let writer = tokio::spawn(async move {
        while let Some(line) = out_rx.recv().await {
            if wr.write_all(line.as_bytes()).await.is_err() || wr.write_all(b"\n").await.is_err() {
                break;
            }
            let _ = wr.flush().await;
        }
    });

    // initial state
    let state = engine.state().await;
    let first = ServerMessage::Push { push: crate::ipc::Push::State { state: Box::new(state) } };
    let _ = out_tx.send(serde_json::to_string(&first).unwrap_or_default());

    // push forwarder
    let push_out = out_tx.clone();
    let forwarder = tokio::spawn(async move {
        loop {
            match pushes.recv().await {
                Ok(p) => {
                    let line = serde_json::to_string(&ServerMessage::Push { push: p }).unwrap_or_default();
                    if push_out.send(line).is_err() {
                        break;
                    }
                }
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => break,
            }
        }
    });

    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let env: RequestEnvelope = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(e) => {
                tracing::debug!("bad ui request: {e}");
                continue;
            }
        };
        let engine = engine.clone();
        let out = out_tx.clone();
        tokio::spawn(async move {
            let reply = match engine.request(env.req).await {
                Ok(data) => ReplyEnvelope { id: env.id, ok: true, data: Some(data), error: None },
                Err(error) => ReplyEnvelope { id: env.id, ok: false, data: None, error: Some(error) },
            };
            let _ = out.send(serde_json::to_string(&ServerMessage::Reply(reply)).unwrap_or_default());
        });
    }
    forwarder.abort();
    drop(out_tx);
    let _ = writer.await;
}
