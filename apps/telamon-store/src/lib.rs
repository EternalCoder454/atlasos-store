//! Rust side of Telamon Store. `cpp/main.cpp` starts Qt and the single-instance
//! service; everything else lives here as QObjects exposed to QML, over the
//! Qt-free `telamon-store-core`.

mod appimage_cli;
mod appimages;
mod backend;
mod catalog;
mod featured;
mod jobs;
mod sources;
mod updates;

telamon_framework_ui::app! {
    name: "Telamon Store",
    id: "net.eterneon.telamon.store",
    repo: "atlasos-store",
    ui: "2.0.0",
}

use std::ffi::c_void;

/// The QObjects `main.cpp` hands to the QML engine.
#[repr(C)]
pub struct StoreObjects {
    pub backend: *mut c_void,
    pub catalog: *mut c_void,
    pub search: *mut c_void,
    pub browse: *mut c_void,
    pub jobs: *mut c_void,
    pub sources: *mut c_void,
    pub updates: *mut c_void,
    pub featured: *mut c_void,
    pub app_images: *mut c_void,
}

/// Called once from `main.cpp`, after `QApplication` exists. Makes every
/// QObject and starts reading the catalogs on a worker thread. Ownership of
/// each pointer passes to the caller (QObjects with no parent); delete them
/// after the QML engine.
#[unsafe(no_mangle)]
pub extern "C" fn store_objects_new() -> StoreObjects {
    let backend = backend::qobject::backend_make_unique();
    let mut catalog = catalog::qobject::catalog_make_unique();
    let search = catalog::qobject::app_list_model_make_unique();
    let browse = catalog::qobject::app_list_model_make_unique();
    let mut jobs = jobs::qobject::jobs_make_unique();
    catalog.pin_mut().start();
    jobs.pin_mut().start();
    let mut sources = sources::qobject::sources_make_unique();
    let mut updates = updates::qobject::app_updates_make_unique();
    let mut featured = featured::qobject::featured_make_unique();
    let mut app_images = appimages::qobject::app_images_make_unique();
    sources.pin_mut().start();
    updates.pin_mut().start();
    featured.pin_mut().start();
    app_images.pin_mut().refresh();
    StoreObjects {
        backend: backend.into_raw().cast(),
        catalog: catalog.into_raw().cast(),
        search: search.into_raw().cast(),
        browse: browse.into_raw().cast(),
        jobs: jobs.into_raw().cast(),
        sources: sources.into_raw().cast(),
        updates: updates.into_raw().cast(),
        featured: featured.into_raw().cast(),
        app_images: app_images.into_raw().cast(),
    }
}
