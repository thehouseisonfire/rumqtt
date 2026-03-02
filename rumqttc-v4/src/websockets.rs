//! WebSocket URL/handshake helpers and the `WsAdapter` transport bridge.

use http::{Response, header::ToStrError};

#[cfg(feature = "websocket")]
use async_tungstenite::{
    WebSocketReceiver, WebSocketSender, WebSocketStream,
    bytes::{ByteReader, ByteWriter},
    tungstenite::Message,
};
#[cfg(feature = "websocket")]
use futures_io::{AsyncRead as FuturesAsyncRead, AsyncWrite as FuturesAsyncWrite};
#[cfg(feature = "websocket")]
use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};
#[cfg(feature = "websocket")]
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// Bridges `async-tungstenite`'s split WebSocket halves (`ByteReader` + `ByteWriter`)
/// into a unified `AsyncRead + AsyncWrite` object compatible with `rumqttc`'s `Network`
/// abstraction.
///
/// This replaces the former `ws_stream_tungstenite::WsStream` wrapper and eliminates
/// the `ws_stream_tungstenite` dependency from the `websocket` feature.
#[cfg(feature = "websocket")]
pub(crate) struct WsAdapter<S> {
    reader: ByteReader<WebSocketReceiver<S>>,
    writer: ByteWriter<WebSocketSender<S>>,
}

#[cfg(feature = "websocket")]
impl<S> WsAdapter<S>
where
    S: FuturesAsyncRead + FuturesAsyncWrite + Unpin,
{
    pub(crate) fn new(ws: WebSocketStream<S>) -> Self {
        let (sender, receiver) = ws.split();
        Self {
            reader: ByteReader::new(receiver),
            writer: ByteWriter::new(sender),
        }
    }
}

#[cfg(feature = "websocket")]
impl<S: Unpin> AsyncRead for WsAdapter<S>
where
    WebSocketReceiver<S>:
        futures_util::Stream<Item = Result<Message, async_tungstenite::tungstenite::Error>> + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        tokio::io::AsyncRead::poll_read(Pin::new(&mut self.reader), cx, buf)
    }
}

#[cfg(feature = "websocket")]
impl<S: Unpin> AsyncWrite for WsAdapter<S>
where
    WebSocketSender<S>: async_tungstenite::bytes::Sender + Unpin,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        tokio::io::AsyncWrite::poll_write(Pin::new(&mut self.writer), cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        tokio::io::AsyncWrite::poll_flush(Pin::new(&mut self.writer), cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        tokio::io::AsyncWrite::poll_shutdown(Pin::new(&mut self.writer), cx)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum UrlError {
    #[error("Invalid protocol specified inside url.")]
    Protocol,
    #[error("Couldn't parse host from url.")]
    Host,
    #[error("Couldn't parse host url.")]
    Parse(#[from] http::uri::InvalidUri),
}

#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("Websocket response does not contain subprotocol header")]
    SubprotocolHeaderMissing,
    #[error("MQTT not in subprotocol header: {0}")]
    SubprotocolMqttMissing(String),
    #[error("Subprotocol header couldn't be converted into string representation")]
    HeaderToStr(#[from] ToStrError),
}

pub(crate) fn validate_response_headers(
    response: Response<Option<Vec<u8>>>,
) -> Result<(), ValidationError> {
    let subprotocol = response
        .headers()
        .get("Sec-WebSocket-Protocol")
        .ok_or(ValidationError::SubprotocolHeaderMissing)?
        .to_str()?;

    // Server must respond with Sec-WebSocket-Protocol header value of "mqtt"
    // https://http.dev/ws#sec-websocket-protocol
    if subprotocol.trim() != "mqtt" {
        return Err(ValidationError::SubprotocolMqttMissing(
            subprotocol.to_owned(),
        ));
    }

    Ok(())
}

pub(crate) fn split_url(url: &str) -> Result<(String, u16), UrlError> {
    let uri = url.parse::<http::Uri>()?;
    let domain = domain(&uri).ok_or(UrlError::Protocol)?;
    let port = port(&uri).ok_or(UrlError::Host)?;
    Ok((domain, port))
}

fn domain(uri: &http::Uri) -> Option<String> {
    uri.host().map(|host| {
        // If host is an IPv6 address, it might be surrounded by brackets. These brackets are
        // *not* part of a valid IP, so they must be stripped out.
        //
        // The URI from the request is guaranteed to be valid, so we don't need a separate
        // check for the closing bracket.
        let host = if host.starts_with('[') {
            &host[1..host.len() - 1]
        } else {
            host
        };

        host.to_owned()
    })
}

fn port(uri: &http::Uri) -> Option<u16> {
    uri.port_u16().or_else(|| match uri.scheme_str() {
        Some("wss") => Some(443),
        Some("ws") => Some(80),
        _ => None,
    })
}
