// The small Darwin probes the sampler and the environment collector
// read (plan M0.10): main-thread CPU time, `phys_footprint`, and the
// sysctl strings that name the machine. macOS-only — the app target
// never builds them for Linux.

import Foundation

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
                task_info(mach_task_self_, TASK_VM_INFO, rebound, &count)
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
        return String(cString: buffer)
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
