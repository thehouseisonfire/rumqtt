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
