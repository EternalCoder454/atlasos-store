import QtQuick
import QtQuick.Layouts
import Telamon.Ui

// One source in the Sources list: its name, who it is for, its address, a
// flag when it is not signed or is turned off, the on/off switch at the
// trailing end and a Remove button before it. Every text is plain: the title
// and address come from the source itself.
SectionRow {
    id: row

    // The source (one element of Sources.sourcesJson).
    required property var source
    // False while a job runs: the row (its switch and Remove) is disabled then.
    property bool idle: true

    signal toggled(bool enabled)
    signal removeRequested

    readonly property string scopeText: row.source.scope === "system" ? qsTr("For everyone on this computer (system)") : qsTr("For you only (user)")
    readonly property string installedText: row.source.appCount > 0 ? (row.source.appCount === 1 ? qsTr("1 app installed from it") : qsTr("%1 apps installed from it").arg(row.source.appCount)) : ""

    title: row.source.title
    // What a screen reader reads: everything the row shows.
    subtitle: [row.scopeText, row.source.enabled ? "" : qsTr("Off"), row.installedText, row.source.url, row.source.unsigned ? qsTr("Not signed") : ""].filter(s => s.length > 0).join(" · ")
    showSwitch: true
    switchChecked: row.source.enabled
    // The whole row is off while a job runs, so the switch never moves
    // without the change happening.
    enabled: row.idle
    onSwitchToggled: checked => row.toggled(checked)

    content: ColumnLayout {
        Layout.fillWidth: true
        Layout.alignment: Qt.AlignVCenter
        spacing: TelamonStyle.spacingXSmall

        RowLayout {
            Layout.fillWidth: true
            spacing: TelamonStyle.spacingLarge
            Text {
                Layout.fillWidth: true
                text: row.source.title
                font.family: TelamonStyle.fontFamily
                font.pointSize: TelamonStyle.fontSizeBody
                font.bold: true
                color: TelamonStyle.text
                textFormat: Text.PlainText
                elide: Text.ElideRight
                Accessible.ignored: true
            }
            TelamonBadge {
                visible: !row.source.enabled
                text: qsTr("Off")
            }
            TelamonBadge {
                visible: row.source.unsigned
                text: qsTr("Not signed")
                type: "warning"
            }
        }
        Text {
            Layout.fillWidth: true
            text: row.installedText.length > 0 ? row.scopeText + " · " + row.installedText : row.scopeText
            font.family: TelamonStyle.fontFamily
            font.pointSize: TelamonStyle.fontSizeCaption
            color: TelamonStyle.textMuted
            textFormat: Text.PlainText
            elide: Text.ElideRight
            Accessible.ignored: true
        }
        Text {
            Layout.fillWidth: true
            text: row.source.url
            font.family: TelamonStyle.fontFamily
            font.pointSize: TelamonStyle.fontSizeCaption
            color: TelamonStyle.textMuted
            textFormat: Text.PlainText
            elide: Text.ElideMiddle
            Accessible.ignored: true
        }
    }

    TelamonButton {
        text: qsTr("Remove")
        //: A Remove button in a row of the Sources list; %1 is the source's name
        Accessible.name: qsTr("Remove %1").arg(row.source.title)
        variant: TelamonButton.Destructive
        enabled: row.idle
        onClicked: row.removeRequested()
    }
}
