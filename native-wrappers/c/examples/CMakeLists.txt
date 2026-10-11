add_library(rumqttc_example_common STATIC example_common.c)
target_compile_features(rumqttc_example_common PUBLIC c_std_11)
target_include_directories(rumqttc_example_common PUBLIC "${CMAKE_CURRENT_LIST_DIR}")
target_link_libraries(rumqttc_example_common PUBLIC rumqttc_native_shared)
if(MSVC)
    target_compile_options(rumqttc_example_common PRIVATE /W4 /WX)
else()
    target_compile_options(rumqttc_example_common PRIVATE -Wall -Wextra -Wpedantic -Werror)
endif()
if(NOT WIN32)
    target_link_libraries(rumqttc_example_common PUBLIC Threads::Threads)
endif()

add_executable(rumqttc-thread-join-contract
    "${RUMQTTC_ROOT}/tests/native/thread_join_contract.c")
target_compile_features(rumqttc-thread-join-contract PRIVATE c_std_11)
target_link_libraries(rumqttc-thread-join-contract PRIVATE rumqttc_example_common)
if(MSVC)
    target_compile_options(rumqttc-thread-join-contract PRIVATE /W4 /WX /experimental:c11atomics)
else()
    target_compile_options(rumqttc-thread-join-contract PRIVATE -Wall -Wextra -Wpedantic -Werror)
    set_target_properties(rumqttc-thread-join-contract PROPERTIES BUILD_RPATH "${RUMQTTC_LIBRARY_DIR}")
endif()
if(WIN32)
    add_custom_command(TARGET rumqttc-thread-join-contract POST_BUILD
        COMMAND ${CMAKE_COMMAND} -E copy_if_different
            "${RUMQTTC_SHARED_LIBRARY}" "$<TARGET_FILE_DIR:rumqttc-thread-join-contract>")
endif()
add_test(NAME rumqttc-thread-join-contract COMMAND rumqttc-thread-join-contract)
set_tests_properties(rumqttc-thread-join-contract PROPERTIES LABELS "native;concurrency" TIMEOUT 10)

set(RUMQTTC_C_EXAMPLES
    websocket_tokens
    event_polling
    reconnect
    session_recovery
    diagnostics
    multithreaded_publishing
    tracked_completion
    offline_publishing
    manual_acknowledgement
    session_store
    authenticator
    shutdown
    ordered_shutdown)
list(APPEND RUMQTTC_C_EXAMPLES shared_execution resource_limits custom_transport tls_profile redirect_authority configuration_rotation)
add_library(rumqttc_transport_socket STATIC transport_socket.c)
target_compile_features(rumqttc_transport_socket PUBLIC c_std_11)
target_link_libraries(rumqttc_transport_socket PUBLIC rumqttc_example_common)
if(MSVC)
    target_compile_options(rumqttc_transport_socket PRIVATE /W4 /WX /experimental:c11atomics)
    target_link_libraries(rumqttc_transport_socket PUBLIC ws2_32)
else()
    target_compile_options(rumqttc_transport_socket PRIVATE -Wall -Wextra -Wpedantic -Werror)
endif()

foreach(example IN LISTS RUMQTTC_C_EXAMPLES)
    set(target "rumqttc-example-${example}")
    add_executable(${target} "${example}.c")
    target_compile_features(${target} PRIVATE c_std_11)
    target_link_libraries(${target} PRIVATE rumqttc_example_common)
    if(MSVC)
        target_compile_options(${target} PRIVATE /W4 /WX)
        if(example STREQUAL "tls_profile" OR example STREQUAL "configuration_rotation")
            target_compile_definitions(${target} PRIVATE _CRT_SECURE_NO_WARNINGS)
        endif()
        if(example STREQUAL "multithreaded_publishing" OR example STREQUAL "authenticator"
                OR example STREQUAL "session_store" OR example STREQUAL "websocket_tokens")
            target_compile_options(${target} PRIVATE /experimental:c11atomics)
        endif()
    else()
        target_compile_options(${target} PRIVATE -Wall -Wextra -Wpedantic -Werror)
        set_target_properties(${target} PROPERTIES BUILD_RPATH "${RUMQTTC_LIBRARY_DIR}")
    endif()
    if(WIN32)
        add_custom_command(TARGET ${target} POST_BUILD
            COMMAND ${CMAKE_COMMAND} -E copy_if_different
                "${RUMQTTC_SHARED_LIBRARY}" "$<TARGET_FILE_DIR:${target}>")
    endif()
    if(example STREQUAL "configuration_rotation")
        add_test(NAME ${target} COMMAND "${Python3_EXECUTABLE}"
            "${RUMQTTC_ROOT}/tests/native/configuration_rotation_fixture.py" --binary $<TARGET_FILE:${target}>)
    elseif(example STREQUAL "tls_profile")
        add_test(NAME ${target} COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}"
            --redact --tls-profile-example --binary $<TARGET_FILE:${target}>)
        set_tests_properties(${target} PROPERTIES SKIP_RETURN_CODE 77)
    elseif(example STREQUAL "custom_transport")
        target_link_libraries(${target} PRIVATE rumqttc_transport_socket)
        add_test(NAME ${target} COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}"
            --custom-transport --binary $<TARGET_FILE:${target}>)
    else()
        add_test(NAME ${target}
            COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --binary $<TARGET_FILE:${target}>)
    endif()
    set_tests_properties(${target} PROPERTIES LABELS "native;example" TIMEOUT 30)
    if(example STREQUAL "websocket_tokens" OR example STREQUAL "ordered_shutdown")
        set_tests_properties(${target} PROPERTIES SKIP_RETURN_CODE 77)
    endif()
endforeach()

if(OpenSSL_FOUND)
    foreach(signer_example IN ITEMS external_identity external_identity_deferred)
        set(signer_target "rumqttc-example-${signer_example}")
        add_executable(${signer_target} external_identity.c external_signer.c)
        if(signer_example STREQUAL "external_identity_deferred")
            target_compile_definitions(${signer_target} PRIVATE RUMQTTC_EXAMPLE_DEFERRED)
            if(MSVC)
                target_compile_options(${signer_target} PRIVATE /experimental:c11atomics)
            endif()
        endif()
        target_compile_features(${signer_target} PRIVATE c_std_11)
        target_link_libraries(${signer_target} PRIVATE rumqttc_example_common OpenSSL::Crypto)
        if(MSVC)
            target_compile_definitions(${signer_target} PRIVATE _CRT_SECURE_NO_WARNINGS)
            target_compile_options(${signer_target} PRIVATE /W4 /WX)
        else()
            target_compile_options(${signer_target} PRIVATE -Wall -Wextra -Wpedantic -Werror)
            set_target_properties(${signer_target} PROPERTIES BUILD_RPATH "${RUMQTTC_LIBRARY_DIR}")
        endif()
        if(WIN32)
            add_custom_command(TARGET ${signer_target} POST_BUILD
                COMMAND ${CMAKE_COMMAND} -E copy_if_different "${RUMQTTC_SHARED_LIBRARY}" "$<TARGET_FILE_DIR:${signer_target}>")
        endif()
        add_test(NAME ${signer_target}
            COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --redact --external-identity-example
                --binary $<TARGET_FILE:${signer_target}>)
        set_tests_properties(${signer_target} PROPERTIES LABELS "native;example;tls" TIMEOUT 30 SKIP_RETURN_CODE 77)
    endforeach()
endif()
