// Starts Qt, makes the Store single-instance and loads the window. A second
// launch (a link from the browser, a .flatpakref from the file manager,
// `telamon-store --app <id>` from Telamon Updater) hands its arguments to this
// one and exits; they are read in Rust (src/backend.rs), never here.
#include <telamon/app.h>

#include <KDBusService>
#include <KWindowSystem>

#include <QApplication>
#include <QCommandLineParser>
#include <QDir>
#include <QQmlApplicationEngine>
#include <QQuickWindow>
#include <QSGRendererInterface>
#include <QUrl>

#include <memory>

// Defined in src/lib.rs.
struct StoreObjects {
    void *backend;
    void *catalog;
    void *search;
    void *browse;
    void *jobs;
    void *sources;
    void *updates;
    void *featured;
};
extern "C" StoreObjects store_objects_new();

// Hands a launch's arguments (without the program name) to the backend.
static void activate(QObject *backend, const QStringList &arguments, const QString &cwd)
{
    if (!QMetaObject::invokeMethod(backend, "activate", Q_ARG(QStringList, arguments), Q_ARG(QString, cwd))) {
        qWarning("telamon-store: the backend did not take the launch arguments");
    }
}

static void raise(QQmlApplicationEngine *engine)
{
    auto *window = qobject_cast<QQuickWindow *>(engine->rootObjects().value(0));
    if (!window) {
        return;
    }
    if (window->visibility() == QWindow::Minimized) {
        window->showNormal();
    } else {
        window->show();
    }
    // The launcher's activation token: without it Wayland keeps the window
    // down.
    KWindowSystem::updateStartupId(window);
    KWindowSystem::activateWindow(window);
}

int main(int argc, char *argv[])
{
    telamon_app_init();
    // Drawn on the CPU like the other Telamon apps unless QT_QUICK_BACKEND says
    // otherwise (the P phase measures whether the Store's image grid is
    // better on the GPU).
    if (qEnvironmentVariableIsEmpty("QT_QUICK_BACKEND")) {
        QQuickWindow::setGraphicsApi(QSGRendererInterface::Software);
    }

    QApplication app(argc, argv);
    telamon_app_ready();

    QCommandLineParser parser;
    parser.setApplicationDescription(QStringLiteral("The app store of Telamon OS."));
    parser.addHelpOption();
    parser.addVersionOption();
    parser.addOption({QStringLiteral("app"), QStringLiteral("Open the page of the app <id>."), QStringLiteral("id")});
    parser.addOption({QStringLiteral("search"), QStringLiteral("Search for <text>."), QStringLiteral("text")});
    parser.addOption({QStringLiteral("page"), QStringLiteral("Open home, installed, updates or sources."), QStringLiteral("page")});
    parser.addPositionalArgument(QStringLiteral("link"), QStringLiteral(".flatpakref, .flatpakrepo or .flatpak files, appstream: or flatpak+https: links."),
                                 QStringLiteral("[link...]"));
    // Only --help and --version are acted on here. Every other argument goes
    // to the backend, which alone decides what is valid and says what it
    // refused in the window, so the two never disagree.
    parser.parse(QCoreApplication::arguments());
    if (parser.isSet(QStringLiteral("help"))) {
        parser.showHelp();
    }
    if (parser.isSet(QStringLiteral("version"))) {
        parser.showVersion();
    }

    // One instance per session. A second launch's arguments come here through
    // activateRequested; without a session bus each launch runs on its own.
    KDBusService service(KDBusService::Unique | KDBusService::NoExitOnFailure);

    // The backend, catalog and models outlive the engine: the window's
    // bindings read them until the engine is gone.
    const StoreObjects made = store_objects_new();
    std::unique_ptr<QObject> backend(static_cast<QObject *>(made.backend));
    std::unique_ptr<QObject> catalog(static_cast<QObject *>(made.catalog));
    std::unique_ptr<QObject> searchModel(static_cast<QObject *>(made.search));
    std::unique_ptr<QObject> browseModel(static_cast<QObject *>(made.browse));
    std::unique_ptr<QObject> jobs(static_cast<QObject *>(made.jobs));
    std::unique_ptr<QObject> sources(static_cast<QObject *>(made.sources));
    std::unique_ptr<QObject> updates(static_cast<QObject *>(made.updates));
    std::unique_ptr<QObject> featured(static_cast<QObject *>(made.featured));
    auto engine = std::make_unique<QQmlApplicationEngine>();
    QObject::connect(engine.get(), &QQmlApplicationEngine::objectCreationFailed, &app, [] { QCoreApplication::exit(1); }, Qt::QueuedConnection);
    engine->setInitialProperties({
        {QStringLiteral("backend"), QVariant::fromValue(backend.get())},
        {QStringLiteral("catalog"), QVariant::fromValue(catalog.get())},
        {QStringLiteral("searchModel"), QVariant::fromValue(searchModel.get())},
        {QStringLiteral("browseModel"), QVariant::fromValue(browseModel.get())},
        {QStringLiteral("jobs"), QVariant::fromValue(jobs.get())},
        {QStringLiteral("sources"), QVariant::fromValue(sources.get())},
        {QStringLiteral("updates"), QVariant::fromValue(updates.get())},
        {QStringLiteral("featured"), QVariant::fromValue(featured.get())},
    });
    engine->loadFromModule("net.eterneon.telamon.store", "Main");
    if (engine->rootObjects().isEmpty()) {
        return 1;
    }

    // A second launch. With nothing but the program name (the launcher
    // icon, the taskbar) the window only comes up, keeping its place.
    QObject::connect(&service, &KDBusService::activateRequested, backend.get(),
                     [e = engine.get(), b = backend.get()](const QStringList &arguments, const QString &cwd) {
                         raise(e);
                         if (arguments.size() > 1) {
                             // The Rust side reads 64; one more lets it say some were left out.
                             activate(b, arguments.mid(1, 65), cwd);
                         }
                     });
    // org.freedesktop.Application.Open from any process in the session: links
    // and files only, and no folder, so relative paths are refused.
    QObject::connect(&service, &KDBusService::openRequested, backend.get(), [e = engine.get(), b = backend.get()](const QList<QUrl> &urls) {
        raise(e);
        if (urls.isEmpty()) {
            return;
        }
        // `--` first: whatever the caller sent is never read as an option.
        QStringList arguments{QStringLiteral("--")};
        // The Rust side looks at 64 arguments (this `--` and 63 URLs) and
        // counts the rest; one more is enough for it to say some were left
        // out.
        for (const QUrl &url : urls.mid(0, 64)) {
            arguments << url.toString(QUrl::FullyEncoded);
        }
        activate(b, arguments, QString());
    });
    activate(backend.get(), QCoreApplication::arguments().mid(1), QDir::currentPath());

    const int code = app.exec();
    engine.reset();
    return code;
}
