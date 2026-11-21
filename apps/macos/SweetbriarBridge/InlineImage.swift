// Inline images in a Markdown draft (spec/05 "inline images", spec/08
// "Inline images"): their media types, Content-IDs, names and the
// Markdown that references them.
import Foundation

public enum InlineImage {
    /// The media types `save_draft` accepts inline (spec/05).
    public static let mediaTypes: Set<String> = ["image/png", "image/jpeg", "image/gif"]
    /// Pasted image data has no file name; this names it.
    public static let pastedName = "Image"
    /// The domain of every Content-ID the shell mints (spec/05).
    static let contentIdDomain = "sweetbriar.invalid"
    /// CommonMark lets a backslash escape any of these.
    private static let asciiPunctuation = Set(##"!"#$%&'()*+,-./:;<=>?@[\]^_`{|}~"##.unicodeScalars)

    /// A fresh bare Content-ID: a lowercase UUID at `sweetbriar.invalid`.
    public static func mintContentId() -> String {
        "\(UUID().uuidString.lowercased())@\(contentIdDomain)"
    }

    /// The reference's name: the file name without its extension, or
    /// "Image" when that leaves nothing.
    public static func name(forFilename filename: String) -> String {
        let stem = (filename as NSString).deletingPathExtension
        return stem.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? pastedName : stem
    }

    /// `![name](cid:id)`, the name escaped like Markdown text so nothing
    /// in it can end the alt text, start other markup or split the
    /// reference across lines.
    public static func reference(name: String, contentId: String) -> String {
        "![\(escaped(name))](cid:\(contentId))"
    }

    /// Every ASCII punctuation character gets a backslash; line breaks
    /// and other controls become spaces.
    static func escaped(_ text: String) -> String {
        var escaped = String.UnicodeScalarView()
        for scalar in text.unicodeScalars {
            if CharacterSet.controlCharacters.contains(scalar)
                || CharacterSet.newlines.contains(scalar)
            {
                escaped.append(" ")
                continue
            }
            if asciiPunctuation.contains(scalar) { escaped.append("\\") }
            escaped.append(scalar)
        }
        return String(escaped)
    }
}
