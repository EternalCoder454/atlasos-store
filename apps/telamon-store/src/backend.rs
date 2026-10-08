//! The window's entry point: what a launch asks for. `main.cpp` calls
//! `activate` with the first launch's arguments and with each forwarded
//! second launch's; QML listens to `requested` and opens the page.

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qstringlist.h");
        type QStringList = cxx_qt_lib::QStringList;
    }

    extern "RustQt" {
        #[qobject]
        #[namespace = "telamon_store"]
        type Backend = super::BackendRust;

        /// Handles a launch's arguments (without the program name), relative
        /// paths read against `cwd`.
        #[qinvokable]
        fn activate(self: Pin<&mut Backend>, args: &QStringList, cwd: &QString);

        /// One request: `kind` is `page`, `app`, `search`, `remove`, `ref`,
        /// `repo`, `bundle`, `rpm`, `appimage`, `nativeBundle` or `refUrl`; `value` the page name, ID,
        /// text, path or URL.
        #[qsignal]
        fn requested(self: Pin<&mut Backend>, kind: QString, value: QString);

        /// What a launch asked for that the Store won't do, one line per
        /// argument ("argument: reason"), already made safe to show as plain
        /// text.
        #[qsignal]
        fn refused(self: Pin<&mut Backend>, text: QString);
    }

    impl cxx_qt::Threading for Backend {}

    #[namespace = "rust::cxxqtlib1"]
    unsafe extern "C++" {
        include!("cxx-qt-lib/common.h");

        #[cxx_name = "make_unique"]
        fn backend_make_unique() -> UniquePtr<Backend>;
    }
}

use core::pin::Pin;
use cxx_qt_lib::{QString, QStringList};
use std::path::PathBuf;
use telamon_store_core::launch::{self, FileKind, Request};

#[derive(Default)]
pub struct BackendRust {}

impl qobject::Backend {
    pub fn activate(mut self: Pin<&mut Self>, args: &QStringList, cwd: &QString) {
        // Only what parse looks at is copied; the rest is counted.
        let total = args.len().max(0) as usize;
        let args: Vec<String> = args
            .iter()
            .take(launch::MAX_ARGS)
            .map(|a| a.to_string())
            .collect();
        let cwd = PathBuf::from(cwd.to_string());
        let mut launch = launch::parse_with(&args, &cwd, &|p| {
            telamon_store_core::appimage::format::sniff_path(p).is_some()
        });
        launch.dropped += total.saturating_sub(args.len());
        let mut lines: Vec<String> = launch
            .refused
            .iter()
            .map(|r| format!("{}: {}", r.arg, r.reason))
            .collect();
        if launch.dropped > 0 {
            lines.push(format!("{} more not looked at", launch.dropped));
        }
        if !lines.is_empty() {
            // {:?} keeps each argument on its line in the journal.
            log::warn!("ignored launch arguments: {lines:?}");
            self.as_mut()
                .refused(QString::from(lines.join("\n").as_str()));
        }
        for request in launch.requests {
            let (kind, value) = describe(&request);
            self.as_mut()
                .requested(QString::from(kind), QString::from(value.as_str()));
        }
    }
}

fn describe(request: &Request) -> (&'static str, String) {
    match request {
        Request::Page(p) => ("page", p.name().to_string()),
        Request::App(id) => ("app", id.clone()),
        Request::Remove(id) => ("remove", id.clone()),
        Request::Search(q) => ("search", q.clone()),
        Request::RefUrl(url) => ("refUrl", url.clone()),
        Request::File(kind, path) => (
            match kind {
                FileKind::Ref => "ref",
                FileKind::Repo => "repo",
                FileKind::Bundle => "bundle",
                FileKind::Rpm => "rpm",
                FileKind::AppImage => "appimage",
                FileKind::NativeBundle => "nativeBundle",
            },
            path.to_string_lossy().into_owned(),
        ),
    }
}
