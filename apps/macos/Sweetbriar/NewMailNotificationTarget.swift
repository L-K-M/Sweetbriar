import Foundation

/// Where a delivered new-mail notification sends the user: the account
/// its `userInfo` names. Pure, so tests pin the mapping without a
/// notification center or a running app.
enum NewMailNotificationTarget {
    /// The `userInfo` key `NewMailNotification` writes.
    static let accountIDKey = "accountId"

    /// The named account, or nil for a missing, non-string or empty
    /// value; an unknown account is the main window's business.
    static func accountID(from userInfo: [AnyHashable: Any]) -> String? {
        guard let value = userInfo[accountIDKey] as? String, !value.isEmpty else { return nil }
        return value
    }
}
