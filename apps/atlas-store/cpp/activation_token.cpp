#include "activation_token.h"

#include <KWaylandExtras>
#include <KWindowSystem>

#include <QWindow>

ActivationToken::ActivationToken(QObject *parent)
    : QObject(parent)
{
    m_timeout.setSingleShot(true);
    m_timeout.setInterval(1000);
    connect(&m_timeout, &QTimer::timeout, this, [this] { finish(QString()); });
    if (KWindowSystem::isPlatformWayland()) {
        // One request is open at a time (`request` ends an earlier one), and
        // the Store asks for no other tokens, so the first one to arrive is
        // the answer. The signal's number is not compared: it is the input
        // serial in some KDE Frameworks releases and a request number in
        // others.
        connect(KWaylandExtras::self(), &KWaylandExtras::xdgActivationTokenArrived, this, [this](quint32, const QString &token) { finish(token); });
    }
}

void ActivationToken::request(QWindow *window, const QString &appId)
{
    finish(QString());
    m_pending = true;
    m_appId = appId;
    const quint64 generation = ++m_generation;
    if (!window || !KWindowSystem::isPlatformWayland()) {
        // X11 has no activation tokens for this, and without a window there is
        // nothing to ask for. Answered from the event loop, so the caller sees
        // the same order of events either way.
        QTimer::singleShot(0, this, [this, generation] {
            if (generation == m_generation) {
                finish(QString());
            }
        });
        return;
    }
    m_timeout.start();
    // The compositor wants the app's desktop file name, without ".desktop".
    QString desktopName = appId;
    if (desktopName.endsWith(QLatin1String(".desktop"))) {
        desktopName.chop(8);
    }
    KWaylandExtras::requestXdgActivationToken(window, KWaylandExtras::lastInputSerial(window), desktopName);
}

void ActivationToken::finish(const QString &token)
{
    if (!m_pending) {
        return;
    }
    m_pending = false;
    m_timeout.stop();
    Q_EMIT ready(m_appId, token);
}
