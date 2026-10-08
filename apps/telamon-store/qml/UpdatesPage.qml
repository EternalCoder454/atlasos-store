pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import Telamon.Ui

// The Updates place: the apps with an update waiting, through Telamon
// Updater's engine (src/updates.rs). The engine updates everything that
// waits at once, so there is Update All and no Update on a row. Runtimes and
// other components that update along are one folded line. The settings for
// updating in the background stay in Telamon Settings; a button opens them.
// Every text from the catalog or the engine is plain.
TelamonPage {
    id: page

    required property var updates
    required property var jobs

    title: qsTr("Updates")

    signal appRequested(string appId)

    readonly property var apps: JSON.parse(page.updates.appsJson)
    readonly property var others: JSON.parse(page.updates.othersJson)
    readonly property bool working: page.updates.busy
    readonly property bool hasError: page.updates.errorText.length > 0
    readonly property bool anyWaiting: page.apps.length > 0 || page.others.length > 0
    // Nothing is installed at all (the installed list is the Store's, read once).
    readonly property bool noApps: {
        void page.jobs.installedRevision;
        return page.jobs.installedReady && JSON.parse(page.jobs.installedJson).length === 0;
    }
    readonly property string lastText: page.updates.lastChecked > 0 ? qsTr("Last checked: %1").arg(TelamonFormat.date(new Date(page.updates.lastChecked * 1000), "relative")) : ""

    // Opening the page checks when no check ended this session or the last
    // one is older than ten minutes; nothing runs while the page is not open.
    Component.onCompleted: page.updates.pageOpened()

    // Open Telamon Settings asks the window system for an activation token
    // first (C++, activation_token.cpp), as an app's Open does, so the
    // window comes to the front.
    property bool openingSettings: false

    function openSettings() {
        if (page.openingSettings) {
            return;
        }
        page.openingSettings = true;
        ActivationToken.request(page.Window.window, "telamon-settings");
    }

    Connections {
        target: ActivationToken
        function onReady(appId, token) {
            if (!page.openingSettings || appId !== "telamon-settings") {
                return;
            }
            page.openingSettings = false;
            page.updates.openSettingsWithToken(token);
        }
    }

    headerTrailing: [
        TelamonButton {
            text: qsTr("Check for Updates")
            symbol: Symbols.Refresh
            enabled: !page.working
            onClicked: page.updates.check()
        },
        TelamonButton {
            text: qsTr("Update All")
            variant: TelamonButton.Prominent
            visible: page.updates.loaded && page.anyWaiting
            enabled: !page.working
            // One press, unless some update asks for new permissions: then
            // the Store's own dialog lists them first.
            onClicked: {
                if (page.updates.reviewCount > 0) {
                    reviewDialog.show(page.apps.filter(a => a.review), page.updates.downloadText);
                } else {
                    page.updates.updateAll();
                }
            }
        }
    ]

    UpdateReviewDialog {
        id: reviewDialog
        onUpdateAll: page.updates.updateAll()
        onUpdateWithout: page.updates.updateWithoutNewPermissions()
    }

    Text {
        Layout.fillWidth: true
        visible: text.length > 0 && !page.working
        text: page.lastText
        font.family: TelamonStyle.fontFamily
        font.pointSize: TelamonStyle.fontSizeCaption
        color: TelamonStyle.textMuted
        textFormat: Text.PlainText
    }

    // What the worker is doing. Cancel only while it waits for another
    // update to finish: the engine cannot be stopped once it runs.
    RowLayout {
        Layout.fillWidth: true
        visible: page.working
        spacing: TelamonStyle.spacingLarge

        ColumnLayout {
            Layout.fillWidth: true
            spacing: TelamonStyle.spacingSmall
            Text {
                Layout.fillWidth: true
                text: page.updates.status.length > 0 ? page.updates.status : qsTr("Working…")
                wrapMode: Text.Wrap
                font.family: TelamonStyle.fontFamily
                font.pointSize: TelamonStyle.fontSizeCaption
                color: page.updates.waiting ? TelamonStyle.warning : TelamonStyle.textMuted
                textFormat: Text.PlainText
            }
            TelamonProgressBar {
                Layout.fillWidth: true
                indeterminate: page.updates.percent <= 0
                value: Math.max(0, page.updates.percent) / 100
            }
        }
        TelamonButton {
            Layout.alignment: Qt.AlignVCenter
            text: qsTr("Cancel")
            visible: page.updates.waiting
            onClicked: page.updates.cancelWait()
        }
    }

    // What the last update did.
    RowLayout {
        Layout.fillWidth: true
        visible: !page.working && page.updates.notice.length > 0
        spacing: TelamonStyle.spacingLarge
        Text {
            Layout.fillWidth: true
            text: page.updates.notice
            wrapMode: Text.Wrap
            font.family: TelamonStyle.fontFamily
            font.pointSize: TelamonStyle.fontSizeBody
            color: TelamonStyle.success
            textFormat: Text.PlainText
        }
        TelamonButton {
            text: qsTr("Dismiss")
            onClicked: page.updates.clearMessages()
        }
    }

    // A failure with a list to show: above it. Without one, the empty state.
    Rectangle {
        Layout.fillWidth: true
        visible: page.hasError && !page.working && page.updates.loaded
        implicitHeight: errorColumn.implicitHeight + TelamonStyle.spacingLarge * 2
        radius: TelamonStyle.radius
        color: TelamonStyle.errorFill
        border.width: 1
        border.color: Qt.alpha(TelamonStyle.error, 0.4)

        ColumnLayout {
            id: errorColumn
            anchors.fill: parent
            anchors.margins: TelamonStyle.spacingLarge
            spacing: TelamonStyle.spacing
            RowLayout {
                Layout.fillWidth: true
                spacing: TelamonStyle.spacingLarge
                Text {
                    Layout.fillWidth: true
                    text: page.updates.errorText
                    wrapMode: Text.Wrap
                    font.family: TelamonStyle.fontFamily
                    font.pointSize: TelamonStyle.fontSizeBody
                    color: TelamonStyle.error
                    textFormat: Text.PlainText
                }
                TelamonButton {
                    text: qsTr("Dismiss")
                    onClicked: page.updates.clearMessages()
                }
            }
            TelamonExpandableSection {
                Layout.fillWidth: true
                visible: page.updates.errorDetail.length > 0
                title: qsTr("Details")
                Text {
                    Layout.fillWidth: true
                    text: page.updates.errorDetail
                    wrapMode: Text.Wrap
                    font.family: TelamonStyle.fontFamily
                    font.pointSize: TelamonStyle.fontSizeCaption
                    color: TelamonStyle.textMuted
                    textFormat: Text.PlainText
                }
            }
        }
    }

    // The first look for updates.
    ColumnLayout {
        Layout.fillWidth: true
        Layout.topMargin: TelamonStyle.spacingXXLarge
        visible: page.working && !page.updates.loaded
        spacing: TelamonStyle.spacingLarge
        TelamonSpinner {
            Layout.alignment: Qt.AlignHCenter
            running: parent.visible
        }
        Text {
            Layout.fillWidth: true
            horizontalAlignment: Text.AlignHCenter
            text: qsTr("Checking for updates…")
            font.family: TelamonStyle.fontFamily
            font.pointSize: TelamonStyle.fontSizeBody
            color: TelamonStyle.textMuted
            textFormat: Text.PlainText
        }
        Text {
            Layout.fillWidth: true
            visible: text.length > 0
            horizontalAlignment: Text.AlignHCenter
            text: page.lastText
            font.family: TelamonStyle.fontFamily
            font.pointSize: TelamonStyle.fontSizeCaption
            color: TelamonStyle.textMuted
            textFormat: Text.PlainText
        }
    }

    // The check failed and there is no list.
    TelamonEmptyState {
        Layout.fillWidth: true
        Layout.preferredHeight: Kirigami.Units.gridUnit * 14
        visible: !page.working && !page.updates.loaded && page.hasError
        symbol: Symbols.Error
        title: qsTr("Could Not Check for Updates")
        text: page.updates.errorText
        actionText: qsTr("Try Again")
        actionSymbol: Symbols.Refresh
        onTriggered: page.updates.check()
    }
    TelamonExpandableSection {
        Layout.fillWidth: true
        visible: !page.working && !page.updates.loaded && page.updates.errorDetail.length > 0
        title: qsTr("Details")
        Text {
            Layout.fillWidth: true
            text: page.updates.errorDetail
            wrapMode: Text.Wrap
            font.family: TelamonStyle.fontFamily
            font.pointSize: TelamonStyle.fontSizeCaption
            color: TelamonStyle.textMuted
            textFormat: Text.PlainText
        }
    }

    // Nothing waits.
    TelamonEmptyState {
        Layout.fillWidth: true
        Layout.preferredHeight: Kirigami.Units.gridUnit * 14
        visible: page.updates.loaded && !page.anyWaiting && !page.working
        symbol: page.noApps ? Symbols.Apps : Symbols.CheckCircle
        title: page.noApps ? qsTr("No Apps Installed") : qsTr("Up to Date")
        text: page.noApps ? qsTr("Apps you install show up here.") : qsTr("All apps are up to date.")
    }

    // Only components wait: no app does.
    Text {
        Layout.fillWidth: true
        visible: page.updates.loaded && page.apps.length === 0 && page.others.length > 0
        text: qsTr("All apps are up to date.")
        font.family: TelamonStyle.fontFamily
        font.pointSize: TelamonStyle.fontSizeBody
        color: TelamonStyle.textMuted
        textFormat: Text.PlainText
    }

    // One grouped card, a row per app: icon, name, branch, who it is for and
    // the size; what it asks for that the installed version doesn't have
    // (never installed unseen); what is new where the catalog says. Clicking
    // the row opens the app's page.
    Section {
        Layout.fillWidth: true
        visible: page.updates.loaded && page.apps.length > 0
        title: qsTr("Apps")
        footer: page.updates.downloadText.length > 0 ? qsTr("Update All downloads about %1 in all.").arg(page.updates.downloadText) : ""

        Repeater {
            model: page.apps
            SectionRow {
                id: row
                required property var modelData
                readonly property real iconSide: Math.round(Kirigami.Units.gridUnit * 2.8)

                title: row.modelData.name
                subtitle: [row.modelData.detail, row.modelData.asks.length > 0 ? qsTr("Asks for new permissions: %1").arg(row.modelData.asks) : "", row.modelData.notes].filter(s => s.length > 0).join(". ")
                clickable: true
                onClicked: page.appRequested(row.modelData.appId)

                leading: Rectangle {
                    width: row.iconSide
                    height: row.iconSide
                    radius: Math.round(row.iconSide * 0.225)
                    color: icon.status === Image.Ready ? "transparent" : Qt.alpha(TelamonStyle.accent, 0.14)
                    Accessible.ignored: true
                    Text {
                        anchors.centerIn: parent
                        visible: icon.status !== Image.Ready
                        text: row.modelData.name.length > 0 ? row.modelData.name.charAt(0).toUpperCase() : ""
                        font.family: TelamonStyle.fontFamily
                        font.pointSize: TelamonStyle.fontSizeHeading
                        font.bold: true
                        color: TelamonStyle.accent
                        textFormat: Text.PlainText
                    }
                    Image {
                        id: icon
                        anchors.fill: parent
                        source: row.modelData.iconSource
                        asynchronous: true
                        cache: true
                        fillMode: Image.PreserveAspectFit
                        sourceSize: Qt.size(row.iconSide * Screen.devicePixelRatio, row.iconSide * Screen.devicePixelRatio)
                    }
                }

                content: ColumnLayout {
                    Layout.fillWidth: true
                    Layout.alignment: Qt.AlignVCenter
                    spacing: TelamonStyle.spacingXSmall

                    Text {
                        Layout.fillWidth: true
                        text: row.modelData.name
                        font.family: TelamonStyle.fontFamily
                        font.pointSize: TelamonStyle.fontSizeBody
                        font.bold: true
                        color: TelamonStyle.text
                        textFormat: Text.PlainText
                        elide: Text.ElideRight
                        Accessible.ignored: true
                    }
                    Text {
                        Layout.fillWidth: true
                        text: row.modelData.detail
                        font.family: TelamonStyle.fontFamily
                        font.pointSize: TelamonStyle.fontSizeCaption
                        color: TelamonStyle.textMuted
                        textFormat: Text.PlainText
                        elide: Text.ElideRight
                        Accessible.ignored: true
                    }
                    Text {
                        Layout.fillWidth: true
                        visible: row.modelData.asks.length > 0
                        text: qsTr("Asks for new permissions: %1").arg(row.modelData.asks)
                        wrapMode: Text.Wrap
                        maximumLineCount: 3
                        elide: Text.ElideRight
                        font.family: TelamonStyle.fontFamily
                        font.pointSize: TelamonStyle.fontSizeCaption
                        color: TelamonStyle.warning
                        textFormat: Text.PlainText
                        Accessible.ignored: true
                    }
                    Text {
                        Layout.fillWidth: true
                        visible: row.modelData.notes.length > 0
                        text: row.modelData.notes
                        wrapMode: Text.Wrap
                        maximumLineCount: 2
                        elide: Text.ElideRight
                        font.family: TelamonStyle.fontFamily
                        font.pointSize: TelamonStyle.fontSizeCaption
                        color: TelamonStyle.textMuted
                        textFormat: Text.PlainText
                        Accessible.ignored: true
                    }
                }
            }
        }
    }

    // Runtimes and other components are part of the run, quietly.
    TelamonExpandableSection {
        Layout.fillWidth: true
        visible: page.updates.loaded && page.others.length > 0
        title: page.others.length === 1 ? qsTr("1 runtime or other component will also be updated") : qsTr("%1 runtimes and other components will also be updated").arg(page.others.length)

        Repeater {
            model: page.others
            RowLayout {
                id: other
                required property var modelData
                Layout.fillWidth: true
                spacing: TelamonStyle.spacingLarge
                Text {
                    Layout.fillWidth: true
                    text: other.modelData.name
                    elide: Text.ElideRight
                    font.family: TelamonStyle.fontFamily
                    font.pointSize: TelamonStyle.fontSizeBody
                    color: TelamonStyle.text
                    textFormat: Text.PlainText
                }
                Text {
                    text: other.modelData.detail
                    font.family: TelamonStyle.fontFamily
                    font.pointSize: TelamonStyle.fontSizeCaption
                    color: TelamonStyle.textMuted
                    textFormat: Text.PlainText
                }
            }
        }
    }

    // The settings for updating in the background live in Telamon Settings.
    RowLayout {
        Layout.fillWidth: true
        Layout.topMargin: TelamonStyle.spacingLarge
        spacing: TelamonStyle.spacingLarge

        Text {
            Layout.fillWidth: true
            text: page.updates.autoUpdates ? qsTr("Apps update in the background: On") : qsTr("Apps update in the background: Off")
            font.family: TelamonStyle.fontFamily
            font.pointSize: TelamonStyle.fontSizeCaption
            color: TelamonStyle.textMuted
            textFormat: Text.PlainText
        }
        TelamonButton {
            text: qsTr("Update Settings")
            symbol: Symbols.Settings
            variant: TelamonButton.Ghost
            onClicked: page.openSettings()
        }
    }
    Text {
        Layout.fillWidth: true
        visible: page.updates.settingsNote.length > 0
        text: page.updates.settingsNote
        wrapMode: Text.Wrap
        font.family: TelamonStyle.fontFamily
        font.pointSize: TelamonStyle.fontSizeCaption
        color: TelamonStyle.error
        textFormat: Text.PlainText
    }
}
