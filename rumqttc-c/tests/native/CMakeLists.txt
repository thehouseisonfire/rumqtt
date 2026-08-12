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

add_library(native_common STATIC native_common.c)
target_include_directories(native_common PUBLIC "${CMAKE_CURRENT_LIST_DIR}")
target_link_libraries(native_common PUBLIC rumqttc_native_shared)
if(MSVC)
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

add_executable(rumqttc-native-integration native_integration.c error_out_contract.c)
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

set(BROKER_RUNNER "${CMAKE_CURRENT_LIST_DIR}/broker_fixture.py")
add_test(NAME rumqttc-native-integration
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --binary $<TARGET_FILE:rumqttc-native-integration>)
set_tests_properties(rumqttc-native-integration PROPERTIES LABELS "native;integration" TIMEOUT 90)

add_test(NAME rumqttc-error-out-coverage
    COMMAND "${Python3_EXECUTABLE}" "${CMAKE_CURRENT_LIST_DIR}/check_error_out_coverage.py")
set_tests_properties(rumqttc-error-out-coverage PROPERTIES LABELS "native;compile" TIMEOUT 10)

add_test(NAME rumqttc-native-stress
    COMMAND "${Python3_EXECUTABLE}" "${BROKER_RUNNER}" --binary $<TARGET_FILE:rumqttc-native-stress>)
set_tests_properties(rumqttc-native-stress PROPERTIES LABELS "native;stress" TIMEOUT 180)

add_subdirectory("${RUMQTTC_ROOT}/examples" examples)
