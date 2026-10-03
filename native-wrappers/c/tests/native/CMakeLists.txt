cmake_minimum_required(VERSION 3.20)
project(rumqttc_native_tests LANGUAGES C)

include(CTest)
find_package(Python3 REQUIRED COMPONENTS Interpreter)

set(RUMQTTC_ROOT "${CMAKE_CURRENT_LIST_DIR}/../.." CACHE PATH "rumqttc-c source directory")
set(RUMQTTC_LIBRARY_DIR "${RUMQTTC_ROOT}/../target/debug" CACHE PATH "directory containing rumqttc")

if(WIN32)
    set(RUMQTTC_SHARED_LIBRARY "${RUMQTTC_LIBRARY_DIR}/rumqttc.dll")
    set(RUMQTTC_IMPORT_LIBRARY "${RUMQTTC_LIBRARY_DIR}/rumqttc.dll.lib")
elseif(APPLE)
    set(RUMQTTC_SHARED_LIBRARY "${RUMQTTC_LIBRARY_DIR}/librumqttc.dylib")
else()
    set(RUMQTTC_SHARED_LIBRARY "${RUMQTTC_LIBRARY_DIR}/librumqttc.so")
endif()

add_library(rumqttc_native_shared SHARED IMPORTED GLOBAL)
set_target_properties(rumqttc_native_shared PROPERTIES
    IMPORTED_LOCATION "${RUMQTTC_SHARED_LIBRARY}"
    INTERFACE_INCLUDE_DIRECTORIES "${RUMQTTC_ROOT}/include")
if(WIN32)
    set_target_properties(rumqttc_native_shared PROPERTIES IMPORTED_IMPLIB "${RUMQTTC_IMPORT_LIBRARY}")
endif()

# Crypto is optional for general consumers and required in the dedicated signer CI job.
option(RUMQTTC_REQUIRE_EXTERNAL_SIGNER "Require the native EVP signer consumer" OFF)
if(RUMQTTC_REQUIRE_EXTERNAL_SIGNER)
    find_package(OpenSSL REQUIRED COMPONENTS Crypto)
else()
    find_package(OpenSSL QUIET COMPONENTS Crypto)
endif()

add_library(native_common STATIC native_common.c event_contract.c)
target_include_directories(native_common PUBLIC "${CMAKE_CURRENT_LIST_DIR}")
target_link_libraries(native_common PUBLIC rumqttc_native_shared)
if(MSVC)
    # These portable fixtures intentionally use the ISO C stdio/environment API.
    target_compile_definitions(native_common PUBLIC _CRT_SECURE_NO_WARNINGS)
    target_compile_options(native_common PRIVATE /W4 /WX)
else()
    target_compile_options(native_common PRIVATE -Wall -Wextra -Wpedantic -Werror)
endif()
if(NOT WIN32)
    find_package(Threads REQUIRED)
    target_link_libraries(native_common PUBLIC Threads::Threads)
endif()

function(rumqttc_native_target target source)
    add_executable(${target} ${source})
    target_compile_features(${target} PRIVATE c_std_11)
    target_link_libraries(${target} PRIVATE native_common)
    if(MSVC)
        target_compile_options(${target} PRIVATE /W4 /WX)
    else()
        target_compile_options(${target} PRIVATE -Wall -Wextra -Wpedantic -Werror)
        set_target_properties(${target} PROPERTIES BUILD_RPATH "${RUMQTTC_LIBRARY_DIR}")
    endif()
    if(WIN32)
        add_custom_command(TARGET ${target} POST_BUILD
            COMMAND ${CMAKE_COMMAND} -E copy_if_different
                "${RUMQTTC_SHARED_LIBRARY}" "$<TARGET_FILE_DIR:${target}>")
    endif()
endfunction()

add_executable(rumqttc-native-integration
    native_integration.c
    error_out_contract.c
    protocol_options.c)
target_compile_features(rumqttc-native-integration PRIVATE c_std_11)
target_link_libraries(rumqttc-native-integration PRIVATE native_common)
if(MSVC)
    target_compile_options(rumqttc-native-integration PRIVATE /W4 /WX)
else()
    target_compile_options(rumqttc-native-integration PRIVATE -Wall -Wextra -Wpedantic -Werror)
    set_target_properties(rumqttc-native-integration PROPERTIES BUILD_RPATH "${RUMQTTC_LIBRARY_DIR}")
endif()
if(WIN32)
    add_custom_command(TARGET rumqttc-native-integration POST_BUILD
        COMMAND ${CMAKE_COMMAND} -E copy_if_different
            "${RUMQTTC_SHARED_LIBRARY}" "$<TARGET_FILE_DIR:rumqttc-native-integration>")
endif()
rumqttc_native_target(rumqttc-native-stress native_stress.c)
rumqttc_native_target(rumqttc-native-event-queue native_event_queue.c)
rumqttc_native_target(rumqttc-native-event-properties native_event_properties.c)
rumqttc_native_target(rumqttc-native-websocket-handshake native_websocket_handshake.c)
rumqttc_native_target(rumqttc-native-auth native_auth.c)
rumqttc_native_target(rumqttc-native-auth-overlap native_auth_overlap.c)
rumqttc_native_target(rumqttc-native-auth-invalid native_auth_invalid.c)
rumqttc_native_target(rumqttc-native-auth-timeout native_auth_timeout.c)
rumqttc_native_target(rumqttc-native-auth-reject native_auth_reject.c)
rumqttc_native_target(rumqttc-native-auth-cancel native_auth_cancel.c)
rumqttc_native_target(rumqttc-native-auth-abandon native_auth_abandon.c)
rumqttc_native_target(rumqttc-native-auth-reconnect native_auth_reconnect.c)
rumqttc_native_target(rumqttc-native-scram native_scram.c)
rumqttc_native_target(rumqttc-native-srv-redirect native_srv_redirect.c)
rumqttc_native_target(rumqttc-native-srv-candidates native_srv_candidates.c)
rumqttc_native_target(rumqttc-native-redirect-matrix native_redirect_matrix.c)
rumqttc_native_target(rumqttc-native-srv-failure native_srv_failure.c)
rumqttc_native_target(rumqttc-native-srv-cancel native_srv_cancel.c)
rumqttc_native_target(rumqttc-native-redirect-reject-loop native_redirect_reject_loop.c)
rumqttc_native_target(rumqttc-native-proxy native_proxy.c)
rumqttc_native_target(rumqttc-native-proxy-matrix native_proxy_matrix.c)
rumqttc_native_target(rumqttc-native-store-corrupt native_store_corrupt.c)
rumqttc_native_target(rumqttc-native-store-cancel native_store_cancel.c)
rumqttc_native_target(rumqttc-native-store-restart native_store_restart.c)
rumqttc_native_target(rumqttc-native-store-ownership native_store_ownership.c)
rumqttc_native_target(rumqttc-native-store-atomic native_store_atomic.c)
rumqttc_native_target(rumqttc-native-store-write-failure native_store_write_failure.c)
rumqttc_native_target(rumqttc-native-wire-options native_wire_options.c)
rumqttc_native_target(rumqttc-native-runtime-limits native_runtime_limits.c)
rumqttc_native_target(rumqttc-native-runtime-batching native_runtime_batching.c)
rumqttc_native_target(rumqttc-native-tls-matrix native_tls_matrix.c)
rumqttc_native_target(rumqttc-native-tls-profiles native_tls_profiles.c)
rumqttc_native_target(rumqttc-native-callback-races native_callback_races.c)
rumqttc_native_target(rumqttc-native-network-options native_network_options.c)
if(WIN32)
    target_link_libraries(rumqttc-native-network-options PRIVATE ws2_32 kernel32)
endif()
rumqttc_native_target(rumqttc-native-close-options native_close_options.c)
rumqttc_native_target(rumqttc-native-will-process native_will_process.c)

# This executable must not link the imported library or native_common: the
# final FreeLibrary/dlclose is part of its ownership assertion.
add_executable(rumqttc-native-unload native_unload.c)
target_compile_features(rumqttc-native-unload PRIVATE c_std_11)
target_include_directories(rumqttc-native-unload PRIVATE "${RUMQTTC_ROOT}/include")
target_link_libraries(rumqttc-native-unload PRIVATE ${CMAKE_DL_LIBS})
if(MSVC)
    target_compile_options(rumqttc-native-unload PRIVATE /W4 /WX /experimental:c11atomics)
else()
    target_compile_options(rumqttc-native-unload PRIVATE -Wall -Wextra -Wpedantic -Werror)
endif()
if(MSVC)
    target_compile_options(rumqttc-native-auth PRIVATE /experimental:c11atomics)
    target_compile_options(rumqttc-native-websocket-handshake PRIVATE /experimental:c11atomics)
    target_compile_options(rumqttc-native-auth-overlap PRIVATE /experimental:c11atomics)
    target_compile_options(rumqttc-native-auth-invalid PRIVATE /experimental:c11atomics)
    target_compile_options(rumqttc-native-auth-timeout PRIVATE /experimental:c11atomics)
    target_compile_options(rumqttc-native-auth-reject PRIVATE /experimental:c11atomics)
    target_compile_options(rumqttc-native-auth-cancel PRIVATE /experimental:c11atomics)
    target_compile_options(rumqttc-native-auth-abandon PRIVATE /experimental:c11atomics)
    target_compile_options(rumqttc-native-auth-reconnect PRIVATE /experimental:c11atomics)
    target_compile_options(rumqttc-native-srv-redirect PRIVATE /experimental:c11atomics)
    target_compile_options(rumqttc-native-redirect-matrix PRIVATE /experimental:c11atomics)
    target_compile_options(rumqttc-native-srv-failure PRIVATE /experimental:c11atomics)
    target_compile_options(rumqttc-native-srv-cancel PRIVATE /experimental:c11atomics)
    target_compile_options(rumqttc-native-store-corrupt PRIVATE /experimental:c11atomics)
    target_compile_options(rumqttc-native-store-cancel PRIVATE /experimental:c11atomics)
    target_compile_options(rumqttc-native-store-restart PRIVATE /experimental:c11atomics)
    target_compile_options(rumqttc-native-store-ownership PRIVATE /experimental:c11atomics)
    target_compile_options(rumqttc-native-store-write-failure PRIVATE /experimental:c11atomics)
    target_compile_options(rumqttc-native-callback-races PRIVATE /experimental:c11atomics)
    target_compile_options(rumqttc-native-close-options PRIVATE /experimental:c11atomics)
endif()

set(BROKER_RUNNER "${CMAKE_CURRENT_LIST_DIR}/broker_fixture.py")
add_test(NAME rumqttc-native-integration
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --binary $<TARGET_FILE:rumqttc-native-integration>)
set_tests_properties(rumqttc-native-integration PROPERTIES LABELS "native;integration" TIMEOUT 90)

add_test(NAME rumqttc-error-out-coverage
    COMMAND "${Python3_EXECUTABLE}" "${CMAKE_CURRENT_LIST_DIR}/check_error_out_coverage.py")
set_tests_properties(rumqttc-error-out-coverage PROPERTIES LABELS "native;compile" TIMEOUT 10)

add_test(NAME rumqttc-platform-trust-fixture
    COMMAND "${Python3_EXECUTABLE}" -m unittest discover
        -s "${CMAKE_CURRENT_LIST_DIR}" -p test_platform_trust.py)
set_tests_properties(rumqttc-platform-trust-fixture PROPERTIES LABELS "native;tls;fixture" TIMEOUT 15)

add_test(NAME rumqttc-native-stress
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --binary $<TARGET_FILE:rumqttc-native-stress>)
set_tests_properties(rumqttc-native-stress PROPERTIES LABELS "native;stress" TIMEOUT 180)

add_test(NAME rumqttc-native-auth
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --binary $<TARGET_FILE:rumqttc-native-auth>)
set_tests_properties(rumqttc-native-auth PROPERTIES LABELS "native;authentication" TIMEOUT 90)

add_test(NAME rumqttc-native-auth-overlap
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --binary $<TARGET_FILE:rumqttc-native-auth-overlap>)
set_tests_properties(rumqttc-native-auth-overlap PROPERTIES LABELS "native;authentication" TIMEOUT 90)

add_test(NAME rumqttc-native-auth-invalid
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --redact --binary $<TARGET_FILE:rumqttc-native-auth-invalid>)
set_tests_properties(rumqttc-native-auth-invalid PROPERTIES LABELS "native;authentication" TIMEOUT 90)

add_test(NAME rumqttc-native-auth-timeout
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --binary $<TARGET_FILE:rumqttc-native-auth-timeout>)
set_tests_properties(rumqttc-native-auth-timeout PROPERTIES LABELS "native;authentication" TIMEOUT 90)

add_test(NAME rumqttc-native-auth-reject
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --binary $<TARGET_FILE:rumqttc-native-auth-reject>)
set_tests_properties(rumqttc-native-auth-reject PROPERTIES LABELS "native;authentication" TIMEOUT 90)

add_test(NAME rumqttc-native-auth-cancel
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --binary $<TARGET_FILE:rumqttc-native-auth-cancel>)
set_tests_properties(rumqttc-native-auth-cancel PROPERTIES LABELS "native;authentication" TIMEOUT 90)

add_test(NAME rumqttc-native-auth-abandon
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --binary $<TARGET_FILE:rumqttc-native-auth-abandon>)
set_tests_properties(rumqttc-native-auth-abandon PROPERTIES LABELS "native;authentication" TIMEOUT 90)

add_test(NAME rumqttc-native-auth-reconnect
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --binary $<TARGET_FILE:rumqttc-native-auth-reconnect>)
set_tests_properties(rumqttc-native-auth-reconnect PROPERTIES LABELS "native;authentication" TIMEOUT 90)

add_test(NAME rumqttc-native-scram
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --redact --binary $<TARGET_FILE:rumqttc-native-scram>)
set_tests_properties(rumqttc-native-scram PROPERTIES LABELS "native;authentication" TIMEOUT 90)

add_test(NAME rumqttc-native-srv-redirect
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --binary $<TARGET_FILE:rumqttc-native-srv-redirect>)
set_tests_properties(rumqttc-native-srv-redirect PROPERTIES LABELS "native;redirect" TIMEOUT 90)

add_test(NAME rumqttc-native-redirect-matrix
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --binary $<TARGET_FILE:rumqttc-native-redirect-matrix>)
set_tests_properties(rumqttc-native-redirect-matrix PROPERTIES LABELS "native;redirect;persistence" TIMEOUT 90)

add_test(NAME rumqttc-native-srv-failure
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --binary $<TARGET_FILE:rumqttc-native-srv-failure>)
set_tests_properties(rumqttc-native-srv-failure PROPERTIES LABELS "native;redirect" TIMEOUT 90)

add_test(NAME rumqttc-native-srv-cancel
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --binary $<TARGET_FILE:rumqttc-native-srv-cancel>)
set_tests_properties(rumqttc-native-srv-cancel PROPERTIES LABELS "native;redirect" TIMEOUT 90)

add_test(NAME rumqttc-native-redirect-reject-loop
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --binary $<TARGET_FILE:rumqttc-native-redirect-reject-loop>)
set_tests_properties(rumqttc-native-redirect-reject-loop PROPERTIES LABELS "native;redirect" TIMEOUT 90)

add_test(NAME rumqttc-native-proxy
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --redact --binary $<TARGET_FILE:rumqttc-native-proxy>)
set_tests_properties(rumqttc-native-proxy PROPERTIES LABELS "native;proxy" TIMEOUT 90)

add_test(NAME rumqttc-native-proxy-matrix
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --redact --proxy-matrix
        --binary $<TARGET_FILE:rumqttc-native-proxy-matrix>)
set_tests_properties(rumqttc-native-proxy-matrix PROPERTIES LABELS "native;proxy;tls" TIMEOUT 90)

add_test(NAME rumqttc-native-store-corrupt
    COMMAND $<TARGET_FILE:rumqttc-native-store-corrupt>)
set_tests_properties(rumqttc-native-store-corrupt PROPERTIES LABELS "native;persistence" TIMEOUT 90)

add_test(NAME rumqttc-native-store-cancel
    COMMAND $<TARGET_FILE:rumqttc-native-store-cancel>)
set_tests_properties(rumqttc-native-store-cancel PROPERTIES LABELS "native;persistence" TIMEOUT 90)

add_test(NAME rumqttc-native-store-restart
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --binary $<TARGET_FILE:rumqttc-native-store-restart>)
set_tests_properties(rumqttc-native-store-restart PROPERTIES LABELS "native;persistence" TIMEOUT 90)

add_test(NAME rumqttc-native-store-ownership
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --binary $<TARGET_FILE:rumqttc-native-store-ownership>)
set_tests_properties(rumqttc-native-store-ownership PROPERTIES LABELS "native;persistence;ownership" TIMEOUT 90)

add_test(NAME rumqttc-native-store-atomic
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --atomic-restart --binary $<TARGET_FILE:rumqttc-native-store-atomic>)
set_tests_properties(rumqttc-native-store-atomic PROPERTIES LABELS "native;persistence" TIMEOUT 90)

add_test(NAME rumqttc-native-store-write-failure
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --binary $<TARGET_FILE:rumqttc-native-store-write-failure>)
set_tests_properties(rumqttc-native-store-write-failure PROPERTIES LABELS "native;persistence" TIMEOUT 90)

add_test(NAME rumqttc-native-unload
    COMMAND $<TARGET_FILE:rumqttc-native-unload> "${RUMQTTC_SHARED_LIBRARY}")
set_tests_properties(rumqttc-native-unload PROPERTIES LABELS "native;ownership" TIMEOUT 30)

add_test(NAME rumqttc-native-wire-options
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --binary $<TARGET_FILE:rumqttc-native-wire-options>)
set_tests_properties(rumqttc-native-wire-options PROPERTIES LABELS "native;wire" TIMEOUT 90)

add_test(NAME rumqttc-native-tls-matrix
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --redact --tls-matrix
        --binary $<TARGET_FILE:rumqttc-native-tls-matrix>)
set_tests_properties(rumqttc-native-tls-matrix PROPERTIES LABELS "native;tls" TIMEOUT 90)
add_test(NAME rumqttc-native-tls-profiles
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --redact --tls-matrix --tls-profiles --proxy-matrix
        --binary $<TARGET_FILE:rumqttc-native-tls-profiles>)
set_tests_properties(rumqttc-native-tls-profiles PROPERTIES LABELS "native;tls" TIMEOUT 120)

add_test(NAME rumqttc-native-callback-races
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --binary $<TARGET_FILE:rumqttc-native-callback-races>)
set_tests_properties(rumqttc-native-callback-races PROPERTIES LABELS "native;ownership;stress" TIMEOUT 90)

add_test(NAME rumqttc-native-network-options
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --redact --network-matrix
        --binary $<TARGET_FILE:rumqttc-native-network-options>)
set_tests_properties(rumqttc-native-network-options PROPERTIES LABELS "native;network;websocket;unix" TIMEOUT 90)

add_test(NAME rumqttc-native-close-options
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --binary $<TARGET_FILE:rumqttc-native-close-options>)
set_tests_properties(rumqttc-native-close-options PROPERTIES LABELS "native;shutdown;stress" TIMEOUT 90)

add_test(NAME rumqttc-native-will-process
    COMMAND "${Python3_EXECUTABLE}" "${CMAKE_CURRENT_LIST_DIR}/will_fixture.py"
        --binary $<TARGET_FILE:rumqttc-native-will-process>)
set_tests_properties(rumqttc-native-will-process PROPERTIES LABELS "native;will" TIMEOUT 90 SKIP_RETURN_CODE 77)

add_test(NAME rumqttc-native-runtime-limits
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --binary $<TARGET_FILE:rumqttc-native-runtime-limits>)
set_tests_properties(rumqttc-native-runtime-limits PROPERTIES LABELS "native;wire" TIMEOUT 90)

add_test(NAME rumqttc-native-runtime-batching
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --binary $<TARGET_FILE:rumqttc-native-runtime-batching>)
set_tests_properties(rumqttc-native-runtime-batching PROPERTIES LABELS "native;wire" TIMEOUT 90)

add_test(NAME rumqttc-native-srv-candidates
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --binary $<TARGET_FILE:rumqttc-native-srv-candidates>)
set_tests_properties(rumqttc-native-srv-candidates PROPERTIES LABELS "native;redirect" TIMEOUT 90)

add_test(NAME rumqttc-native-event-queue
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --binary $<TARGET_FILE:rumqttc-native-event-queue>)
set_tests_properties(rumqttc-native-event-queue PROPERTIES LABELS "native;events;stress" TIMEOUT 90)

add_test(NAME rumqttc-native-event-properties
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --binary $<TARGET_FILE:rumqttc-native-event-properties>)
set_tests_properties(rumqttc-native-event-properties PROPERTIES LABELS "native;events" TIMEOUT 90)

add_subdirectory("${RUMQTTC_ROOT}/examples" examples)

rumqttc_native_target(rumqttc-native-custom-transport custom_transport.c)
if(MSVC)
    target_compile_options(rumqttc-native-custom-transport PRIVATE /experimental:c11atomics)
endif()
add_test(NAME rumqttc-native-custom-transport COMMAND rumqttc-native-custom-transport)
set_tests_properties(rumqttc-native-custom-transport PROPERTIES LABELS "native;transport;ownership" TIMEOUT 30)
rumqttc_native_target(rumqttc-native-custom-transport-matrix custom_transport_matrix.c)
target_link_libraries(rumqttc-native-custom-transport-matrix PRIVATE rumqttc_transport_socket)
add_test(NAME rumqttc-native-custom-transport-matrix
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --custom-transport --proxy-matrix
        --binary $<TARGET_FILE:rumqttc-native-custom-transport-matrix>)
set_tests_properties(rumqttc-native-custom-transport-matrix PROPERTIES LABELS "native;transport;tls;proxy;websocket" TIMEOUT 120)

add_test(NAME rumqttc-native-websocket-handshake COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --binary $<TARGET_FILE:rumqttc-native-websocket-handshake>)
set_tests_properties(rumqttc-native-websocket-handshake PROPERTIES LABELS "native;websocket" TIMEOUT 60)

rumqttc_native_target(rumqttc-native-acknowledgement-options native_acknowledgement_options.c)
add_test(NAME rumqttc-native-acknowledgement-options
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --binary $<TARGET_FILE:rumqttc-native-acknowledgement-options>)
set_tests_properties(rumqttc-native-acknowledgement-options PROPERTIES LABELS "native;wire;ownership" TIMEOUT 30)

if(OpenSSL_FOUND)
    rumqttc_native_target(rumqttc-native-tls-advanced native_tls_advanced.c)
    target_sources(rumqttc-native-tls-advanced PRIVATE "${RUMQTTC_ROOT}/examples/external_signer.c")
    target_include_directories(rumqttc-native-tls-advanced PRIVATE "${RUMQTTC_ROOT}/examples")
    target_link_libraries(rumqttc-native-tls-advanced PRIVATE OpenSSL::Crypto)
    if(MSVC)
        target_compile_options(rumqttc-native-tls-advanced PRIVATE /experimental:c11atomics)
    endif()
    add_test(NAME rumqttc-native-tls-advanced
        COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --redact --tls-advanced --proxy-matrix
            --binary $<TARGET_FILE:rumqttc-native-tls-advanced>)
    set_tests_properties(rumqttc-native-tls-advanced PROPERTIES LABELS "native;tls" TIMEOUT 120 SKIP_RETURN_CODE 77)
endif()
