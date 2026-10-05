pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Layouts
import QtQuick.Templates as T
import org.kde.kirigami as Kirigami
import Atlas.Ui

// One app in a grid: icon, name, summary and developer. Made to be cheap
// (a grid shows thousands): one asynchronous, size-limited image and a few
// texts, no install button (installing asks for confirmation on the app's
// own page). Every text is catalog text, so all of it is plain. The card
// emits `clicked`.
T.AbstractButton {
    id: control

    property string appId
    property string name
    property string summary
    property string developer
    // A `file:` URL or "".
    property string iconSource
    property bool verified: false
    property string sourceTitle

    readonly property real iconSide: Math.round(Kirigami.Units.gridUnit * 3.4)

    implicitWidth: Kirigami.Units.gridUnit * 18
    implicitHeight: Math.round(Kirigami.Units.gridUnit * 3.4) + topPadding + bottomPadding
    padding: AtlasStyle.spacingLarge
    hoverEnabled: true
    focusPolicy: Qt.StrongFocus

    Accessible.role: Accessible.Button
    Accessible.name: control.name
    Accessible.description: [control.summary, control.developer].filter(s => s.length > 0).join(", ")

    Keys.onReturnPressed: event => {
        if (!event.isAutoRepeat) {
            control.clicked();
        }
    }
    Keys.onEnterPressed: event => {
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

        Rectangle {
            Layout.preferredWidth: control.iconSide
            Layout.preferredHeight: control.iconSide
            Layout.alignment: Qt.AlignVCenter
            radius: Math.round(control.iconSide * 0.225)
            color: icon.status === Image.Ready ? "transparent" : Qt.alpha(AtlasStyle.accent, 0.14)
            Image {
                id: icon
                anchors.fill: parent
                source: control.iconSource
                asynchronous: true
                cache: true
                fillMode: Image.PreserveAspectFit
                // Decoded at the size shown (a large SVG or PNG costs nothing
                // more), sharp on a scaled screen.
                sourceSize: Qt.size(control.iconSide * Screen.devicePixelRatio, control.iconSide * Screen.devicePixelRatio)
            }
        }

        ColumnLayout {
            Layout.fillWidth: true
            Layout.alignment: Qt.AlignVCenter
            spacing: Math.round(AtlasStyle.spacingSmall / 2)

            Text {
                Layout.fillWidth: true
                text: control.name
                font.family: AtlasStyle.fontFamily
                font.pointSize: AtlasStyle.fontSizeBody
                font.bold: true
                color: Kirigami.Theme.textColor
                textFormat: Text.PlainText
                elide: Text.ElideRight
            }
            Text {
                Layout.fillWidth: true
                visible: text.length > 0
                text: control.summary
                font.family: AtlasStyle.fontFamily
                font.pointSize: AtlasStyle.fontSizeCaption
                color: AtlasStyle.textMuted
                textFormat: Text.PlainText
                elide: Text.ElideRight
            }
            Text {
                Layout.fillWidth: true
                visible: text.length > 0
                text: control.verified ? qsTr("%1 · Verified").arg(control.developer) : control.developer
                font.family: AtlasStyle.fontFamily
                font.pointSize: AtlasStyle.fontSizeCaption
                color: control.verified ? AtlasStyle.success : AtlasStyle.textMuted
                textFormat: Text.PlainText
                elide: Text.ElideRight
            }
        }
    }
}
