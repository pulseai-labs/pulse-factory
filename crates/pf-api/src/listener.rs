//! The TLS listener (ADR-0012): accept a TCP connection, run the TLS 1.3 handshake with the server
//! key, and serve HTTP/1.1 from the router. Each request carries the peer's address.
//!
//! The peer guard and the per-request audit lines are in the router (`guard.rs`, `router.rs`,
//! `r1.s2.w5`). A TLS failure before HTTP (a pin mismatch, a handshake timeout) is no request and
//! writes no audit record. This listener only keeps a slow or idle connection from holding resources
//! for ever: a handshake and a request head each have a time limit, and the number of open
//! connections has a ceiling.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use axum::Extension;
use axum::Router;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio_rustls::TlsAcceptor;

use crate::router::PeerAddr;

/// The time a client has to finish the TLS handshake.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// The time a client has to send a request head.
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);

/// The most connections open at once. More wait in the kernel's queue.
const MAX_CONNECTIONS: usize = 256;

/// Serve `app` on `listener` until `shutdown` completes.
pub async fn serve(
    listener: TcpListener,
    config: Arc<rustls::ServerConfig>,
    app: Router,
    shutdown: impl Future<Output = ()>,
) {
    let acceptor = TlsAcceptor::from(config);
    let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    tokio::pin!(shutdown);
    loop {
        let permit = tokio::select! {
            () = &mut shutdown => break,
            permit = permits.clone().acquire_owned() => match permit {
                Ok(p) => p,
                Err(_) => break,
            },
        };
        let (stream, peer) = tokio::select! {
            () = &mut shutdown => break,
            accepted = listener.accept() => match accepted {
                Ok(pair) => pair,
                Err(_) => {
                    // A failed accept (no file descriptors left, say) must not spin.
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
        };
        let acceptor = acceptor.clone();
        let app = app
            .clone()
            .layer(Extension(PeerAddr(peer.ip().to_canonical())));
        tokio::spawn(async move {
            let _permit = permit;
            let Ok(Ok(tls)) =
                tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await
            else {
                return;
            };
            let mut http = hyper::server::conn::http1::Builder::new();
            http.timer(TokioTimer::new())
                .header_read_timeout(HEAD_TIMEOUT);
            let _ = http
                .serve_connection(TokioIo::new(tls), TowerToHyperService::new(app))
                .await;
        });
    }
}
