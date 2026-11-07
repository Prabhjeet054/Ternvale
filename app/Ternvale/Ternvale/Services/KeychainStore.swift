import Foundation
import Security
import os

/// Windows password / product key in the login Keychain. Values are never logged.
enum KeychainStore {
    private static let log = Logger(subsystem: "com.ternvale.app", category: "keychain")
    private static let service = "com.ternvale.vm"

    static func passwordAccount(vm: String) -> String { "\(vm)/windows-password" }
    static func productKeyAccount(vm: String) -> String { "\(vm)/windows-product-key" }

    static func store(account: String, secret: String) throws {
        let payload = Data(secret.utf8)
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
        SecItemDelete(query as CFDictionary)
        var add = query
        add[kSecValueData as String] = payload
        add[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        let status = SecItemAdd(add as CFDictionary, nil)
        guard status == errSecSuccess else {
            log.error("keychain store failed status=\(status, privacy: .public) account=\(account, privacy: .public)")
            throw KeychainError.storeFailed(status)
        }
        log.info("stored Keychain item account=\(account, privacy: .public)")
    }

    enum KeychainError: LocalizedError {
        case storeFailed(OSStatus)
        var errorDescription: String? {
            switch self {
            case .storeFailed(let status):
                "Could not store the password in Keychain (status \(status))."
            }
        }
    }
}
