use crate::{Error, Result};

/// Declarative handshake edits, applied in order to each new connection's
/// request. Values are always redacted, including nonstandard credential headers.
#[derive(Clone, PartialEq, Eq)]
pub enum WebSocketHeader {
    Append { name: String, value: String },
    Replace { name: String, value: String },
    Remove { name: String },
}

impl std::fmt::Debug for WebSocketHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WebSocketHeader([REDACTED])")
    }
}

impl WebSocketHeader {
    pub(crate) fn validate(&self) -> Result<()> {
        if !cfg!(feature = "websocket") {
            return Err(Error::configuration("WebSocket feature is disabled"));
        }
        #[cfg(feature = "websocket")]
        self.parse()?;
        Ok(())
    }

    #[cfg(feature = "websocket")]
    fn parse(&self) -> Result<(http::HeaderName, Option<http::HeaderValue>)> {
        let (name, value) = match self {
            Self::Append { name, value } | Self::Replace { name, value } => (name, Some(value)),
            Self::Remove { name } => (name, None),
        };
        let name = http::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| Error::configuration("invalid WebSocket header name"))?;
        if matches!(
            name.as_str(),
            "host" | "connection" | "upgrade" | "content-length" | "transfer-encoding"
        ) || name.as_str().starts_with("sec-websocket-")
        {
            return Err(Error::configuration(
                "cannot modify protected WebSocket headers",
            ));
        }
        let value = value
            .map(|v| {
                let mut value = http::HeaderValue::from_str(v)
                    .map_err(|_| Error::configuration("invalid WebSocket header value"))?;
                value.set_sensitive(true);
                Ok(value)
            })
            .transpose()?;
        Ok((name, value))
    }
}

#[cfg(feature = "websocket")]
pub fn prepare(
    headers: &[WebSocketHeader],
) -> Result<
    impl Fn(http::Request<()>) -> std::future::Ready<http::Request<()>> + Send + Sync + 'static,
> {
    let parsed: Vec<_> = headers
        .iter()
        .map(|header| {
            let (name, value) = header.parse()?;
            Ok((
                matches!(header, WebSocketHeader::Append { .. }),
                name,
                value,
            ))
        })
        .collect::<Result<_>>()?;
    Ok(move |mut request: http::Request<()>| {
        for (append, name, value) in &parsed {
            match value {
                Some(value) if *append => {
                    request.headers_mut().append(name.clone(), value.clone());
                }
                Some(value) => {
                    request.headers_mut().insert(name.clone(), value.clone());
                }
                None => {
                    request.headers_mut().remove(name);
                }
            }
        }
        std::future::ready(request)
    })
}
