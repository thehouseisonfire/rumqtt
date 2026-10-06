use rumqttc::*;
use std::ptr;

const OK: u32 = 0;
const INVALID_STATE: u32 = 2;
const BACKPRESSURE: u32 = 4;

const fn view(value: &str) -> rumqttc_string_view_t {
    rumqttc_string_view_t {
        data: value.as_ptr().cast(),
        len: value.len(),
    }
}

#[test]
fn retained_configuration_selects_execution_and_capacity_recovers_after_destroy() {
    for protocol in [1, 2] {
        unsafe {
            let mut context = ptr::null_mut();
            let mut retained = ptr::null_mut();
            let mut config = ptr::null_mut();
            let mut client = ptr::null_mut();
            let options = rumqttc_execution_options_t {
                struct_size: u32::try_from(size_of::<rumqttc_execution_options_t>()).unwrap(),
                worker_threads: 1,
                max_blocking_threads: 2,
                client_capacity: 1,
                reserved: 0,
            };
            assert_eq!(
                rumqttc_execution_context_new(
                    &raw const options,
                    &raw mut context,
                    ptr::null_mut()
                ),
                OK
            );
            assert_eq!(
                rumqttc_execution_context_retain(context, &raw mut retained, ptr::null_mut()),
                OK
            );
            assert_eq!(
                rumqttc_config_new(protocol, &raw mut config, ptr::null_mut()),
                OK
            );
            assert_eq!(
                rumqttc_config_set_broker(config, view("127.0.0.1"), 65535, ptr::null_mut()),
                OK
            );
            assert_eq!(
                rumqttc_config_set_client_id(config, view("context-test"), ptr::null_mut()),
                OK
            );
            assert_eq!(
                rumqttc_config_set_execution_context(config, context, ptr::null_mut()),
                OK
            );
            rumqttc_execution_context_release(context);
            assert_eq!(
                rumqttc_client_start(config, &raw mut client, ptr::null_mut()),
                OK
            );
            let mut rejected = ptr::null_mut();
            assert_eq!(
                rumqttc_client_start(config, &raw mut rejected, ptr::null_mut()),
                BACKPRESSURE
            );
            assert!(rejected.is_null());
            assert_eq!(
                rumqttc_client_destroy_timeout_ms(client, 5000, ptr::null_mut()),
                OK
            );
            assert_eq!(
                rumqttc_client_start(config, &raw mut client, ptr::null_mut()),
                OK
            );
            assert_eq!(
                rumqttc_execution_context_request_shutdown(retained, ptr::null_mut()),
                OK
            );
            assert_eq!(
                rumqttc_client_start(config, &raw mut rejected, ptr::null_mut()),
                INVALID_STATE
            );
            assert_eq!(
                rumqttc_execution_context_join_timeout_ms(retained, 5000, ptr::null_mut()),
                OK
            );
            let mut state = 99;
            assert_eq!(
                rumqttc_execution_context_state(retained, &raw mut state, ptr::null_mut()),
                OK
            );
            assert_eq!(state, 2);
            assert_eq!(
                rumqttc_client_destroy_timeout_ms(client, 5000, ptr::null_mut()),
                OK
            );
            assert_eq!(
                rumqttc_config_clear_execution_context(config, ptr::null_mut()),
                OK
            );
            assert_eq!(
                rumqttc_client_start(config, &raw mut client, ptr::null_mut()),
                OK
            );
            rumqttc_config_destroy(config);
            assert_eq!(
                rumqttc_client_destroy_timeout_ms(client, 5000, ptr::null_mut()),
                OK
            );
            rumqttc_execution_context_release(retained);
        }
    }
}

#[test]
fn context_errors_initialize_outputs_and_open_join_is_rejected() {
    unsafe {
        let mut context = ptr::dangling_mut::<rumqttc_execution_context>();
        let mut error = ptr::null_mut();
        let mut options = rumqttc_execution_options_t {
            struct_size: u32::try_from(size_of::<rumqttc_execution_options_t>()).unwrap(),
            worker_threads: 0,
            max_blocking_threads: 2,
            client_capacity: 1,
            reserved: 0,
        };
        assert_eq!(
            rumqttc_execution_context_new(&raw const options, &raw mut context, &raw mut error),
            3
        );
        assert!(context.is_null());
        assert!(!error.is_null());
        rumqttc_error_destroy(error);
        options.worker_threads = 1;
        options.reserved = 1;
        assert_eq!(
            rumqttc_execution_context_new(&raw const options, &raw mut context, ptr::null_mut()),
            1
        );
        assert!(context.is_null());
        assert_eq!(
            rumqttc_execution_context_new(ptr::null(), &raw mut context, ptr::null_mut()),
            OK
        );
        assert_eq!(
            rumqttc_execution_context_try_join(context, ptr::null_mut()),
            INVALID_STATE
        );
        assert_eq!(
            rumqttc_execution_context_request_shutdown(context, ptr::null_mut()),
            OK
        );
        assert_eq!(
            rumqttc_execution_context_join_timeout_ms(context, 5000, ptr::null_mut()),
            OK
        );
        assert_eq!(
            rumqttc_execution_context_try_join(context, ptr::null_mut()),
            OK
        );
        rumqttc_execution_context_release(context);
        rumqttc_execution_context_release(ptr::null_mut());
        assert_ne!(rumqttc_library_capabilities() & (1 << 16), 0);
    }
}
