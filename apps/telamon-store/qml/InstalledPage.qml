pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import Telamon.Ui

// The installed apps (src/jobs.rs reads them on the worker) as rows of one
// grouped card: icon, name, installation, size and version, each row with
// its own Remove, and Remove Unused with the list shown first. Every text is
// plain.
TelamonPage {
    id: page

    required property var jobs
    // The AppImages the Store installed (src/appimages.rs).
    required property var appImages
    // The Telamon apps the Store installed (src/native.rs).
    required property var nativeApps

    title: qsTr("Installed")

    signal appRequested(string appId)
    signal removeRequested(string appId, string name, string scope, string ref)
    signal removeAppImageRequested(string id, string name)
    signal removeNativeRequested(string id, string name)
    signal nativeRequested(string appId)

    readonly property var apps: {
        void page.jobs.installedRevision;
        return JSON.parse(page.jobs.installedJson);
    }
    readonly property bool idle: page.jobs.phase === "idle"
    readonly property var appImageList: JSON.parse(page.appImages.installedJson)
    readonly property bool appImagesIdle: page.appImages.phase === "idle"
    readonly property var nativeList: telamonApps.all.filter(a => a.installed === true)
    readonly property bool nativeIdle: page.nativeApps.phase === "idle" || page.nativeApps.phase === "checking"

    NativeAppsList {
        id: telamonApps
        json: page.nativeApps.appsJson
    }

    // Open asks the window system for an activation token first, then starts
    // the AppImage; without one Wayland can leave its window behind this one.
    property string openingId: ""

    Connections {
        target: ActivationToken
        function onReady(key, token) {
            if (page.openingId.length === 0 || key !== "appimage:" + page.openingId) {
                return;
            }
            const id = page.openingId;
            page.openingId = "";
            page.appImages.open(id, token);
        }
    }

    // Open for a Telamon app, the same way.
    property string openingNative: ""

    Connections {
        target: ActivationToken
        function onReady(key, token) {
            if (page.openingNative.length === 0 || key !== "native:" + page.openingNative) {
                return;
            }
            const id = page.openingNative;
            page.openingNative = "";
            page.nativeApps.open(id, token);
        }
    }

    headerTrailing: [
        TelamonButton {
            text: qsTr("Remove Unused")
            enabled: page.idle && page.jobs.installedReady
            onClicked: page.jobs.checkUnused()
        }
    ]

    JobStatus {
        Layout.fillWidth: true
        jobs: page.jobs
    }

    // What the last AppImage install, removal or Open did, or why not.
    RowLayout {
        Layout.fillWidth: true
        visible: page.appImages.errorText.length > 0 || page.appImages.resultText.length > 0
        spacing: TelamonStyle.spacingLarge
        Text {
            Layout.fillWidth: true
            text: page.appImages.errorText.length > 0 ? page.appImages.errorText : page.appImages.resultText
            wrapMode: Text.Wrap
            font.family: TelamonStyle.fontFamily
            font.pointSize: TelamonStyle.fontSizeBody
            color: page.appImages.errorText.length > 0 ? TelamonStyle.error : TelamonStyle.success
            textFormat: Text.PlainText
        }
        TelamonButton {
            text: qsTr("Dismiss")
            onClicked: page.appImages.clearMessages()
        }
    }

    // What the last Telamon app install, update, removal or Open did.
    RowLayout {
        Layout.fillWidth: true
        visible: page.nativeApps.errorText.length > 0 || page.nativeApps.resultText.length > 0
        spacing: TelamonStyle.spacingLarge
        Text {
            Layout.fillWidth: true
            text: page.nativeApps.errorText.length > 0 ? page.nativeApps.errorText : page.nativeApps.resultText
            wrapMode: Text.Wrap
            font.family: TelamonStyle.fontFamily
            font.pointSize: TelamonStyle.fontSizeBody
            color: page.nativeApps.errorText.length > 0 ? TelamonStyle.error : TelamonStyle.success
            textFormat: Text.PlainText
        }
        TelamonButton {
            text: qsTr("Dismiss")
            onClicked: page.nativeApps.clearMessages()
        }
    }

    Text {
        Layout.fillWidth: true
        visible: page.jobs.installedError.length > 0
        text: page.jobs.installedError
        wrapMode: Text.Wrap
        font.family: TelamonStyle.fontFamily
        font.pointSize: TelamonStyle.fontSizeCaption
        color: TelamonStyle.error
        textFormat: Text.PlainText
    }

    TelamonSpinner {
        Layout.alignment: Qt.AlignHCenter
        visible: !page.jobs.installedReady
        running: visible
    }

    TelamonEmptyState {
        Layout.fillWidth: true
        Layout.preferredHeight: 260
        visible: page.jobs.installedReady && page.apps.length === 0 && page.appImageList.length === 0 && page.nativeList.length === 0
        symbol: Symbols.Apps
        title: page.jobs.installedError.length > 0 ? qsTr("Could Not Read Installed Apps") : qsTr("No Apps Installed")
        text: page.jobs.installedError.length > 0 ? qsTr("Try again in a moment.") : qsTr("Apps you install show up here.")
        actionText: page.jobs.installedError.length > 0 ? qsTr("Try Again") : ""
        actionSymbol: Symbols.Refresh
        onTriggered: page.jobs.refresh()
    }

    // One grouped card, a row per app: icon, name and details, and the
    // app's Remove inside its row at the trailing end. Clicking the row (or
    // Return on it) opens the app's page; Tab moves on to its Remove.
    Section {
        Layout.fillWidth: true
        visible: page.apps.length > 0

        Repeater {
            model: page.apps
            SectionRow {
                id: row
                required property var modelData
                readonly property real iconSide: Math.round(Kirigami.Units.gridUnit * 2.8)

                title: row.modelData.name
                subtitle: qsTr("%1 installation · %2%3").arg(row.modelData.scope).arg(row.modelData.size).arg(row.modelData.version.length > 0 ? " · " + row.modelData.version : "")
                clickable: true
                onClicked: page.appRequested(row.modelData.appId)

                leading: Rectangle {
                    width: row.iconSide
                    height: row.iconSide
                    radius: Math.round(row.iconSide * 0.225)
                    color: icon.status === Image.Ready ? "transparent" : Qt.alpha(TelamonStyle.accent, 0.14)
                    Accessible.ignored: true
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

                // The name bold and both lines elided, as on the app tiles;
                // the row's title and subtitle above are what a screen
                // reader reads.
                content: ColumnLayout {
                    Layout.fillWidth: true
                    Layout.alignment: Qt.AlignVCenter
                    spacing: TelamonStyle.spacingXSmall

                    Text {
                        Layout.fillWidth: true
                        text: row.title
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
                        text: row.subtitle
                        font.family: TelamonStyle.fontFamily
                        font.pointSize: TelamonStyle.fontSizeCaption
                        color: TelamonStyle.textMuted
                        textFormat: Text.PlainText
                        elide: Text.ElideRight
                        Accessible.ignored: true
                    }
                }

                TelamonButton {
                    text: qsTr("Remove")
                    //: A Remove button in a row of the Installed list; %1 is the app's name
                    Accessible.name: qsTr("Remove %1").arg(row.modelData.name)
                    variant: TelamonButton.Destructive
                    enabled: page.idle
                    onClicked: page.removeRequested(row.modelData.appId, row.modelData.name, row.modelData.scope, row.modelData.ref)
                }
            }
        }
    }

    // The Telamon apps the Store installed, below the Flatpak apps.
    Text {
        Layout.fillWidth: true
        Layout.topMargin: TelamonStyle.spacingLarge
        visible: page.nativeList.length > 0
        text: qsTr("Telamon Apps")
        font.family: TelamonStyle.fontFamily
        font.pointSize: TelamonStyle.fontSizeHeading
        font.bold: true
        color: TelamonStyle.text
        textFormat: Text.PlainText
        Accessible.role: Accessible.Heading
    }

    Section {
        Layout.fillWidth: true
        visible: page.nativeList.length > 0

        Repeater {
            model: page.nativeList
            SectionRow {
                id: nRow
                required property var modelData
                readonly property real iconSide: Math.round(Kirigami.Units.gridUnit * 2.8)

                title: nRow.modelData.name
                subtitle: nRow.modelData.present ? [nRow.modelData.state === "update" ? qsTr("%1, update to %2 available").arg(nRow.modelData.installedVersion).arg(nRow.modelData.availableVersion) : nRow.modelData.installedVersion, nRow.modelData.installedSize].filter(t => t.length > 0).join(" \u00b7 ") : qsTr("The app's files are missing. Uninstall it and install it again.")
                clickable: true
                onClicked: page.nativeRequested(nRow.modelData.id)

                leading: Rectangle {
                    width: nRow.iconSide
                    height: nRow.iconSide
                    radius: Math.round(nRow.iconSide * 0.225)
                    color: nIcon.status === Image.Ready ? "transparent" : Qt.alpha(TelamonStyle.accent, 0.14)
                    Accessible.ignored: true
                    Text {
                        anchors.centerIn: parent
                        visible: nIcon.status !== Image.Ready
                        text: nRow.modelData.name.length > 0 ? nRow.modelData.name.charAt(0).toUpperCase() : ""
                        font.family: TelamonStyle.fontFamily
                        font.pointSize: TelamonStyle.fontSizeHeading
                        font.bold: true
                        color: TelamonStyle.accent
                        textFormat: Text.PlainText
                    }
                    Image {
                        id: nIcon
                        anchors.fill: parent
                        source: nRow.modelData.iconSource
                        asynchronous: true
                        cache: true
                        fillMode: Image.PreserveAspectFit
                        sourceSize: Qt.size(nRow.iconSide * Screen.devicePixelRatio, nRow.iconSide * Screen.devicePixelRatio)
                    }
                }

                content: ColumnLayout {
                    Layout.fillWidth: true
                    Layout.alignment: Qt.AlignVCenter
                    spacing: TelamonStyle.spacingXSmall
                    Text {
                        Layout.fillWidth: true
                        text: nRow.title
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
                        text: nRow.subtitle
                        font.family: TelamonStyle.fontFamily
                        font.pointSize: TelamonStyle.fontSizeCaption
                        color: TelamonStyle.textMuted
                        textFormat: Text.PlainText
                        elide: Text.ElideRight
                        Accessible.ignored: true
                    }
                }

                RowLayout {
                    spacing: TelamonStyle.spacing
                    TelamonButton {
                        text: qsTr("Open")
                        //: An Open button in a row of the Telamon apps list; %1 is the app's name
                        Accessible.name: qsTr("Open %1").arg(nRow.modelData.name)
                        enabled: page.nativeIdle && nRow.modelData.present
                        onClicked: {
                            page.openingNative = nRow.modelData.id;
                            ActivationToken.request(page.Window.window, "native:" + nRow.modelData.id);
                        }
                    }
                    TelamonButton {
                        text: qsTr("Uninstall")
                        //: An Uninstall button in a row of the Telamon apps list; %1 is the app's name
                        Accessible.name: qsTr("Uninstall %1").arg(nRow.modelData.name)
                        variant: TelamonButton.Destructive
                        enabled: page.nativeIdle
                        onClicked: page.removeNativeRequested(nRow.modelData.id, nRow.modelData.name)
                    }
                }
            }
        }
    }

    // The AppImages the Store installed, below the Flatpak apps.
    Text {
        Layout.fillWidth: true
        Layout.topMargin: TelamonStyle.spacingLarge
        visible: page.appImageList.length > 0
        text: qsTr("AppImages")
        font.family: TelamonStyle.fontFamily
        font.pointSize: TelamonStyle.fontSizeHeading
        font.bold: true
        color: TelamonStyle.text
        textFormat: Text.PlainText
        Accessible.role: Accessible.Heading
    }

    Section {
        Layout.fillWidth: true
        visible: page.appImageList.length > 0

        Repeater {
            model: page.appImageList
            SectionRow {
                id: aRow
                required property var modelData
                readonly property real iconSide: Math.round(Kirigami.Units.gridUnit * 2.8)

                title: aRow.modelData.name
                subtitle: aRow.modelData.present ? [aRow.modelData.version, aRow.modelData.size].filter(t => t.length > 0).join(" \u00b7 ") : qsTr("The file is gone. Remove this entry.")

                leading: Rectangle {
                    width: aRow.iconSide
                    height: aRow.iconSide
                    radius: Math.round(aRow.iconSide * 0.225)
                    color: aIcon.status === Image.Ready ? "transparent" : Qt.alpha(TelamonStyle.accent, 0.14)
                    Accessible.ignored: true
                    Text {
                        anchors.centerIn: parent
                        visible: aIcon.status !== Image.Ready
                        text: aRow.modelData.name.length > 0 ? aRow.modelData.name.charAt(0).toUpperCase() : ""
                        font.family: TelamonStyle.fontFamily
                        font.pointSize: TelamonStyle.fontSizeHeading
                        font.bold: true
                        color: TelamonStyle.accent
                        textFormat: Text.PlainText
                    }
                    Image {
                        id: aIcon
                        anchors.fill: parent
                        source: aRow.modelData.iconSource
                        asynchronous: true
                        cache: true
                        fillMode: Image.PreserveAspectFit
                        sourceSize: Qt.size(aRow.iconSide * Screen.devicePixelRatio, aRow.iconSide * Screen.devicePixelRatio)
                    }
                }

                content: ColumnLayout {
                    Layout.fillWidth: true
                    Layout.alignment: Qt.AlignVCenter
                    spacing: TelamonStyle.spacingXSmall
                    Text {
                        Layout.fillWidth: true
                        text: aRow.title
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
                        text: aRow.subtitle
                        font.family: TelamonStyle.fontFamily
                        font.pointSize: TelamonStyle.fontSizeCaption
                        color: TelamonStyle.textMuted
                        textFormat: Text.PlainText
                        elide: Text.ElideRight
                        Accessible.ignored: true
                    }
                }

                RowLayout {
                    spacing: TelamonStyle.spacing
                    TelamonButton {
                        text: qsTr("Open")
                        //: An Open button in a row of the AppImages list; %1 is the app's name
                        Accessible.name: qsTr("Open %1").arg(aRow.modelData.name)
                        enabled: page.appImagesIdle && aRow.modelData.present
                        onClicked: {
                            page.openingId = aRow.modelData.id;
                            ActivationToken.request(page.Window.window, "appimage:" + aRow.modelData.id);
                        }
                    }
                    TelamonButton {
                        text: qsTr("Uninstall")
                        //: An Uninstall button in a row of the AppImages list; %1 is the app's name
                        Accessible.name: qsTr("Uninstall %1").arg(aRow.modelData.name)
                        variant: TelamonButton.Destructive
                        enabled: page.appImagesIdle
                        onClicked: page.removeAppImageRequested(aRow.modelData.id, aRow.modelData.name)
                    }
                }
            }
        }
    }
}
