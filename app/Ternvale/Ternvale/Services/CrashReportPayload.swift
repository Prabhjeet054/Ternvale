import Foundation

/// Exactly what the preview lists and what `CrashReporter` may POST.
struct CrashReportPayload: Equatable {
    struct Item: Equatable, Identifiable {
        let id: String
        let name: String
        /// `file` or `field`.
        let kind: String
        let included: Bool
        let bytes: Int?
        /// Why a file is omitted (serial, guest disk, …).
        let reason: String?
        let redactedPreview: String?
    }

    let appVersion: String
    let items: [Item]

    var includedItems: [Item] { items.filter(\.included) }

    var jsonObject: [String: Any] {
        [
            "app_version": appVersion,
            "files": includedItems.filter { $0.kind == "file" }.map { item in
                ["name": item.name, "bytes": item.bytes ?? 0]
            },
            "fields": Dictionary(
                uniqueKeysWithValues: includedItems.filter { $0.kind == "field" }.map {
                    ($0.name, $0.redactedPreview ?? "")
                }
            ),
        ]
    }

    func jsonData() throws -> Data {
        try JSONSerialization.data(withJSONObject: jsonObject, options: [.prettyPrinted, .sortedKeys])
    }

    /// Build a preview from candidate files and host fields. Serial and guest files stay out.
    static func preview(
        files: [(name: String, bytes: Int, text: String?)],
        fields: [(name: String, value: String)],
        appVersion: String,
        redactor: (String) -> String = { CrashRedactor.redact($0) }
    ) -> CrashReportPayload {
        var items: [Item] = []
        for file in files {
            if CrashRedactor.shouldExclude(filename: file.name) {
                items.append(
                    Item(
                        id: "file:\(file.name)",
                        name: file.name,
                        kind: "file",
                        included: false,
                        bytes: file.bytes,
                        reason: "guest serial or guest file contents (excluded by default)",
                        redactedPreview: nil
                    )
                )
                continue
            }
            let snippet = file.text.map { String(redactor($0).prefix(400)) }
            items.append(
                Item(
                    id: "file:\(file.name)",
                    name: file.name,
                    kind: "file",
                    included: true,
                    bytes: file.bytes,
                    reason: nil,
                    redactedPreview: snippet
                )
            )
        }
        for field in fields {
            items.append(
                Item(
                    id: "field:\(field.name)",
                    name: field.name,
                    kind: "field",
                    included: true,
                    bytes: nil,
                    reason: nil,
                    redactedPreview: redactor(field.value)
                )
            )
        }
        return CrashReportPayload(appVersion: appVersion, items: items)
    }
}
