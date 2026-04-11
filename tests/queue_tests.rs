use smriti::tools::queue;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn queue_input(instruction: &str, project: &str) -> queue::QueueInput {
    queue::QueueInput {
        instruction: instruction.to_string(),
        source_project: project.to_string(),
    }
}

fn count_lines(path: &std::path::Path) -> usize {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count()
}

// ═══════════════════════════════════════════════════════════════════════════
// QUEUE WRITE
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn queue_writes_entry_to_jsonl() {
    let dir = tempfile::tempdir().unwrap();
    let queue_dir = dir.path().to_path_buf();

    queue::execute(&queue_dir, 1000, queue_input("Remember this meeting", "atlas"))
        .await
        .expect("queue failed");

    let pending = queue_dir.join("pending.jsonl");
    assert!(pending.exists(), "pending.jsonl should be created");
    assert_eq!(count_lines(&pending), 1);
}

#[tokio::test]
async fn queue_entry_is_valid_json() {
    let dir = tempfile::tempdir().unwrap();
    let queue_dir = dir.path().to_path_buf();

    queue::execute(
        &queue_dir,
        1000,
        queue_input("Store the architecture decision: use rmcp", "knowledge-base"),
    )
    .await
    .unwrap();

    let pending = queue_dir.join("pending.jsonl");
    let content = std::fs::read_to_string(&pending).unwrap();

    for line in content.lines().filter(|l| !l.trim().is_empty()) {
        let parsed: serde_json::Value =
            serde_json::from_str(line).expect("each line should be valid JSON");

        assert!(parsed.get("instruction").is_some(), "must have 'instruction'");
        assert!(parsed.get("source_project").is_some(), "must have 'source_project'");
        assert!(parsed.get("queued_at").is_some(), "must have 'queued_at'");

        assert_eq!(
            parsed["instruction"].as_str().unwrap(),
            "Store the architecture decision: use rmcp"
        );
        assert_eq!(parsed["source_project"].as_str().unwrap(), "knowledge-base");
    }
}

#[tokio::test]
async fn queue_accumulates_multiple_entries() {
    let dir = tempfile::tempdir().unwrap();
    let queue_dir = dir.path().to_path_buf();

    for i in 0..5 {
        queue::execute(
            &queue_dir,
            1000,
            queue_input(&format!("Instruction {i}"), "test-project"),
        )
        .await
        .unwrap();
    }

    let pending = queue_dir.join("pending.jsonl");
    assert_eq!(count_lines(&pending), 5);
}

#[tokio::test]
async fn queue_creates_dir_if_missing() {
    let dir = tempfile::tempdir().unwrap();
    let queue_dir = dir.path().join("nested").join("queue");

    // Directory does not exist yet
    assert!(!queue_dir.exists());

    queue::execute(&queue_dir, 1000, queue_input("test", "proj"))
        .await
        .expect("should create dir and write");

    assert!(queue_dir.join("pending.jsonl").exists());
}

// ═══════════════════════════════════════════════════════════════════════════
// QUEUE SIZE LIMIT
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn queue_rejects_when_full() {
    let dir = tempfile::tempdir().unwrap();
    let queue_dir = dir.path().to_path_buf();
    let max_size = 3;

    for i in 0..max_size {
        queue::execute(
            &queue_dir,
            max_size,
            queue_input(&format!("Entry {i}"), "proj"),
        )
        .await
        .unwrap();
    }

    // One more should be rejected
    let result = queue::execute(
        &queue_dir,
        max_size,
        queue_input("Overflow entry", "proj"),
    )
    .await;

    assert!(result.is_err(), "should reject when queue is at max_size");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("queue full"), "error should mention 'queue full', got: {err}");
}

#[tokio::test]
async fn queue_size_limit_of_one() {
    let dir = tempfile::tempdir().unwrap();
    let queue_dir = dir.path().to_path_buf();

    queue::execute(&queue_dir, 1, queue_input("First", "proj"))
        .await
        .unwrap();

    let result = queue::execute(&queue_dir, 1, queue_input("Second", "proj")).await;
    assert!(result.is_err());
}

// ═══════════════════════════════════════════════════════════════════════════
// QUEUE RETURNS MESSAGE
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn queue_returns_success_message() {
    let dir = tempfile::tempdir().unwrap();
    let queue_dir = dir.path().to_path_buf();

    let out = queue::execute(&queue_dir, 1000, queue_input("some instruction", "proj"))
        .await
        .unwrap();

    assert!(
        !out.message.is_empty(),
        "should return a non-empty success message"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// QUEUE IDEMPOTENCY — empty queue file handled correctly
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn queue_handles_empty_file_gracefully() {
    let dir = tempfile::tempdir().unwrap();
    let queue_dir = dir.path().to_path_buf();

    // Create empty file manually
    std::fs::create_dir_all(&queue_dir).unwrap();
    std::fs::write(queue_dir.join("pending.jsonl"), "").unwrap();

    queue::execute(&queue_dir, 1000, queue_input("after empty file", "proj"))
        .await
        .expect("should handle empty file");

    assert_eq!(count_lines(&queue_dir.join("pending.jsonl")), 1);
}
