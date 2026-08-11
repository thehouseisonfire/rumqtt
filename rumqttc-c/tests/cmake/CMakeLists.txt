cmake_minimum_required(VERSION 3.20)
project(rumqttc_package_smoke LANGUAGES C)

find_package(rumqttc CONFIG REQUIRED)

add_executable(rumqttc-package-smoke-static ../c/header_smoke.c)
target_link_libraries(rumqttc-package-smoke-static PRIVATE rumqttc::rumqttc_static)

add_executable(rumqttc-package-smoke-shared ../c/header_smoke.c)
target_link_libraries(rumqttc-package-smoke-shared PRIVATE rumqttc::rumqttc_shared)

add_executable(rumqttc-package-smoke-default ../c/header_smoke.c)
target_link_libraries(rumqttc-package-smoke-default PRIVATE rumqttc::rumqttc)
