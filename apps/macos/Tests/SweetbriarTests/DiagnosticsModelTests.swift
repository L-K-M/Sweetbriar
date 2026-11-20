import SweetbriarBridge
import Testing
@testable import Sweetbriar

/// The diagnostics preview's model (spec/10 "Privacy", spec/07
/// `diagnostics_package`): the built text shows, a failure shows
/// instead of an empty document, and the shell's versions reach the
/// core.
@MainActor
@Suite("Diagnostics model")
struct DiagnosticsModelTests {
    @Test func aBuiltPackageShowsItsText() async {
        let source = FakeDiagnosticsSource(text: "{\"schema_version\":1}")
        let model = DiagnosticsModel(source: source)

        await model.load()

        #expect(model.text == "{\"schema_version\":1}")
        #expect(model.error == nil)
        #expect(model.isLoading == false)
    }

    @Test func aFailedPackageShowsItsError() async {
        let source = FakeDiagnosticsSource(
            failure: CoreFailure.internalError(detail: "no package"))
        let model = DiagnosticsModel(source: source)

        await model.load()

        #expect(model.text == nil)
        #expect(model.error?.contains("no package") == true)
        #expect(model.isLoading == false)
    }

    @Test func theSourceReceivesTheShellVersions() async throws {
        let source = FakeDiagnosticsSource(text: "{}")
        let model = DiagnosticsModel(source: source)

        await model.load()

        let call = try #require(await source.calls.first)
        #expect(call.appVersion == DiagnosticsModel.appVersion)
        #expect(call.osVersion == DiagnosticsModel.osVersion)
    }
}

private actor FakeDiagnosticsSource: DiagnosticsSource {
    struct Call: Equatable {
        let appVersion: String
        let osVersion: String
    }

    private let text: String?
    private let failure: (any Error)?
    private(set) var calls: [Call] = []

    init(text: String) {
        self.text = text
        self.failure = nil
    }

    init(failure: any Error) {
        self.text = nil
        self.failure = failure
    }

    func diagnosticsPackage(appVersion: String, osVersion: String) async throws -> String {
        calls.append(Call(appVersion: appVersion, osVersion: osVersion))
        if let failure { throw failure }
        return text ?? ""
    }
}
