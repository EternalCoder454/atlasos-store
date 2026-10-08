//! The Updates place: app updates through Telamon Updater's engine.
//!
//! Stub: the bridge and the object are wired into `lib.rs`, `main.cpp` and
//! `qml/Main.qml`; the work on item 2 (Updates) fills it in.

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        #[qobject]
        /// The object is started.
        #[qproperty(bool, ready)]
        #[namespace = "telamon_store"]
        type AppUpdates = super::AppUpdatesRust;
    }

    unsafe extern "RustQt" {
        /// Starts the worker. Once.
        #[qinvokable]
        fn start(self: Pin<&mut AppUpdates>);
    }

    impl cxx_qt::Threading for AppUpdates {}

    #[namespace = "rust::cxxqtlib1"]
    unsafe extern "C++" {
        include!("cxx-qt-lib/common.h");

        #[cxx_name = "make_unique"]
        fn app_updates_make_unique() -> UniquePtr<AppUpdates>;
    }
}

use core::pin::Pin;
use cxx_qt::CxxQtType;

#[derive(Default)]
pub struct AppUpdatesRust {
    ready: bool,
}

impl qobject::AppUpdates {
    pub fn start(mut self: Pin<&mut Self>) {
        if !*self.ready() {
            self.as_mut().set_ready(true);
        }
    }
}
