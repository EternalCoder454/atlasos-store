//! Flathub's curated lists for Home and the category pages.
//!
//! Stub: the bridge and the object are wired into `lib.rs`, `main.cpp` and
//! `qml/Main.qml`; the work on items 3 and 4 (Home, categories) fills it in.

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
        type Featured = super::FeaturedRust;
    }

    unsafe extern "RustQt" {
        /// Starts the worker. Once.
        #[qinvokable]
        fn start(self: Pin<&mut Featured>);
    }

    impl cxx_qt::Threading for Featured {}

    #[namespace = "rust::cxxqtlib1"]
    unsafe extern "C++" {
        include!("cxx-qt-lib/common.h");

        #[cxx_name = "make_unique"]
        fn featured_make_unique() -> UniquePtr<Featured>;
    }
}

use core::pin::Pin;
use cxx_qt::CxxQtType;

#[derive(Default)]
pub struct FeaturedRust {
    ready: bool,
}

impl qobject::Featured {
    pub fn start(mut self: Pin<&mut Self>) {
        if !*self.ready() {
            self.as_mut().set_ready(true);
        }
    }
}
