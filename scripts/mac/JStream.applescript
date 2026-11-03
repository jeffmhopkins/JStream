-- JStream.app (built by scripts/mac/install.sh with `osacompile -s`, a stay-open applet):
-- starts the daemon through start-daemon.sh, copies the control token to the clipboard,
-- opens the web UI in the default browser, and stops the daemon when the app quits.
-- The daemon is the applet's child, so macOS attributes its permission prompts to JStream.
--
-- Docs: docs/runbooks/mac-testing.md#script
--
-- Globals, not properties: an applet saves its properties into itself on quit, which
-- would break its signature.

global daemonPid, uiURL

on run
	set daemonPid to ""
	set uiURL to ""
	set launcher to quoted form of (POSIX path of (path to me) & "Contents/Resources/start-daemon.sh")
	try
		set started to do shell script launcher
	on error message
		display alert "JStream couldn't start" message message as critical
		quit
		return
	end try
	set daemonPid to paragraph 1 of started
	set uiURL to paragraph 2 of started
	set the clipboard to (paragraph 3 of started)
	display notification "Your access token is on the clipboard. When your browser asks you to sign in, paste it as the password (any user name)." with title "JStream is running"
	open location uiURL
end run

-- Clicking the Dock icon opens the UI again, with the token on the clipboard.
on reopen
	if uiURL is not "" then
		open location uiURL
	end if
end reopen

-- If the daemon stops on its own, say so and quit, rather than leave an app that does nothing.
on idle
	if daemonPid is not "" then
		try
			do shell script "kill -0 " & daemonPid
		on error
			set daemonPid to ""
			display alert "JStream stopped" message "Its logs are in ~/Library/Application Support/JStream/logs." as warning
			quit
		end try
	end if
	return 5
end idle

on quit
	if daemonPid is not "" then
		-- SIGTERM: the daemon saves and stops; wait up to 10 seconds for it.
		do shell script "kill " & daemonPid & " 2>/dev/null; for i in $(seq 1 100); do kill -0 " & daemonPid & " 2>/dev/null || exit 0; sleep 0.1; done; exit 0"
		set daemonPid to ""
	end if
	continue quit
end quit
