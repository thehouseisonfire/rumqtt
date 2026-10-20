use rumqttc::*;
use std::ptr;
fn error_drop(error: &mut *mut rumqttc_error) {
    unsafe { rumqttc_error_destroy(*error) };
    *error = ptr::null_mut();
}
#[test]
fn disabled_feature_has_initialized_outputs_and_stable_exports() {
    if cfg!(feature = "file-session-store") {
        return;
    }
    unsafe {
        let mut out = ptr::dangling_mut();
        let mut error = ptr::null_mut();
        assert_eq!(
            rumqttc_file_store_context_new(ptr::null(), &raw mut out, &raw mut error),
            3
        );
        assert!(out.is_null());
        error_drop(&mut error);
        let mut state = 999;
        assert_eq!(
            rumqttc_file_store_operation_poll(ptr::null(), &raw mut state, &raw mut error),
            3
        );
        assert_eq!(state, 0);
        error_drop(&mut error);
        assert_eq!(rumqttc_library_capabilities() & (1 << 22), 0);
    }
}
#[cfg(feature = "file-session-store")]
fn wait(op: *const rumqttc_file_store_operation) {
    unsafe {
        let mut state = 0;
        let mut error = ptr::null_mut();
        let status =
            rumqttc_file_store_operation_wait_timeout_ms(op, 5000, &raw mut state, &raw mut error);
        error_drop(&mut error);
        assert_eq!(status, 0);
        assert_eq!(state, 2);
    }
}
#[cfg(feature = "file-session-store")]
#[test]
fn retained_operations_and_duplicate_opens_preserve_typed_results() {
    unsafe {
        let root = tempfile::tempdir().unwrap();
        #[cfg(target_os = "linux")]
        let root_path = {
            use std::os::unix::ffi::OsStrExt;
            let path = root
                .path()
                .join(std::ffi::OsStr::from_bytes(b"native-path-\xff"));
            std::fs::create_dir(&path).unwrap();
            path
        };
        #[cfg(not(target_os = "linux"))]
        let root_path = root.path().to_owned();
        #[cfg(unix)]
        let root_bytes = {
            use std::os::unix::ffi::OsStrExt;
            root_path.as_os_str().as_bytes().to_vec()
        };
        #[cfg(windows)]
        let root_bytes = {
            use std::os::windows::ffi::OsStrExt;
            root_path
                .as_os_str()
                .encode_wide()
                .flat_map(u16::to_ne_bytes)
                .collect::<Vec<_>>()
        };
        let path = rumqttc_path_view_t {
            data: root_bytes.as_ptr().cast(),
            len: root_bytes.len() / if cfg!(windows) { 2 } else { 1 },
            encoding: if cfg!(windows) { 2 } else { 1 },
            reserved: 0,
        };
        let mut context = ptr::null_mut();
        let mut op = ptr::null_mut();
        let mut store = ptr::null_mut();
        let mut error = ptr::null_mut();
        assert_eq!(
            rumqttc_file_store_context_new(ptr::null(), &raw mut context, &raw mut error),
            0
        );
        assert_eq!(
            rumqttc_file_store_begin_open(
                context,
                1,
                path,
                ptr::null(),
                &raw mut op,
                &raw mut error
            ),
            0
        );
        wait(op);
        assert_eq!(
            rumqttc_file_store_operation_get_store(op, &raw mut store, &raw mut error),
            0
        );
        rumqttc_file_store_operation_release(op);
        assert_eq!(
            rumqttc_file_store_begin_open(
                context,
                1,
                path,
                ptr::null(),
                &raw mut op,
                &raw mut error
            ),
            0
        );
        let mut state = 0;
        assert_eq!(
            rumqttc_file_store_operation_wait_timeout_ms(op, 5000, &raw mut state, &raw mut error),
            2
        );
        assert_eq!(state, 3);
        let mut evidence: rumqttc_file_store_error_details_t = std::mem::zeroed();
        evidence.struct_size = std::mem::size_of_val(&evidence).try_into().unwrap();
        assert_eq!(
            rumqttc_error_file_store_details(error, &raw mut evidence),
            0
        );
        assert_eq!(evidence.kind, 2);
        error_drop(&mut error);
        rumqttc_file_store_operation_release(op);
        let scope = rumqttc_string_view_t {
            data: b"scope".as_ptr().cast(),
            len: 5,
        };
        let id = rumqttc_string_view_t {
            data: b"client".as_ptr().cast(),
            len: 6,
        };
        assert_eq!(
            rumqttc_file_store_inspect(store, scope, id, &raw mut op, &raw mut error),
            0
        );
        wait(op);
        let mut result: rumqttc_file_store_result_t = std::mem::zeroed();
        result.struct_size = std::mem::size_of_val(&result).try_into().unwrap();
        assert_eq!(
            rumqttc_file_store_operation_result(op, &raw mut result, &raw mut error),
            0
        );
        assert_eq!(result.kind, 1);
        assert_eq!(result.present, 0);
        rumqttc_file_store_operation_release(op);
        assert_eq!(
            rumqttc_file_store_close(store, &raw mut op, &raw mut error),
            0
        );
        wait(op);
        assert_eq!(
            rumqttc_file_store_context_request_shutdown(context, &raw mut error),
            0
        );
        assert_eq!(
            rumqttc_file_store_context_join_timeout_ms(context, 5000, &raw mut error),
            0
        );
        // Terminal observers remain readable after context quiescence.
        wait(op);
        rumqttc_file_store_operation_release(op);
        rumqttc_file_store_release(store);
        rumqttc_file_store_context_release(context);
    }
}
