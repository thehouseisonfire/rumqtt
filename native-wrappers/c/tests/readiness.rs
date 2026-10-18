use rumqttc::*;
use std::ptr;

#[test]
fn notification_construction_checks_versioned_records_and_initializes_outputs() {
    unsafe {
        let mut context = ptr::dangling_mut();
        let options = rumqttc_notification_options_t {
            struct_size: 0,
            reserved: 0,
            capacity: 1,
        };
        assert_eq!(
            rumqttc_notification_context_new(&raw const options, &raw mut context, ptr::null_mut()),
            1
        );
        assert!(context.is_null());
        assert_eq!(
            rumqttc_notification_context_new(ptr::null(), ptr::null_mut(), ptr::null_mut()),
            1
        );
        let mut count = 99;
        assert_eq!(
            rumqttc_notification_collect(
                ptr::null(),
                ptr::null_mut(),
                0,
                1,
                &raw mut count,
                ptr::null_mut()
            ),
            1
        );
        assert_eq!(count, 0);
        let mut fd = 42;
        let status =
            rumqttc_notification_borrowed_posix_fd(ptr::null(), &raw mut fd, ptr::null_mut());
        assert_ne!(status, 0);
        assert_eq!(fd, -1);
        let mut handle = ptr::dangling_mut();
        assert_ne!(
            rumqttc_notification_borrowed_windows_handle(
                ptr::null(),
                &raw mut handle,
                ptr::null_mut()
            ),
            0
        );
        assert!(handle.is_null());
        rumqttc_notification_context_release(ptr::null_mut());
    }
}

#[cfg(not(feature = "readiness"))]
#[test]
fn disabled_readiness_keeps_api_exports_and_reports_capabilities_separately() {
    unsafe {
        assert_eq!(rumqttc_notification_capabilities(), 0);
        assert_eq!(rumqttc_library_capabilities() & (1 << 20), 0);
        let mut context = ptr::dangling_mut();
        assert_eq!(
            rumqttc_notification_context_new(ptr::null(), &raw mut context, ptr::null_mut()),
            3
        );
        assert!(context.is_null());
        let mut key = rumqttc_notification_key_t {
            slot: 99,
            generation: 99,
        };
        assert_eq!(
            rumqttc_notification_register_events(
                ptr::null(),
                ptr::null(),
                0,
                &raw mut key,
                ptr::null_mut()
            ),
            3
        );
        assert_eq!((key.slot, key.generation), (0, 0));
        key.slot = 99;
        key.generation = 99;
        assert_eq!(
            rumqttc_notification_register_first_connection(
                ptr::null(),
                ptr::null(),
                0,
                &raw mut key,
                ptr::null_mut()
            ),
            3
        );
        assert_eq!((key.slot, key.generation), (0, 0));
    }
}

#[cfg(feature = "readiness")]
#[test]
fn client_destruction_invalidates_events_without_releasing_context_resources() {
    unsafe {
        let mut context = ptr::null_mut();
        let mut retained = ptr::null_mut();
        let mut config = ptr::null_mut();
        let mut client = ptr::null_mut();
        let mut key = rumqttc_notification_key_t::default();
        assert_eq!(
            rumqttc_notification_context_new(ptr::null(), &raw mut context, ptr::null_mut()),
            0
        );
        assert_eq!(
            rumqttc_notification_context_retain(context, &raw mut retained, ptr::null_mut()),
            0
        );
        rumqttc_notification_context_release(context);
        assert_eq!(rumqttc_config_new(1, &raw mut config, ptr::null_mut()), 0);
        let host = rumqttc_string_view_t {
            data: c"127.0.0.1".as_ptr(),
            len: 9,
        };
        assert_eq!(
            rumqttc_config_set_broker(config, host, 65535, ptr::null_mut()),
            0
        );
        assert_eq!(
            rumqttc_client_start(config, &raw mut client, ptr::null_mut()),
            0
        );
        rumqttc_config_destroy(config);
        assert_eq!(
            rumqttc_notification_register_events(
                retained,
                client,
                7,
                &raw mut key,
                ptr::null_mut()
            ),
            0
        );
        assert_eq!(
            rumqttc_client_destroy_timeout_ms(client, 5000, ptr::null_mut()),
            0
        );
        let mut entries: [rumqttc_notification_ready_t; 2] = std::mem::zeroed();
        let mut count = 0;
        assert_eq!(
            rumqttc_notification_collect(
                retained,
                entries.as_mut_ptr(),
                size_of::<rumqttc_notification_ready_t>(),
                2,
                &raw mut count,
                ptr::null_mut()
            ),
            0
        );
        assert_eq!(count, 1);
        assert_eq!(entries[0].tag, 7);
        assert_eq!(entries[0].flags & 1, 1);
        assert_eq!(
            rumqttc_notification_acknowledge(retained, key, entries[0].revision, ptr::null_mut()),
            0
        );
        let mut ready = 99;
        assert_eq!(
            rumqttc_notification_prepare_wait(retained, &raw mut ready, ptr::null_mut()),
            0
        );
        assert_eq!(ready, 0);
        assert_eq!(
            rumqttc_notification_context_try_join(retained, ptr::null_mut()),
            11
        );
        assert_eq!(
            rumqttc_notification_context_request_close(retained, ptr::null_mut()),
            0
        );
        assert_eq!(
            rumqttc_notification_context_try_join(retained, ptr::null_mut()),
            0
        );
        rumqttc_notification_context_release(retained);
    }
}
