use super::*;

// ── Tool loop tests ──────────────────────────────────────────────────

#[test]
fn same_tool_call_five_times_triggers_loop() {
    let mut detector = LoopDetector::new();
    for i in 0..4 {
        assert!(
            !detector.record_tool_call("read_file", r#"{"path":"foo.rs"}"#),
            "should not trigger after {} calls",
            i + 1
        );
    }
    assert!(
        detector.record_tool_call("read_file", r#"{"path":"foo.rs"}"#),
        "should trigger after 5 identical calls"
    );
}

#[test]
fn same_tool_call_four_times_no_detection() {
    let mut detector = LoopDetector::new();
    for _ in 0..4 {
        assert!(!detector.record_tool_call("read_file", r#"{"path":"foo.rs"}"#));
    }
}

#[test]
fn different_tool_calls_no_detection() {
    let mut detector = LoopDetector::new();
    for i in 0..10 {
        let name = format!("tool_{i}");
        assert!(!detector.record_tool_call(&name, "{}"));
    }
}

#[test]
fn same_tool_name_different_args_no_detection() {
    let mut detector = LoopDetector::new();
    for i in 0..10 {
        let args = format!(r#"{{"path":"file_{i}.rs"}}"#);
        assert!(!detector.record_tool_call("read_file", &args));
    }
}

#[test]
fn interleaved_tools_break_run() {
    let mut detector = LoopDetector::new();
    for _ in 0..3 {
        assert!(!detector.record_tool_call("read_file", r#"{"path":"a"}"#));
    }
    // Different tool breaks the run.
    assert!(!detector.record_tool_call("edit_file", r#"{"path":"a"}"#));
    // Restart the same call — only 1 so far.
    for _ in 0..4 {
        assert!(!detector.record_tool_call("read_file", r#"{"path":"a"}"#));
    }
    // Now the 5th consecutive identical call.
    assert!(detector.record_tool_call("read_file", r#"{"path":"a"}"#));
}

// ── Content loop tests ───────────────────────────────────────────────

#[test]
fn content_repetition_ten_times_triggers_loop() {
    let mut detector = LoopDetector::new();
    let content = "I'll now read the file and make the change.";
    for i in 0..9 {
        assert!(
            !detector.record_content(content),
            "should not trigger after {} repetitions",
            i + 1
        );
    }
    assert!(
        detector.record_content(content),
        "should trigger after 10 identical content messages"
    );
}

#[test]
fn content_below_threshold_no_detection() {
    let mut detector = LoopDetector::new();
    for _ in 0..9 {
        assert!(!detector.record_content("same message"));
    }
}

#[test]
fn different_content_no_detection() {
    let mut detector = LoopDetector::new();
    for i in 0..20 {
        assert!(!detector.record_content(&format!("message {i}")));
    }
}

// ── Reset test ───────────────────────────────────────────────────────

#[test]
fn reset_clears_state() {
    let mut detector = LoopDetector::new();
    for _ in 0..4 {
        detector.record_tool_call("read_file", r#"{"path":"a"}"#);
    }
    for _ in 0..9 {
        detector.record_content("same");
    }
    detector.reset();
    // After reset, counters start fresh — must not detect.
    assert!(!detector.record_tool_call("read_file", r#"{"path":"a"}"#));
    assert!(!detector.record_content("same"));
}

// ── Boundary: exactly at threshold ──────────────────────────────────

#[test]
fn tool_loop_exactly_at_threshold() {
    let mut detector = LoopDetector::new();
    for _ in 0..5 {
        detector.record_tool_call("bash", "ls");
    }
    // Already detected on the 5th; the 6th should still detect.
    assert!(detector.record_tool_call("bash", "ls"));
}

#[test]
fn max_history_does_not_panic() {
    let mut detector = LoopDetector::new();
    // Push more than MAX_HISTORY entries — should never panic.
    for i in 0..200 {
        detector.record_tool_call("tool", &format!("{i}"));
        detector.record_content(&format!("content {i}"));
    }
}
