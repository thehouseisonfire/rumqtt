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
    target_compile_options(rumqttc-thread-join-contract PRIVATE /W4 /WX)
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
    event_polling
    multithreaded_publishing
    tracked_completion
    manual_acknowledgement
    shutdown)

foreach(example IN LISTS RUMQTTC_C_EXAMPLES)
    set(target "rumqttc-example-${example}")
    add_executable(${target} "${example}.c")
    target_compile_features(${target} PRIVATE c_std_11)
    target_link_libraries(${target} PRIVATE rumqttc_example_common)
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
    add_test(NAME ${target}
        COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --binary $<TARGET_FILE:${target}>)
    set_tests_properties(${target} PROPERTIES LABELS "native;example" TIMEOUT 30)
endforeach()
