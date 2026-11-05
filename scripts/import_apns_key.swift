import Foundation
import Security

guard CommandLine.arguments.count == 2,
      CommandLine.arguments[1].range(of: "^[A-Z0-9]{10}$", options: .regularExpression) != nil else {
    fputs("Usage: swift scripts/import_apns_key.swift KEY_ID < private-key.p8\n", stderr)
    exit(2)
}
let key = FileHandle.standardInput.readDataToEndOfFile()
guard !key.isEmpty, key.count <= 16 * 1024 else { exit(2) }
let query: [CFString: Any] = [
    kSecClass: kSecClassGenericPassword,
    kSecAttrService: "org.e47.xedoc.apns",
    kSecAttrAccount: CommandLine.arguments[1],
]
var trusted: SecTrustedApplication?
guard SecTrustedApplicationCreateFromPath("/usr/bin/security", &trusted) == errSecSuccess,
      let trusted else { exit(3) }
var access: SecAccess?
guard SecAccessCreate("Xedoc APNs signing key" as CFString, [trusted] as CFArray, &access) == errSecSuccess,
      let access else { exit(4) }
var attributes = query
attributes[kSecValueData] = key
attributes[kSecAttrLabel] = "Xedoc APNs \(CommandLine.arguments[1])"
attributes[kSecAttrAccess] = access
let status = SecItemAdd(attributes as CFDictionary, nil)
guard status == errSecSuccess else {
    fputs("Keychain import failed: \(status). Existing keys are not overwritten.\n", stderr)
    exit(5)
}
print("APNs key imported into Keychain.")
