//! The Sources place: the Flatpak remotes (list, enable, add, remove).
//!
//! Stub: the bridge and the object are wired into `lib.rs`, `main.cpp` and
//! `qml/Main.qml`; the work on item 1 (Sources) fills it in.

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
        type Sources = super::SourcesRust;
    }

    unsafe extern "RustQt" {
        /// Starts the worker. Once.
        #[qinvokable]
        fn start(self: Pin<&mut Sources>);
    }

    impl cxx_qt::Threading for Sources {}

    #[namespace = "rust::cxxqtlib1"]
    unsafe extern "C++" {
        include!("cxx-qt-lib/common.h");

        #[cxx_name = "make_unique"]
        fn sources_make_unique() -> UniquePtr<Sources>;
    }
}

use core::pin::Pin;
use cxx_qt::CxxQtType;

#[derive(Default)]
pub struct SourcesRust {
    ready: bool,
}

impl qobject::Sources {
    pub fn start(mut self: Pin<&mut Self>) {
        if !*self.ready() {
            self.as_mut().set_ready(true);
        }
    }
}
