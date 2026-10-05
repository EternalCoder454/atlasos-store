import QtQuick
import QtQuick.Layouts
import QtQuick.Templates as T
import org.kde.kirigami as Kirigami
import Atlas.Ui

// A category on the home page: a symbol, its name and how many apps it has.
T.AbstractButton {
    id: control

    property int symbol: Symbols.Apps
    // -1 while the catalog isn't loaded: no number is shown.
    property int count: -1

    implicitWidth: Kirigami.Units.gridUnit * 11
    implicitHeight: Kirigami.Units.gridUnit * 3.6
    padding: AtlasStyle.spacingLarge
    hoverEnabled: true
    focusPolicy: Qt.StrongFocus

    Accessible.role: Accessible.Button
    Accessible.name: control.text
    Accessible.description: control.count >= 0 ? (control.count === 1 ? qsTr("1 app") : qsTr("%1 apps").arg(control.count)) : ""

    Keys.onReturnPressed: event => {
        if (!event.isAutoRepeat) {
            control.clicked();
        }
    }

    background: Item {
        Rectangle {
            id: card
            anchors.fill: parent
            radius: AtlasStyle.radius
            color: control.down ? AtlasStyle.pressed : control.hovered ? AtlasStyle.hover : AtlasStyle.control
            border.width: 1
            border.color: AtlasStyle.separator
        }
        AtlasFocusRing {
            radius: card.radius + gap
            shown: control.visualFocus
        }
    }

    contentItem: RowLayout {
        spacing: AtlasStyle.spacingLarge
        Symbol {
            icon: control.symbol
            size: Kirigami.Units.iconSizes.medium
            color: AtlasStyle.accent
        }
        ColumnLayout {
            Layout.fillWidth: true
            spacing: 0
            Text {
                Layout.fillWidth: true
                text: control.text
                font.family: AtlasStyle.fontFamily
                font.pointSize: AtlasStyle.fontSizeBody
                font.bold: true
                color: Kirigami.Theme.textColor
                textFormat: Text.PlainText
                elide: Text.ElideRight
            }
            Text {
                Layout.fillWidth: true
                visible: control.count >= 0
                text: (control.count === 1 ? qsTr("1 app") : qsTr("%1 apps").arg(Math.max(0, control.count)))
                font.family: AtlasStyle.fontFamily
                font.pointSize: AtlasStyle.fontSizeCaption
                color: AtlasStyle.textMuted
                textFormat: Text.PlainText
                elide: Text.ElideRight
            }
        }
    }
}
