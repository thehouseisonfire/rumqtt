cmake_minimum_required(VERSION 3.20)
project(rumqttc_package_smoke LANGUAGES C)
include(CTest)

find_package(rumqttc CONFIG REQUIRED)

add_executable(rumqttc-package-smoke-static ../c/header_smoke.c)
target_link_libraries(rumqttc-package-smoke-static PRIVATE rumqttc::rumqttc_static)

add_executable(rumqttc-package-smoke-shared ../c/header_smoke.c)
target_link_libraries(rumqttc-package-smoke-shared PRIVATE rumqttc::rumqttc_shared)

add_executable(rumqttc-package-smoke-default ../c/header_smoke.c)
target_link_libraries(rumqttc-package-smoke-default PRIVATE rumqttc::rumqttc)

if(WIN32)
    add_custom_command(TARGET rumqttc-package-smoke-shared POST_BUILD
        COMMAND ${CMAKE_COMMAND} -E copy_if_different
            "$<TARGET_FILE:rumqttc::rumqttc_shared>"
            "$<TARGET_FILE_DIR:rumqttc-package-smoke-shared>")
endif()

add_test(NAME rumqttc-package-smoke-static COMMAND rumqttc-package-smoke-static)
add_test(NAME rumqttc-package-smoke-shared COMMAND rumqttc-package-smoke-shared)
add_test(NAME rumqttc-package-smoke-default COMMAND rumqttc-package-smoke-default)

option(RUMQTTC_EXPECT_PROXY "Check HTTP and SOCKS5 capability in the installed package" OFF)
if(RUMQTTC_EXPECT_PROXY)
    add_executable(rumqttc-package-proxy-static ../c/proxy_capability_smoke.c)
    target_link_libraries(rumqttc-package-proxy-static PRIVATE rumqttc::rumqttc_static)
    add_executable(rumqttc-package-proxy-shared ../c/proxy_capability_smoke.c)
    target_link_libraries(rumqttc-package-proxy-shared PRIVATE rumqttc::rumqttc_shared)
    if(WIN32)
        add_custom_command(TARGET rumqttc-package-proxy-shared POST_BUILD
            COMMAND ${CMAKE_COMMAND} -E copy_if_different
                "$<TARGET_FILE:rumqttc::rumqttc_shared>"
                "$<TARGET_FILE_DIR:rumqttc-package-proxy-shared>")
    endif()
    add_test(NAME rumqttc-package-proxy-static COMMAND rumqttc-package-proxy-static)
    add_test(NAME rumqttc-package-proxy-shared COMMAND rumqttc-package-proxy-shared)
endif()

option(RUMQTTC_CHECK_FEATURES "Check the installed library's selected feature profile" OFF)
option(RUMQTTC_CHECK_PKGCONFIG "Check static and shared pkg-config consumers" OFF)
if(RUMQTTC_CHECK_PKGCONFIG)
    find_package(PkgConfig REQUIRED)
    pkg_check_modules(RUMQTTC_PC REQUIRED rumqttc)
    pkg_get_variable(RUMQTTC_PC_LIBDIR rumqttc libdir)
endif()
if(RUMQTTC_CHECK_FEATURES)
    foreach(feature RUSTLS NATIVE_TLS WEBSOCKET HTTP_PROXY SOCKS5_PROXY)
        if(NOT DEFINED RUMQTTC_EXPECT_${feature})
            message(FATAL_ERROR "RUMQTTC_EXPECT_${feature} must be set for feature checks")
        endif()
    endforeach()
    foreach(linkage static shared)
        set(target "rumqttc-package-features-${linkage}")
        add_executable(${target} ../c/feature_capability_smoke.c)
        target_link_libraries(${target} PRIVATE rumqttc::rumqttc_${linkage})
        foreach(feature RUSTLS NATIVE_TLS WEBSOCKET HTTP_PROXY SOCKS5_PROXY)
            if(RUMQTTC_EXPECT_${feature})
                target_compile_definitions(${target} PRIVATE RUMQTTC_EXPECT_${feature}=1)
            else()
                target_compile_definitions(${target} PRIVATE RUMQTTC_EXPECT_${feature}=0)
            endif()
        endforeach()
        if(WIN32 AND linkage STREQUAL "shared")
            add_custom_command(TARGET ${target} POST_BUILD
                COMMAND ${CMAKE_COMMAND} -E copy_if_different
                    "$<TARGET_FILE:rumqttc::rumqttc_shared>"
                    "$<TARGET_FILE_DIR:${target}>")
        endif()
        add_test(NAME ${target} COMMAND ${target})
        if(RUMQTTC_CHECK_PKGCONFIG)
            set(pc_target "rumqttc-pkgconfig-features-${linkage}")
            add_executable(${pc_target} ../c/feature_capability_smoke.c)
            target_include_directories(${pc_target} PRIVATE ${RUMQTTC_PC_INCLUDE_DIRS})
            target_compile_options(${pc_target} PRIVATE ${RUMQTTC_PC_CFLAGS_OTHER})
            foreach(feature RUSTLS NATIVE_TLS WEBSOCKET HTTP_PROXY SOCKS5_PROXY)
                target_compile_definitions(${pc_target} PRIVATE
                    "RUMQTTC_EXPECT_${feature}=$<BOOL:${RUMQTTC_EXPECT_${feature}}>")
            endforeach()
            if(linkage STREQUAL "static")
                target_compile_definitions(${pc_target} PRIVATE RUMQTTC_STATIC)
                set(pc_libraries ${RUMQTTC_PC_STATIC_LIBRARIES})
                list(REMOVE_ITEM pc_libraries rumqttc)
                target_link_directories(${pc_target} PRIVATE ${RUMQTTC_PC_STATIC_LIBRARY_DIRS})
                target_link_libraries(${pc_target} PRIVATE
                    "${RUMQTTC_PC_LIBDIR}/${CMAKE_STATIC_LIBRARY_PREFIX}rumqttc${CMAKE_STATIC_LIBRARY_SUFFIX}"
                    ${pc_libraries})
                target_link_options(${pc_target} PRIVATE ${RUMQTTC_PC_STATIC_LDFLAGS_OTHER})
            elseif(WIN32)
                target_link_libraries(${pc_target} PRIVATE "${RUMQTTC_PC_LIBDIR}/rumqttc.dll.lib")
                add_custom_command(TARGET ${pc_target} POST_BUILD
                    COMMAND ${CMAKE_COMMAND} -E copy_if_different
                        "${RUMQTTC_PC_LIBDIR}/rumqttc-0_1.dll" "$<TARGET_FILE_DIR:${pc_target}>")
            else()
                target_link_libraries(${pc_target} PRIVATE
                    "${RUMQTTC_PC_LIBDIR}/${CMAKE_SHARED_LIBRARY_PREFIX}rumqttc${CMAKE_SHARED_LIBRARY_SUFFIX}")
                target_link_options(${pc_target} PRIVATE ${RUMQTTC_PC_LDFLAGS_OTHER})
            endif()
            add_test(NAME ${pc_target} COMMAND ${pc_target})
        endif()
    endforeach()
endif()
