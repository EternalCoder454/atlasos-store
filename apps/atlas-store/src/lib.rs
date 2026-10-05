//! Rust side of Atlas Store. `cpp/main.cpp` starts Qt and the single-instance
//! service; everything else lives here as QObjects exposed to QML, over the
//! Qt-free `atlas-store-core`.

mod backend;
mod catalog;

atlas_framework_ui::app! {
    name: "Atlas Store",
    id: "net.eterneon.atlas.store",
    repo: "atlasos-store",
    ui: "1.4.0",
}

use std::ffi::c_void;

/// The QObjects `main.cpp` hands to the QML engine.
#[repr(C)]
pub struct AtlasObjects {
    pub backend: *mut c_void,
    pub catalog: *mut c_void,
    pub search: *mut c_void,
    pub browse: *mut c_void,
}

/// Called once from `main.cpp`, after `QApplication` exists. Makes every
/// QObject and starts reading the catalogs on a worker thread. Ownership of
/// each pointer passes to the caller (QObjects with no parent); delete them
/// after the QML engine.
#[unsafe(no_mangle)]
pub extern "C" fn atlas_objects_new() -> AtlasObjects {
    let backend = backend::qobject::backend_make_unique();
    let mut catalog = catalog::qobject::catalog_make_unique();
    let search = catalog::qobject::app_list_model_make_unique();
    let browse = catalog::qobject::app_list_model_make_unique();
    catalog.pin_mut().start();
    AtlasObjects {
        backend: backend.into_raw().cast(),
        catalog: catalog.into_raw().cast(),
        search: search.into_raw().cast(),
        browse: browse.into_raw().cast(),
    }
}
