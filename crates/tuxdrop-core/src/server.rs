//! HTTPS server for the LocalSend API.
//!
//! Connections are accepted by hand (instead of `axum::serve`) so each request
//! can carry the peer address and the fingerprint of its TLS client
//! certificate.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use axum::body::Body;
use axum::extract::{Extension, Query, State};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;
use tower_service::Service;

use crate::discovery::Peer;
use crate::identity::{Identity, fingerprint_of, same_fingerprint};
use crate::proto::{
    API_PREFIX, DeviceInfo, PinQuery, PrepareUploadRequest, SessionQuery, UploadQuery,
};
use crate::receive::Receiver;
use crate::tls;

#[derive(Clone)]
struct Conn {
    addr: IpAddr,
    client_fingerprint: Option<String>,
}

struct AppState {
    me: DeviceInfo,
    receiver: Receiver,
    peers: mpsc::Sender<Peer>,
}

/// Binds a dual-stack listener (IPv4 arrives as v4-mapped addresses).
pub fn bind(addr: SocketAddr) -> Result<TcpListener> {
    let domain = socket2::Domain::for_address(addr);
    let socket = socket2::Socket::new(domain, socket2::Type::STREAM, None)?;
    if addr.is_ipv6() {
        socket.set_only_v6(false)?;
    }
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    socket.listen(128)?;
    Ok(TcpListener::from_std(socket.into())?)
}

pub async fn serve(
    listener: TcpListener,
    identity: &Identity,
    me: DeviceInfo,
    receiver: Receiver,
    peers: mpsc::Sender<Peer>,
) -> Result<()> {
    let acceptor = TlsAcceptor::from(Arc::new(tls::server_config(identity)?));
    let router = Router::new()
        .route("/register", post(register))
        .route("/info", get(info))
        .route("/prepare-upload", post(prepare_upload))
        .route("/upload", post(upload))
        .route("/cancel", post(cancel));
    let router = Router::new()
        .nest(API_PREFIX, router)
        .with_state(Arc::new(AppState {
            me,
            receiver,
            peers,
        }));

    loop {
        let (tcp, addr) = match listener.accept().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let acceptor = acceptor.clone();
        let router = router.clone();
        tokio::spawn(async move {
            let tls =
                match tokio::time::timeout(Duration::from_secs(10), acceptor.accept(tcp)).await {
                    Ok(Ok(tls)) => tls,
                    Ok(Err(e)) => return tracing::debug!("TLS handshake with {addr} failed: {e}"),
                    Err(_) => return tracing::debug!("TLS handshake with {addr} timed out"),
                };
            let conn = Conn {
                addr: addr.ip().to_canonical(),
                client_fingerprint: tls
                    .get_ref()
                    .1
                    .peer_certificates()
                    .and_then(|certs| certs.first())
                    .map(|c| fingerprint_of(c)),
            };
            let service =
                hyper::service::service_fn(move |mut req: Request<hyper::body::Incoming>| {
                    req.extensions_mut().insert(conn.clone());
                    router.clone().call(req.map(Body::new))
                });
            if let Err(e) = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                .serve_connection(TokioIo::new(tls), service)
                .await
            {
                tracing::debug!("connection with {addr} ended: {e}");
            }
        });
    }
}

async fn register(
    State(app): State<Arc<AppState>>,
    Extension(conn): Extension<Conn>,
    Json(info): Json<DeviceInfo>,
) -> Json<DeviceInfo> {
    if !same_fingerprint(&info.fingerprint, &app.me.fingerprint) {
        let _ = app.peers.try_send(Peer::new(info, conn.addr));
    }
    Json(app.me.clone())
}

async fn info(State(app): State<Arc<AppState>>) -> Json<DeviceInfo> {
    Json(app.me.clone())
}

async fn prepare_upload(
    State(app): State<Arc<AppState>>,
    Extension(conn): Extension<Conn>,
    Query(pin): Query<PinQuery>,
    Json(req): Json<PrepareUploadRequest>,
) -> Response {
    match app
        .receiver
        .prepare_upload(conn.addr, conn.client_fingerprint, pin.pin, req)
        .await
    {
        Ok(resp) => Json(resp).into_response(),
        Err(status) => status.into_response(),
    }
}

async fn upload(
    State(app): State<Arc<AppState>>,
    Extension(conn): Extension<Conn>,
    Query(q): Query<UploadQuery>,
    body: Body,
) -> StatusCode {
    match app
        .receiver
        .upload(conn.addr, q, body.into_data_stream())
        .await
    {
        Ok(()) => StatusCode::OK,
        Err(status) => status,
    }
}

async fn cancel(
    State(app): State<Arc<AppState>>,
    Extension(conn): Extension<Conn>,
    Query(q): Query<SessionQuery>,
) -> StatusCode {
    match app.receiver.cancel(conn.addr, &q.session_id).await {
        Ok(()) => StatusCode::OK,
        Err(status) => status,
    }
}
