use super::Error;
use bytes::BytesMut;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PingReq;

impl PingReq {
    pub fn write(payload: &mut BytesMut) -> Result<usize, Error> {
        Ok(mqttbytes_core::ping::write_pingreq(payload))
    }

    #[must_use]
    pub const fn size(&self) -> usize {
        2
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PingResp;

impl PingResp {
    pub fn write(payload: &mut BytesMut) -> Result<usize, Error> {
        Ok(mqttbytes_core::ping::write_pingresp(payload))
    }

    #[must_use]
    pub const fn size(&self) -> usize {
        2
    }
}
