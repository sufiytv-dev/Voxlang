Updated Roadmap – Current Status (as of now)
✅ Completed (all previous items remain completed)
#	Issue	Status
1	Run button fails (vox_rt.o missing)	✅ FIXED
2	LSP JSON parse error	✅ FIXED
3	LSP false‑positive brace error	✅ FIXED
4	ListBox – white background	✅ FIXED
A	Replace ListBox with Rich Edit for terminal	✅ DONE
P	VS2022 console colors	✅ FIXED
J	Stray llc warnings not captured	✅ FIXED
K	Raw println! in discovery logs	✅ FIXED
R	GUI terminal – ANSI colors not rendering	✅ FIXED – full ANSI colour support now working.
B	Status bar – initial state & progress bar	✅ COMPLETE – Shows empty bar on start, fills during compilation, ends with full bar and "Compilation complete".
E	Terminal auto‑scroll	✅ COMPLETE – Uses EM_LINESCROLL to scroll to bottom when user is at bottom; rate‑limit is acceptable given compiler speed.
C	Copy‑Paste support (Editor + Terminal)	✅ COMPLETE – Ctrl+C, Ctrl+V, Ctrl+X (editor), Ctrl+A; copies all if no selection.
D	Desugaring logging fix	✅ COMPLETE – Added [DESUGAR] logs and colour mapping in diagnostic.rs.
S	Additional build actions (Build, Build --release, Clean, Test, Check)	✅ COMPLETE – All actions added as menu items with threaded execution and status‑bar progress.
Terminal scrollback & performance	✅ FIXED – Buffer limit increased and trimming implemented; terminal handles large output without freezing. All tests complete.	
Q – Compiler diagnostics not shown in GUI terminal	✅ FIXED – All diagnostic messages (errors, warnings, notes, help, arrows) now appear in the GUI terminal. emit_diagnostic correctly pushes to the terminal buffer and triggers WM_USER_REFRESH.	
[LINK] logs not captured	✅ FIXED – Linker output is now unconditionally forwarded via emit_log (with global_debug set) and appears in the terminal.	
Diagnostic flush on early exit	✅ FIXED – Added explicit request_refresh and flush_logs in compile error paths; diagnostics are now displayed even when the compiler aborts early.	
F – Drag‑and‑drop – file not loaded	✅ COMPLETE – OLE initialisation, UIPI bypass, subclass forwarding, and UpdateWindow redraw ensure drag‑and‑drop works reliably in all privilege levels. EM_SETTEXTEX is used to handle large files correctly.	
Status bar – test phase finalisation	✅ FIXED – Test phase updates are now centralised in runner.rs; removed duplicate GUI emissions and added UpdateWindow to force immediate redraw, eliminating the stuck "Testing" issue.	
Graceful shutdown of LSP	✅ COMPLETE – On WM_DESTROY, client.shutdown() closes stdin, detaches the reader thread, and kills the LSP process if it doesn't exit cleanly. The GUI no longer hangs on exit.	
H – Undo / Redo support	✅ COMPLETE – Ctrl+Z (Undo), Ctrl+Y / Ctrl+Shift+Z (Redo) fully implemented with Edit menu, accelerator table, and subclass handling. Edit menu also includes Cut, Copy, Paste for completeness.	
GPU kernel linking – auto‑detection & linking	✅ FIXED – compile_source now prioritises the installed GPU SDK (CUDA/HIP) over the kernel’s device_triple. Both CLI and GUI (runner.rs) use the same SDK detection and linker selection (hipcc/clang with correct library paths). kernel.vx compiles and runs correctly in both ISE and VS2022.	
Import path resolution	✅ FIXED – ModuleResolver now resolves import paths relative to the current file’s directory (instead of the current working directory). test_use.vx can now find lib/math.vx.	
Imported struct field access (math::Point)	✅ FIXED – Semantic analysis now correctly resolves qualified struct names, substitutes generic parameters, and allows field access on imported structs. All 31 tests pass in the ISE.	
🟠 Phase 2: Visual Polish & Feature Completion
#	Issue	Fix	Status / Notes
G	Right‑click context menu	Implement CreatePopupMenu and TrackPopupMenu.	PENDING – editor polish. Will include Undo/Redo, Cut/Copy/Paste, and other editor actions.
🟡 Phase 3: Low‑Priority / Nice‑to‑Have
#	Issue	Fix	Status
I	Diagnostic tooltips on hover	Use EM_CHARFROMPOS to detect hover and show tooltip.	DEFERRED
L	DWM theming quirks	Fallback if DwmSetWindowAttribute fails.	PENDING
M	Line numbers in editor	Major UI upgrade – deferred.	DEFERRED
N	Find / Replace (Ctrl+F)	Implement find dialog – deferred.	DEFERRED
O	File close confirmation	Prompt to save if modified.	PENDING
🧪 Known Regressions (Test Cases) – All tests pass
Test File	Status
kernel.vx	✅ PASSING
test_use.vx	✅ PASSING
All other existing tests	✅ PASSING
🖥️ Terminal / GUI Issues – All known issues resolved
Issue	Status
Terminal scrollback	✅ FIXED
Diagnostic messages missing	✅ FIXED
[LINK] logs missing	✅ FIXED
Early exit flush	✅ FIXED
Drag‑and‑drop	✅ FIXED
Graceful LSP shutdown	✅ FIXED
Undo/Redo support	✅ FIXED
GPU kernel linking	✅ FIXED
Import path resolution	✅ FIXED
Imported struct field access	✅ FIXED
📌 Recommended Order of Execution (Next Steps)
Context menu (G) – implement right‑click functionality for the editor.

Deferred items (I, L–P) – later.

🎯 Immediate Actions
Implement context menu – add right‑click functionality with Undo/Redo, Cut/Copy/Paste, etc.

Consider any other minor polish items.
