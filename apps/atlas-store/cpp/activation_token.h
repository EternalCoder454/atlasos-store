// Asks the window system for an activation token before the Store starts an
// app, so the app's window comes to the front instead of opening behind the
// Store. QML calls `request` and waits for `ready`; the job that starts the
// app (src/jobs.rs) then runs on the worker with the token. Nothing here
// blocks: the token arrives as a signal, and a timer ends the wait.
#pragma once

#include <QObject>
#include <QString>
#include <QTimer>
#include <QtQml/qqmlregistration.h>

class QWindow;

class ActivationToken : public QObject
{
    Q_OBJECT
    QML_ELEMENT
    QML_SINGLETON

public:
    explicit ActivationToken(QObject *parent = nullptr);

    // Asks for a token for the app `appId` on behalf of `window` and emits
    // `ready` once, later (never from inside this call), with the token or with
    // "" when there is none: on X11, with no window, when the compositor does
    // not answer within a second. A new request ends an unanswered one with "".
    Q_INVOKABLE void request(QWindow *window, const QString &appId);

Q_SIGNALS:
    void ready(const QString &appId, const QString &token);

private:
    void finish(const QString &token);

    QTimer m_timeout;
    QString m_appId;
    bool m_pending = false;
    quint64 m_generation = 0;
};
