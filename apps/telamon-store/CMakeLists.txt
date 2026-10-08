cmake_minimum_required(VERSION 3.24)
project(telamon-store VERSION 0.2.0 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_AUTOMOC ON)

if(CMAKE_INSTALL_PREFIX_INITIALIZED_TO_DEFAULT)
    set(CMAKE_INSTALL_PREFIX /usr CACHE PATH "" FORCE)
endif()
include(GNUInstallDirs)
find_package(Qt6 6.11 REQUIRED COMPONENTS Core Gui Qml Quick QuickControls2 Widgets)
qt_policy(SET QTP0001 NEW)
qt_policy(SET QTP0004 NEW)
# telamon-framework-ui's startup code (single instance, window activation)
find_package(KF6DBusAddons REQUIRED)
find_package(KF6WindowSystem REQUIRED)
find_package(Corrosion REQUIRED)
# telamon-store-core calls libflatpak and GIO; the Rust static library leaves
# them to the final link.
find_package(PkgConfig REQUIRED)
pkg_check_modules(FLATPAK REQUIRED IMPORTED_TARGET flatpak glib-2.0 gio-2.0 gobject-2.0)

set(WORKSPACE_DIR ${CMAKE_CURRENT_SOURCE_DIR}/../..)

# Rust library (CXX-Qt QObjects, atlas-framework, telamon-store-core).
# cxx-qt-build finds Qt via $QMAKE.
find_program(QMAKE6 NAMES qmake6 qmake REQUIRED)
corrosion_import_crate(MANIFEST_PATH ${WORKSPACE_DIR}/Cargo.toml
    CRATES telamon-store LOCKED)
corrosion_set_env_vars(telamon_store QMAKE=${QMAKE6})

# The framework's C header (telamon/app.h) comes with the crate's sources,
# wherever cargo put them (a git checkout in ~/.cargo).
find_program(CARGO NAMES cargo REQUIRED)
execute_process(
    COMMAND ${CARGO} metadata --format-version 1 --locked --manifest-path ${WORKSPACE_DIR}/Cargo.toml
    OUTPUT_VARIABLE CARGO_METADATA
    RESULT_VARIABLE CARGO_METADATA_RESULT
    ERROR_VARIABLE CARGO_METADATA_ERROR
    TIMEOUT 300)
if(NOT CARGO_METADATA_RESULT EQUAL 0)
    message(FATAL_ERROR "cargo metadata failed (${CARGO_METADATA_RESULT}): ${CARGO_METADATA_ERROR}\n"
        "Run `cargo fetch --locked` first, or check the network.")
endif()
string(JSON NPACKAGES LENGTH "${CARGO_METADATA}" packages)
math(EXPR LAST_PACKAGE "${NPACKAGES} - 1")
set(TELAMON_UI_CRATE_DIR "")
foreach(i RANGE ${LAST_PACKAGE})
    string(JSON PKG_NAME GET "${CARGO_METADATA}" packages ${i} name)
    if(PKG_NAME STREQUAL "telamon-framework-ui")
        string(JSON PKG_MANIFEST GET "${CARGO_METADATA}" packages ${i} manifest_path)
        get_filename_component(TELAMON_UI_CRATE_DIR "${PKG_MANIFEST}" DIRECTORY)
    endif()
endforeach()
if(NOT EXISTS "${TELAMON_UI_CRATE_DIR}/include/telamon/app.h")
    message(FATAL_ERROR "telamon-framework-ui's include/telamon/app.h not found (looked in '${TELAMON_UI_CRATE_DIR}')")
endif()

# Telamon.Ui, the shared Telamon look, is installed like Kirigami (telamon-ui, from
# atlas-framework): QML imports it and nothing links it. qmlcachegen and
# qmllint read its types from the installed copy.
if(NOT EXISTS "${QT6_INSTALL_PREFIX}/${QT6_INSTALL_QML}/Telamon/Ui/qmldir")
    message(FATAL_ERROR "Telamon.Ui is not installed: install telamon-ui (atlas-framework)")
endif()

qt_add_executable(telamon-store-app cpp/main.cpp)
set_target_properties(telamon-store-app PROPERTIES OUTPUT_NAME telamon-store)
# cpp/ too: the QML type registrations Qt generates include the headers of
# the C++ QML types (cpp/activation_token.h) by file name alone.
target_include_directories(telamon-store-app PRIVATE "${TELAMON_UI_CRATE_DIR}/include" "${CMAKE_CURRENT_SOURCE_DIR}/cpp")

# qmlcachegen compiles every QML file to C++ at build time; the app reads no
# .qml from disk at run time.
qt_add_qml_module(telamon-store-app
    URI net.eterneon.telamon.store
    VERSION 1.0
    SOURCES
        cpp/activation_token.h
        cpp/activation_token.cpp
    QML_FILES
        qml/Main.qml
        qml/AddSourceDialog.qml
        qml/AppPage.qml
        qml/InstallDialog.qml
        qml/InstalledPage.qml
        qml/JobStatus.qml
        qml/RemoveDialog.qml
        qml/RemoveSourceDialog.qml
        qml/SourceRow.qml
        qml/UnusedDialog.qml
        qml/AppGrid.qml
        qml/AppTile.qml
        qml/CatalogState.qml
        qml/CategoryPage.qml
        qml/CategoryTile.qml
        qml/HomePage.qml
        qml/SearchPage.qml
        qml/SourcesPage.qml
        qml/SourcesStatus.qml
        qml/UpdatesPage.qml
        qml/PlaceholderPage.qml
)

target_link_libraries(telamon-store-app PRIVATE
    telamon_store
    Qt6::Core Qt6::Gui Qt6::Qml Qt6::Quick Qt6::QuickControls2 Qt6::Widgets
    KF6::DBusAddons KF6::WindowSystem PkgConfig::FLATPAK)

install(TARGETS telamon-store-app RUNTIME DESTINATION ${CMAKE_INSTALL_BINDIR})
install(FILES data/net.eterneon.telamon.store.desktop
        DESTINATION ${CMAKE_INSTALL_DATADIR}/applications)
install(FILES data/net.eterneon.telamon.store.metainfo.xml
        DESTINATION ${CMAKE_INSTALL_DATADIR}/metainfo)
install(FILES data/net.eterneon.telamon.store.svg
        DESTINATION ${CMAKE_INSTALL_DATADIR}/icons/hicolor/scalable/apps)
