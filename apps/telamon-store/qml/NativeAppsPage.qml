pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import Telamon.Ui

// Telamon Apps: every app connected to the Store (src/native.rs), whether or
// not it is installed. Opened from the Telamon Apps tile on Home.
TelamonPage {
    id: page

    required property var nativeApps

    title: qsTr("Telamon Apps")

    signal appRequested(string appId)

    NativeAppsList {
        id: list
        json: page.nativeApps.appsJson
    }

    Text {
        Layout.fillWidth: true
        text: qsTr("Apps made for Telamon OS that are not part of the system. They install for your user only and update here.")
        wrapMode: Text.Wrap
        font.family: TelamonStyle.fontFamily
        font.pointSize: TelamonStyle.fontSizeBody
        color: TelamonStyle.textMuted
        textFormat: Text.PlainText
    }

    TelamonSpinner {
        Layout.alignment: Qt.AlignHCenter
        visible: !page.nativeApps.ready
        running: visible
    }

    TelamonEmptyState {
        Layout.fillWidth: true
        Layout.preferredHeight: Kirigami.Units.gridUnit * 14
        visible: page.nativeApps.ready && list.tiles.length === 0
        symbol: Symbols.Widgets
        title: qsTr("No Telamon Apps Yet")
        text: page.nativeApps.noteText.length > 0 ? page.nativeApps.noteText : qsTr("Apps that are connected to the Store show up here.")
        actionText: qsTr("Check Again")
        actionSymbol: Symbols.Refresh
        onTriggered: page.nativeApps.check(true)
    }

    GridLayout {
        Layout.fillWidth: true
        visible: list.tiles.length > 0
        columns: Math.max(1, Math.floor(width / (Kirigami.Units.gridUnit * 18)))
        columnSpacing: TelamonStyle.spacing
        rowSpacing: TelamonStyle.spacing

        Repeater {
            model: list.tiles
            AppTile {
                required property var modelData
                Layout.fillWidth: true
                appId: modelData.appId
                name: modelData.name
                summary: modelData.summary
                developer: modelData.developer
                iconSource: modelData.iconSource
                letter: true
                onClicked: page.appRequested(modelData.appId)
            }
        }
    }
}
