use bytes::{BufMut, BytesMut};

pub const PINGREQ_FRAME: [u8; 2] = [0xC0, 0x00];
pub const PINGRESP_FRAME: [u8; 2] = [0xD0, 0x00];

#[must_use]
pub fn write_pingreq(payload: &mut BytesMut) -> usize {
    payload.put_slice(&PINGREQ_FRAME);
    PINGREQ_FRAME.len()
}

#[must_use]
pub fn write_pingresp(payload: &mut BytesMut) -> usize {
    payload.put_slice(&PINGRESP_FRAME);
    PINGRESP_FRAME.len()
}
