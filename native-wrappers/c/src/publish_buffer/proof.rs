use super::*;

unsafe extern "C" {
    fn publish_buffer_fixture_allocate(len: usize) -> *mut u8;
    fn publish_buffer_fixture_release(userdata: *mut c_void, data: *const u8, len: usize);
}

#[test]
fn c_owner_is_shared_and_released_once_including_empty_storage() {
    for len in [0, 1, 4096] {
        let mut calls = 0usize;
        let data = unsafe { publish_buffer_fixture_allocate(len) };
        let owner = unsafe {
            adopt(
                rumqttc_bytes_view_t { data, len },
                Some(publish_buffer_fixture_release),
                (&raw mut calls).cast(),
            )
        }
        .unwrap();
        if len != 0 {
            assert_eq!(owner.as_ptr(), data);
        }
        let cloned = owner.clone();
        drop(owner);
        assert_eq!(calls, 0);
        std::thread::spawn(move || drop(cloned)).join().unwrap();
        assert_eq!(calls, 1);
    }
}
