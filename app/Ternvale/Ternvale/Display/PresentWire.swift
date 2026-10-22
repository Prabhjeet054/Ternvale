import Foundation

/// TVDP wire format (`ternvale-gpu/src/present_protocol.rs`).
enum PresentWire {
    static let magic: UInt32 = 0x5044_5654
    static let version: UInt16 = 2
    static let frameHeaderLen = 40
    static let messageLen = 16
    static let maxDimension: UInt32 = 16_384

    static let kindFrame: UInt16 = 0x0001
    static let kindHello: UInt16 = 0x0101
    static let kindPresented: UInt16 = 0x0102
    static let kindKey: UInt16 = 0x0201
    static let kindPointer: UInt16 = 0x0202
    static let kindButton: UInt16 = 0x0203
    static let kindScroll: UInt16 = 0x0204
    static let kindSetSize: UInt16 = 0x0205
    static let pointerMax = 32767.0
}

struct PresentFrameHeader: Equatable {
    var width: UInt32
    var height: UInt32
    var stride: UInt32
    var format: UInt32
    var sequence: UInt64
    var payloadLen: UInt32
}

enum PresentWireError: Error {
    case shortRead
    case badMagic(UInt32)
    case badVersion(UInt16)
    case badKind(UInt16)
    case badGeometry
}

enum PresentWireCodec {
    static func decodeFrameHeader(_ raw: [UInt8]) throws -> PresentFrameHeader {
        guard raw.count >= PresentWire.frameHeaderLen else { throw PresentWireError.shortRead }
        let magic = le32(raw, 0)
        guard magic == PresentWire.magic else { throw PresentWireError.badMagic(magic) }
        let version = le16(raw, 4)
        guard version == PresentWire.version else { throw PresentWireError.badVersion(version) }
        guard le16(raw, 6) == PresentWire.kindFrame else {
            throw PresentWireError.badKind(le16(raw, 6))
        }
        let h = PresentFrameHeader(
            width: le32(raw, 8),
            height: le32(raw, 12),
            stride: le32(raw, 16),
            format: le32(raw, 20),
            sequence: le64(raw, 24),
            payloadLen: le32(raw, 32)
        )
        guard h.width <= PresentWire.maxDimension, h.height <= PresentWire.maxDimension,
              UInt64(h.stride) == UInt64(h.width) * 4,
              UInt64(h.payloadLen) == UInt64(h.stride) * UInt64(h.height)
        else { throw PresentWireError.badGeometry }
        return h
    }

    static func encodeMessage(kind: UInt16, value: UInt64) -> [UInt8] {
        var msg = [UInt8]()
        msg += withUnsafeBytes(of: PresentWire.magic.littleEndian, Array.init)
        msg += withUnsafeBytes(of: PresentWire.version.littleEndian, Array.init)
        msg += withUnsafeBytes(of: kind.littleEndian, Array.init)
        msg += withUnsafeBytes(of: value.littleEndian, Array.init)
        return msg
    }

    static func le16(_ b: [UInt8], _ at: Int) -> UInt16 {
        UInt16(b[at]) | UInt16(b[at + 1]) << 8
    }

    static func le32(_ b: [UInt8], _ at: Int) -> UInt32 {
        UInt32(le16(b, at)) | UInt32(le16(b, at + 2)) << 16
    }

    static func le64(_ b: [UInt8], _ at: Int) -> UInt64 {
        UInt64(le32(b, at)) | UInt64(le32(b, at + 4)) << 32
    }
}
