cmake_minimum_required(VERSION 3.20)
project(rumqttc VERSION 0.1.0 LANGUAGES C)

set(RUMQTTC_PACKAGE_VERSION "0.1.0-alpha")
set(RUMQTTC_ABI_LINE "0.1")
string(REPLACE "." "_" RUMQTTC_ABI_FILE_LINE "${RUMQTTC_ABI_LINE}")
set(RUMQTTC_SHARED_UNVERSIONED_NAME
    "${CMAKE_SHARED_LIBRARY_PREFIX}rumqttc${CMAKE_SHARED_LIBRARY_SUFFIX}")

include(CMakePackageConfigHelpers)
include(GNUInstallDirs)

install(FILES include/rumqttc.h DESTINATION ${CMAKE_INSTALL_INCLUDEDIR})
install(FILES
    "${CMAKE_CURRENT_LIST_DIR}/../target/release/${CMAKE_STATIC_LIBRARY_PREFIX}rumqttc${CMAKE_STATIC_LIBRARY_SUFFIX}"
    DESTINATION ${CMAKE_INSTALL_LIBDIR} OPTIONAL)
if(WIN32)
    set(RUMQTTC_SHARED_INSTALL_NAME "rumqttc-${RUMQTTC_ABI_FILE_LINE}.dll")
    set(RUMQTTC_PKGCONFIG_LIBS_PRIVATE
        "-lws2_32 -lbcrypt -lcrypt32 -lncrypt -lsecur32 -luserenv -ladvapi32 -lkernel32 -lntdll")
    set(RUMQTTC_CMAKE_STATIC_LINK_LIBRARIES
        "ws2_32;bcrypt;crypt32;ncrypt;secur32;userenv;advapi32;kernel32;ntdll")
    install(FILES
        "${CMAKE_CURRENT_LIST_DIR}/../target/release/${CMAKE_IMPORT_LIBRARY_PREFIX}rumqttc${CMAKE_SHARED_LIBRARY_SUFFIX}${CMAKE_IMPORT_LIBRARY_SUFFIX}"
        DESTINATION ${CMAKE_INSTALL_LIBDIR})
elseif(APPLE)
    set(RUMQTTC_SHARED_INSTALL_NAME "librumqttc.${RUMQTTC_ABI_LINE}.dylib")
    set(RUMQTTC_PKGCONFIG_LIBS_PRIVATE
        "-lpthread -lm -framework Security -framework CoreFoundation -framework SystemConfiguration")
    set(RUMQTTC_CMAKE_STATIC_LINK_LIBRARIES
        "pthread;m;-framework Security;-framework CoreFoundation;-framework SystemConfiguration")
elseif(CMAKE_SYSTEM_NAME STREQUAL "Linux")
    set(RUMQTTC_SHARED_INSTALL_NAME "librumqttc.so.${RUMQTTC_ABI_LINE}")
    set(RUMQTTC_PKGCONFIG_LIBS_PRIVATE "-lpthread -ldl -lm")
    set(RUMQTTC_CMAKE_STATIC_LINK_LIBRARIES "pthread;dl;m")
else()
    set(RUMQTTC_SHARED_INSTALL_NAME "${RUMQTTC_SHARED_UNVERSIONED_NAME}")
    set(RUMQTTC_PKGCONFIG_LIBS_PRIVATE "")
    set(RUMQTTC_CMAKE_STATIC_LINK_LIBRARIES "")
endif()

install(FILES
    "${CMAKE_CURRENT_LIST_DIR}/../target/release/${CMAKE_SHARED_LIBRARY_PREFIX}rumqttc${CMAKE_SHARED_LIBRARY_SUFFIX}"
    TYPE LIB RENAME "${RUMQTTC_SHARED_INSTALL_NAME}" OPTIONAL)
install(FILES "${CMAKE_CURRENT_LIST_DIR}/../target/release/rumqttc-abi-contract.json"
    DESTINATION "${CMAKE_INSTALL_DATADIR}/rumqttc" RENAME "abi-contract.json" OPTIONAL)
if(UNIX AND NOT RUMQTTC_SHARED_INSTALL_NAME STREQUAL RUMQTTC_SHARED_UNVERSIONED_NAME)
    install(CODE "execute_process(COMMAND \"${CMAKE_COMMAND}\" -E create_symlink
        \"${RUMQTTC_SHARED_INSTALL_NAME}\"
        \"\$ENV{DESTDIR}\${CMAKE_INSTALL_PREFIX}/${CMAKE_INSTALL_LIBDIR}/${RUMQTTC_SHARED_UNVERSIONED_NAME}\")")
endif()

add_library(rumqttc_static STATIC IMPORTED GLOBAL)
set_target_properties(rumqttc_static PROPERTIES
    IMPORTED_LOCATION "${CMAKE_CURRENT_LIST_DIR}/../target/release/${CMAKE_STATIC_LIBRARY_PREFIX}rumqttc${CMAKE_STATIC_LIBRARY_SUFFIX}"
    INTERFACE_COMPILE_DEFINITIONS RUMQTTC_STATIC
    INTERFACE_INCLUDE_DIRECTORIES "${CMAKE_CURRENT_LIST_DIR}/include"
    INTERFACE_LINK_LIBRARIES "${RUMQTTC_CMAKE_STATIC_LINK_LIBRARIES}")
add_library(rumqttc::rumqttc_static ALIAS rumqttc_static)

add_library(rumqttc_shared SHARED IMPORTED GLOBAL)
set_target_properties(rumqttc_shared PROPERTIES
    IMPORTED_LOCATION "${CMAKE_CURRENT_LIST_DIR}/../target/release/${CMAKE_SHARED_LIBRARY_PREFIX}rumqttc${CMAKE_SHARED_LIBRARY_SUFFIX}"
    INTERFACE_INCLUDE_DIRECTORIES "${CMAKE_CURRENT_LIST_DIR}/include")
if(WIN32)
    set_target_properties(rumqttc_shared PROPERTIES
        IMPORTED_IMPLIB "${CMAKE_CURRENT_LIST_DIR}/../target/release/${CMAKE_IMPORT_LIBRARY_PREFIX}rumqttc${CMAKE_SHARED_LIBRARY_SUFFIX}${CMAKE_IMPORT_LIBRARY_SUFFIX}")
endif()
add_library(rumqttc::rumqttc_shared ALIAS rumqttc_shared)

add_library(rumqttc INTERFACE)
target_link_libraries(rumqttc INTERFACE rumqttc::rumqttc_static)
add_library(rumqttc::rumqttc ALIAS rumqttc)

configure_file(pkgconfig/rumqttc.pc.in "${CMAKE_CURRENT_BINARY_DIR}/rumqttc.pc" @ONLY)
install(FILES "${CMAKE_CURRENT_BINARY_DIR}/rumqttc.pc"
    DESTINATION "${CMAKE_INSTALL_LIBDIR}/pkgconfig")

configure_package_config_file(cmake/rumqttcConfig.cmake.in
    "${CMAKE_CURRENT_BINARY_DIR}/rumqttcConfig.cmake"
    INSTALL_DESTINATION "${CMAKE_INSTALL_LIBDIR}/cmake/rumqttc")
if(PROJECT_VERSION_MAJOR EQUAL 0)
    set(RUMQTTC_CMAKE_VERSION_COMPATIBILITY SameMinorVersion)
else()
    set(RUMQTTC_CMAKE_VERSION_COMPATIBILITY SameMajorVersion)
endif()
if(RUMQTTC_PACKAGE_VERSION MATCHES "-")
    # CMake package-version requests accept numeric versions, not SemVer
    # prereleases. A prerelease must not impersonate its future stable version.
    configure_file(cmake/rumqttcConfigVersion-prerelease.cmake.in
        "${CMAKE_CURRENT_BINARY_DIR}/rumqttcConfigVersion.cmake" @ONLY)
else()
    write_basic_package_version_file(
        "${CMAKE_CURRENT_BINARY_DIR}/rumqttcConfigVersion.cmake"
        VERSION ${PROJECT_VERSION}
        COMPATIBILITY ${RUMQTTC_CMAKE_VERSION_COMPATIBILITY})
endif()
install(FILES
    "${CMAKE_CURRENT_BINARY_DIR}/rumqttcConfig.cmake"
    "${CMAKE_CURRENT_BINARY_DIR}/rumqttcConfigVersion.cmake"
    DESTINATION "${CMAKE_INSTALL_LIBDIR}/cmake/rumqttc")
