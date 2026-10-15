// The small Darwin probes the sampler and the environment collector
// read (plan M0.10): main-thread CPU time, `phys_footprint`, and the
// sysctl strings that name the machine. macOS-only — the app target
// never builds them for Linux.

import Foundation
import Synchronization

enum SystemProbes {
    /// Main-thread CPU time in nanoseconds
    /// (`clock_gettime_nsec_np(CLOCK_THREAD_CPUTIME_ID)`) — correct
    /// only on the main thread, which is where the tick runs.
    static func threadCPUNanoseconds() -> UInt64 {
        clock_gettime_nsec_np(CLOCK_THREAD_CPUTIME_ID)
    }

    /// The process's `phys_footprint` — the peak-memory number plan
    /// M0.10 compares.
    static func physicalFootprint() -> UInt64 {
        var info = task_vm_info_data_t()
        var count = mach_msg_type_number_t(
            MemoryLayout<task_vm_info_data_t>.size / MemoryLayout<integer_t>.size
        )
        let result = withUnsafeMutablePointer(to: &info) { pointer in
            pointer.withMemoryRebound(
                to: integer_t.self,
                capacity: Int(count)
            ) { rebound in
                task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), rebound, &count)
            }
        }
        return result == KERN_SUCCESS ? info.phys_footprint : 0
    }

    /// A sysctl string — `hw.model`, `machdep.cpu.brand_string`.
    static func sysctlString(_ name: String) -> String {
        var size = 0
        sysctlbyname(name, nil, &size, nil, 0)
        guard size > 0 else { return "" }
        var buffer = [CChar](repeating: 0, count: size)
        sysctlbyname(name, &buffer, &size, nil, 0)
        let bytes = buffer.prefix { $0 != 0 }.map { UInt8(bitPattern: $0) }
        return String(decoding: bytes, as: UTF8.self)
    }

    /// A sysctl `int` — `kern.hv_vmm_present` (nonzero: the runner is
    /// virtualized).
    static func sysctlInt(_ name: String) -> Int32 {
        var value: Int32 = 0
        var size = MemoryLayout<Int32>.size
        sysctlbyname(name, &value, &size, nil, 0)
        return value
    }
}

/// The process's peak footprint over a run, sampled every few
/// milliseconds off the main thread: a peak inside a main-thread
/// stall, which no display tick sees, still counts (plan M0.10: peak
/// memory). `@unchecked Sendable`: the peak is behind its mutex, and
/// the timer is only started and stopped, from the main actor.
final class FootprintSampler: @unchecked Sendable {
    private static let interval = DispatchTimeInterval.milliseconds(5)

    private let peak = Mutex<UInt64>(0)
    private let timer = DispatchSource.makeTimerSource(
        queue: .global(qos: .userInitiated)
    )

    init() {
        timer.schedule(deadline: .now(), repeating: Self.interval)
        timer.setEventHandler { [weak self] in
            self?.sample()
        }
    }

    /// Begin sampling; pair with one `stop()`.
    func start() {
        timer.resume()
    }

    /// Stop sampling and answer the peak seen.
    func stop() -> UInt64 {
        timer.cancel()
        return peak.withLock { $0 }
    }

    private func sample() {
        let now = SystemProbes.physicalFootprint()
        peak.withLock { $0 = max($0, now) }
    }
}

