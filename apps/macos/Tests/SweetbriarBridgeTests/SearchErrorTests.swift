import Testing
@testable import SweetbriarBridge

@Test
func searchParseErrorsPreserveEveryCodeAndUtf8ByteOffset() {
    let cases: [(CoreFailure, CoreFailure)] = [
        (CoreFailure(.SearchParse(start: 3, end: 5, code: .unexpectedToken)),
         .searchParse(start: 3, end: 5, code: .unexpectedToken)),
        (CoreFailure(.SearchParse(start: 3, end: 5, code: .missingValue)),
         .searchParse(start: 3, end: 5, code: .missingValue)),
        (CoreFailure(.SearchParse(start: 3, end: 5, code: .unclosedQuote)),
         .searchParse(start: 3, end: 5, code: .unclosedQuote)),
        (CoreFailure(.SearchParse(start: 3, end: 5, code: .unclosedGroup)),
         .searchParse(start: 3, end: 5, code: .unclosedGroup)),
        (CoreFailure(.SearchParse(start: 3, end: 5, code: .unknownKey)),
         .searchParse(start: 3, end: 5, code: .unknownKey)),
        (CoreFailure(.SearchParse(start: 3, end: 5, code: .invalidOperator)),
         .searchParse(start: 3, end: 5, code: .invalidOperator)),
        (CoreFailure(.SearchParse(start: 3, end: 5, code: .invalidValue)),
         .searchParse(start: 3, end: 5, code: .invalidValue)),
        (CoreFailure(.SearchParse(start: 3, end: 5, code: .limit)),
         .searchParse(start: 3, end: 5, code: .limit)),
    ]
    for (actual, expected) in cases {
        #expect(actual == expected)
        #expect(actual.errorDescription?.contains("bytes [3,5)") == true)
    }
    #expect(CoreFailure(.InvalidInput(field: "scope", message: "bad")) ==
            .invalidInput(field: "scope", message: "bad"))
    #expect(CoreFailure(.ResetRequired(generation: 7)) == .resetRequired(generation: 7))
}
