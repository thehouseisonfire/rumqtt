use std::any::Any;

pub fn message(payload: &(dyn Any + Send)) -> String {
    payload.downcast_ref::<&str>().map_or_else(
        || {
            payload
                .downcast_ref::<String>()
                .cloned()
                .unwrap_or_else(|| "panic at the C ABI boundary".to_owned())
        },
        |message| (*message).to_owned(),
    )
}
