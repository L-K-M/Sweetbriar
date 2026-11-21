import AppKit

/// The From-switch UI test's seam for the route announcement
/// (spec/11 "UI"; M4.4d3). XCUITest cannot hear `NSAccessibility`
/// announcements, so with `-recordRouteAnnouncements YES` the compose
/// window's default `announceRoute` also writes the message here: an
/// accessibility-only element, identifier `composeRouteAnnouncement`,
/// whose value is the last message. Without the argument no element is
/// created and the announcement is unchanged. Like `-attachPath`, this
/// is a UI-test hook, not a user feature.
@MainActor
final class RouteAnnouncementRecorder: NSView {
    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        setAccessibilityElement(true)
        setAccessibilityRole(.staticText)
        setAccessibilityIdentifier("composeRouteAnnouncement")
        setAccessibilityLabel("Route announcement")
    }

    required init?(coder: NSCoder) { fatalError("RouteAnnouncementRecorder is built in code") }

    /// Record `message` as the element's value.
    func record(_ message: String) {
        setAccessibilityValue(message)
    }
}
