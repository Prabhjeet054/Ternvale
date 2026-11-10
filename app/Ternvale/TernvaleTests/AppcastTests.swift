import CryptoKit
import XCTest
@testable import Ternvale

final class AppcastTests: XCTestCase {
    func testParsesSignedStableAndBetaItems() throws {
        let xml = """
        <?xml version="1.0" encoding="utf-8"?>
        <rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
          <channel>
            <item>
              <title>0.2.0</title>
              <sparkle:version>2</sparkle:version>
              <sparkle:shortVersionString>0.2.0</sparkle:shortVersionString>
              <enclosure url="https://updates.ternvale.dev/Ternvale-0.2.0.zip" sparkle:edSignature="AAAA" length="12" type="application/octet-stream" />
            </item>
            <item>
              <title>0.3.0-beta.1</title>
              <sparkle:version>3</sparkle:version>
              <sparkle:shortVersionString>0.3.0-beta.1</sparkle:shortVersionString>
              <sparkle:channel>beta</sparkle:channel>
              <enclosure url="https://updates.ternvale.dev/Ternvale-0.3.0-beta.1.zip" sparkle:edSignature="BBBB" length="13" type="application/octet-stream" />
            </item>
          </channel>
        </rss>
        """
        let feed = try SparkleAppcast.parse(xml)
        XCTAssertEqual(feed.items.count, 2)
        XCTAssertEqual(feed.items[0].shortVersion, "0.2.0")
        XCTAssertNil(feed.items[0].channel)
        XCTAssertEqual(feed.items[1].channel, "beta")
        try SparkleAppcast.requireSignatures(feed)
    }

    func testUnsignedEnclosureIsRejected() {
        let xml = """
        <rss><channel><item>
          <sparkle:shortVersionString>9.9.9</sparkle:shortVersionString>
          <sparkle:version>99</sparkle:version>
          <enclosure url="https://evil.example/Ternvale.zip" length="1" type="application/octet-stream" />
        </item></channel></rss>
        """
        XCTAssertThrowsError(try SparkleAppcast.parse(xml)) { error in
            XCTAssertEqual(error as? SparkleAppcast.ParseError, .missingEdSignature)
        }
    }

    func testEmptySignatureIsRejected() throws {
        let xml = """
        <rss><channel><item>
          <sparkle:shortVersionString>1.0.0</sparkle:shortVersionString>
          <sparkle:version>1</sparkle:version>
          <enclosure url="https://updates.ternvale.dev/x.zip" sparkle:edSignature="   " length="1" />
        </item></channel></rss>
        """
        let feed = try SparkleAppcast.parse(xml)
        XCTAssertThrowsError(try SparkleAppcast.requireSignatures(feed)) { error in
            XCTAssertEqual(error as? SparkleAppcast.ParseError, .emptySignature)
        }
    }

    func testFindsNewerVersionAndRejectsTamperedArchive() throws {
        let privateKey = Curve25519.Signing.PrivateKey()
        let pub = privateKey.publicKey.rawRepresentation.base64EncodedString()
        let zip = Data("Ternvale.app higher-version payload".utf8)
        let sig = try privateKey.signature(for: zip).base64EncodedString()
        let xml = """
        <rss><channel><item>
          <sparkle:shortVersionString>0.2.0</sparkle:shortVersionString>
          <sparkle:version>2</sparkle:version>
          <enclosure url="http://127.0.0.1/Ternvale-0.2.0.zip" sparkle:edSignature="\(sig)" length="\(zip.count)" />
        </item></channel></rss>
        """
        let feed = try SparkleAppcast.parse(xml)
        XCTAssertTrue(SemVer.isNewer(feed.items[0].shortVersion, than: "0.1.0"))
        XCTAssertTrue(
            SparkleAppcast.shouldInstall(
                item: feed.items[0],
                archive: zip,
                currentVersion: "0.1.0",
                publicKeyBase64: pub
            )
        )
        var tampered = zip
        tampered[0] ^= 0xFF
        XCTAssertFalse(
            SparkleAppcast.verifyArchive(tampered, edSignature: sig, publicKeyBase64: pub)
        )
        XCTAssertFalse(
            SparkleAppcast.shouldInstall(
                item: feed.items[0],
                archive: tampered,
                currentVersion: "0.1.0",
                publicKeyBase64: pub
            )
        )
    }

    func testChannelFeedURLs() {
        XCTAssertEqual(
            SparkleAppcast.feedURL(channel: "stable")?.absoluteString,
            "https://updates.ternvale.dev/appcast.xml"
        )
        XCTAssertEqual(
            SparkleAppcast.feedURL(channel: "beta")?.absoluteString,
            "https://updates.ternvale.dev/appcast-beta.xml"
        )
    }
}
