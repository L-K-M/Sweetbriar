// M0.9 shell: NSApplication by hand (no @main — the delegate's
// lifetime must outlive this scope) so `applicationShouldTerminate`
// can answer `.terminateLater` while the core shuts down (spec/07
// "Lifecycle").

import AppKit

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.setActivationPolicy(.regular)
app.run()
