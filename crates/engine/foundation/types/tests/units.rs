use super::MiB;

#[test]
fn converts_to_bytes_without_overflow() {
    assert_eq!(MiB(0).bytes(), 0);
    assert_eq!(MiB(1).bytes(), 1_048_576);
    assert_eq!(MiB(u32::MAX).bytes(), u64::from(u32::MAX) * 1_048_576);
}

#[test]
fn orders_by_amount() {
    assert!(MiB(512) < MiB(2048));
    assert_eq!(serde_json::to_string(&MiB(2048)).unwrap(), "2048");
}
