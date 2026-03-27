use super::Error;
use bytes::BytesMut;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PingReq;

impl PingReq {
    #[must_use]
    pub const fn size(&self) -> usize {
        2
    }

    pub fn write(&self, payload: &mut BytesMut) -> Result<usize, Error> {
        Ok(mqttbytes_core::ping::write_pingreq(payload))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PingResp;

impl PingResp {
    #[must_use]
    pub const fn size(&self) -> usize {
        2
    }

    pub fn write(&self, payload: &mut BytesMut) -> Result<usize, Error> {
        Ok(mqttbytes_core::ping::write_pingresp(payload))
    }
}
