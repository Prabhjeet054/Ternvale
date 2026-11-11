import Foundation
import Virtualization

private func cString(_ ptr: UnsafePointer<CChar>?) -> String {
    guard let ptr else { return "" }
    return String(cString: ptr)
}

private func splitDisks(_ raw: String) -> [String] {
    if raw.isEmpty { return [] }
    return raw.split(separator: "\n", omittingEmptySubsequences: true).map(String.init)
}

@_cdecl("tv_vz_create")
func tv_vz_create(
    _ name: UnsafePointer<CChar>?,
    _ cpus: UInt32,
    _ ramBytes: UInt64,
    _ kernel: UnsafePointer<CChar>?,
    _ initrd: UnsafePointer<CChar>?,
    _ cmdline: UnsafePointer<CChar>?,
    _ nvram: UnsafePointer<CChar>?,
    _ serialLog: UnsafePointer<CChar>?,
    _ udsDir: UnsafePointer<CChar>?,
    _ ports: UnsafePointer<UInt32>?,
    _ portCount: UInt32,
    _ disks: UnsafePointer<CChar>?,
    _ diskRo: UnsafePointer<UInt8>?,
    _ diskCount: UInt32,
    _ attachVsock: UInt8,
    _ attachNat: UInt8,
    _ cb: tv_vz_state_fn?,
    _ ctx: UnsafeMutableRawPointer?,
    _ err: UnsafeMutablePointer<CChar>?,
    _ errLen: Int
) -> Int32 {
    let vmName = cString(name)
    let session = TvVzSession(name: vmName)
    session.stateCb = cb
    session.ctx = ctx

    do {
        let config = try buildConfig(
            session: session,
            cpus: cpus,
            ramBytes: ramBytes,
            kernel: cString(kernel),
            initrd: cString(initrd),
            cmdline: cString(cmdline),
            nvram: cString(nvram),
            serialLog: cString(serialLog),
            udsDir: cString(udsDir),
            ports: ports,
            portCount: Int(portCount),
            diskPaths: splitDisks(cString(disks)),
            diskRo: diskRo,
            diskCount: Int(diskCount),
            attachVsock: attachVsock != 0,
            attachNat: attachNat != 0
        )
        // VZVirtualMachine.init(configuration:queue:) — macOS 12.0.
        let vm = VZVirtualMachine(configuration: config, queue: session.queue)
        vm.delegate = session
        vm.addObserver(session, forKeyPath: "state", options: [.new], context: nil)
        session.vm = vm
        session.udsDir = cString(udsDir)
        if let ports, portCount > 0 {
            session.vsockPorts = (0..<Int(portCount)).map { ports[$0] }
        }
        attachVsockListeners(session: session, udsDir: session.udsDir, ports: session.vsockPorts)
        let id = Registry.put(session)
        TvLog.emit(.info, "vm=\(vmName) vz created handle=\(id) cpus=\(cpus) ram=\(ramBytes)")
        return id
    } catch {
        tvCopyError(error.localizedDescription, err, errLen)
        TvLog.emit(.error, "vm=\(vmName) vz create failed \(error.localizedDescription)")
        return -1
    }
}

@_cdecl("tv_vz_start")
func tv_vz_start(
    _ handle: Int32,
    _ err: UnsafeMutablePointer<CChar>?,
    _ errLen: Int
) -> Int32 {
    guard let session = Registry.get(handle), let vm = session.vm else {
        tvCopyError("invalid vz handle", err, errLen)
        return -1
    }
    let sem = DispatchSemaphore(value: 0)
    var fail: String?
    session.queue.async {
        // start(completionHandler:) — macOS 11.0; must run on session.queue.
        vm.start { result in
            switch result {
            case .success:
                session.emit(.running, detail: "start")
            case .failure(let error):
                fail = error.localizedDescription
                session.emit(.failed, detail: "start \(error.localizedDescription)")
            }
            sem.signal()
        }
    }
    sem.wait()
    if let fail {
        tvCopyError(fail, err, errLen)
        return -1
    }
    return 0
}

@_cdecl("tv_vz_pause")
func tv_vz_pause(
    _ handle: Int32,
    _ err: UnsafeMutablePointer<CChar>?,
    _ errLen: Int
) -> Int32 {
    guard let session = Registry.get(handle), let vm = session.vm else {
        tvCopyError("invalid vz handle", err, errLen)
        return -1
    }
    let sem = DispatchSemaphore(value: 0)
    var fail: String?
    session.queue.async {
        // pause(completionHandler:) — macOS 12.0.
        vm.pause { result in
            switch result {
            case .success:
                session.emit(.paused, detail: "pause")
            case .failure(let error):
                fail = error.localizedDescription
            }
            sem.signal()
        }
    }
    sem.wait()
    if let fail {
        tvCopyError(fail, err, errLen)
        return -1
    }
    return 0
}

@_cdecl("tv_vz_resume")
func tv_vz_resume(
    _ handle: Int32,
    _ err: UnsafeMutablePointer<CChar>?,
    _ errLen: Int
) -> Int32 {
    guard let session = Registry.get(handle), let vm = session.vm else {
        tvCopyError("invalid vz handle", err, errLen)
        return -1
    }
    let sem = DispatchSemaphore(value: 0)
    var fail: String?
    session.queue.async {
        // resume(completionHandler:) — macOS 12.0.
        vm.resume { result in
            switch result {
            case .success:
                session.emit(.running, detail: "resume")
            case .failure(let error):
                fail = error.localizedDescription
            }
            sem.signal()
        }
    }
    sem.wait()
    if let fail {
        tvCopyError(fail, err, errLen)
        return -1
    }
    return 0
}

@_cdecl("tv_vz_stop")
func tv_vz_stop(
    _ handle: Int32,
    _ force: UInt8,
    _ err: UnsafeMutablePointer<CChar>?,
    _ errLen: Int
) -> Int32 {
    guard let session = Registry.get(handle), let vm = session.vm else {
        tvCopyError("invalid vz handle", err, errLen)
        return -1
    }
    let sem = DispatchSemaphore(value: 0)
    var fail: String?
    session.queue.async {
        if force != 0 {
            // stop(completionHandler:) — macOS 12.0.
            vm.stop { error in
                if let error {
                    fail = error.localizedDescription
                } else {
                    session.emit(.stopped, detail: "force-stop")
                }
                sem.signal()
            }
        } else {
            do {
                // requestStop() — macOS 11.0.
                try vm.requestStop()
                session.emit(.stopping, detail: "requestStop")
            } catch {
                fail = error.localizedDescription
            }
            sem.signal()
        }
    }
    sem.wait()
    if let fail {
        tvCopyError(fail, err, errLen)
        return -1
    }
    return 0
}

@_cdecl("tv_vz_state")
func tv_vz_state(_ handle: Int32) -> Int32 {
    guard let session = Registry.get(handle) else { return TvVmState.failed.rawValue }
    return session.lastState.rawValue
}

@_cdecl("tv_vz_destroy")
func tv_vz_destroy(_ handle: Int32) {
    guard let session = Registry.take(handle), let vm = session.vm else { return }
    session.queue.sync {
        vm.removeObserver(session, forKeyPath: "state")
        vm.delegate = nil
        session.vm = nil
    }
    TvLog.emit(.info, "vm=\(session.name) vz destroyed handle=\(handle)")
}

private func buildConfig(
    session: TvVzSession,
    cpus: UInt32,
    ramBytes: UInt64,
    kernel: String,
    initrd: String,
    cmdline: String,
    nvram: String,
    serialLog: String,
    udsDir: String,
    ports: UnsafePointer<UInt32>?,
    portCount: Int,
    diskPaths: [String],
    diskRo: UnsafePointer<UInt8>?,
    diskCount: Int,
    attachVsock: Bool,
    attachNat: Bool
) throws -> VZVirtualMachineConfiguration {
    let config = VZVirtualMachineConfiguration()
    config.cpuCount = Int(cpus)
    config.memorySize = ramBytes
    // VZGenericPlatformConfiguration — macOS 12.0. Linux guests; not VZMac*.
    config.platform = VZGenericPlatformConfiguration()

    if kernel.isEmpty {
        // VZEFIBootLoader — macOS 13.0.
        let boot = VZEFIBootLoader()
        if !nvram.isEmpty {
            let url = URL(fileURLWithPath: nvram)
            if FileManager.default.fileExists(atPath: nvram) {
                boot.variableStore = VZEFIVariableStore(url: url)
            } else {
                boot.variableStore = try VZEFIVariableStore(creatingVariableStoreAt: url)
            }
        }
        config.bootLoader = boot
    } else {
        // VZLinuxBootLoader — macOS 11.0.
        let boot = VZLinuxBootLoader(kernelURL: URL(fileURLWithPath: kernel))
        if !initrd.isEmpty {
            boot.initialRamdiskURL = URL(fileURLWithPath: initrd)
        }
        if !cmdline.isEmpty {
            boot.commandLine = cmdline
        }
        config.bootLoader = boot
    }

    var storage: [VZStorageDeviceConfiguration] = []
    for i in 0..<min(diskCount, diskPaths.count) {
        let ro = diskRo.map { $0[i] != 0 } ?? false
        // VZDiskImageStorageDeviceAttachment — macOS 12.0 caching/sync modes.
        let attachment = try VZDiskImageStorageDeviceAttachment(
            url: URL(fileURLWithPath: diskPaths[i]),
            readOnly: ro,
            cachingMode: .automatic,
            synchronizationMode: .fsync
        )
        let disk = VZVirtioBlockDeviceConfiguration(attachment: attachment)
        storage.append(disk)
    }
    config.storageDevices = storage

    if attachNat {
        let net = VZVirtioNetworkDeviceConfiguration()
        net.attachment = VZNATNetworkDeviceAttachment()
        config.networkDevices = [net]
    }

    config.entropyDevices = [VZVirtioEntropyDeviceConfiguration()]

    if attachVsock {
        let vsock = VZVirtioSocketDeviceConfiguration()
        config.socketDevices = [vsock]
    }

    if !serialLog.isEmpty {
        if !FileManager.default.fileExists(atPath: serialLog) {
            FileManager.default.createFile(atPath: serialLog, contents: nil)
        }
        let writer = FileHandle(forWritingAtPath: serialLog)
        writer?.seekToEndOfFile()
        let reader = FileHandle(forReadingAtPath: "/dev/null")
        if let writer, let reader {
            session.serialWriter = writer
            session.serialReader = reader
            let serial = VZVirtioConsoleDeviceSerialPortConfiguration()
            // VZFileHandleSerialPortAttachment — macOS 11.0.
            serial.attachment = VZFileHandleSerialPortAttachment(
                fileHandleForReading: reader,
                fileHandleForWriting: writer
            )
            config.serialPorts = [serial]
        }
    }

    try config.validate()
    return config
}

/// Attach vsock listeners after the VM exists (device is on the running VM).
func attachVsockListeners(session: TvVzSession, udsDir: String, ports: [UInt32]) {
    guard !udsDir.isEmpty, let vm = session.vm else { return }
    session.queue.async {
        guard let device = vm.socketDevices.first as? VZVirtioSocketDevice else { return }
        for port in ports {
            let path = (udsDir as NSString).appendingPathComponent("host-\(port).sock")
            let proxy = UnixVsockListener(unixPath: path)
            // setSocketListener(_:forPort:) — macOS 11.0.
            device.setSocketListener(proxy.listener, forPort: port)
            session.vsockProxies.append(proxy)
            TvLog.emit(.debug, "vsock listen port=\(port) path=\(path)")
        }
    }
}
