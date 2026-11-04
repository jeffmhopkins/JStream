// Puts the control token, read from stdin, on the macOS clipboard for the tester
// launcher (jstream.sh), with the least reach: this Mac only (no Universal Clipboard),
// marked concealed and transient so clipboard managers skip it, and cleared after two
// minutes unless something else was copied since. Run with `osascript -l JavaScript`.
//
// Docs: docs/runbooks/mac-testing.md#script
ObjC.import("AppKit");

function run() {
    const input = $.NSFileHandle.fileHandleWithStandardInput.readDataToEndOfFile;
    const token = $.NSString.alloc.initWithDataEncoding(input, $.NSUTF8StringEncoding);
    if (token.length === 0) {
        return;
    }
    const board = $.NSPasteboard.generalPasteboard;
    // NSPasteboardContentsCurrentHostOnly (1): clears the board, keeps the copy local.
    board.prepareForNewContentsWithOptions(1);
    board.setStringForType(token, "public.utf8-plain-text");
    // The nspasteboard.org markers clipboard managers and history tools honour.
    board.setStringForType("", "org.nspasteboard.ConcealedType");
    board.setStringForType("", "org.nspasteboard.TransientType");
    const copied = board.changeCount;
    delay(120);
    if (board.changeCount === copied) {
        board.clearContents;
    }
}
